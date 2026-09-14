//! elanmocd: fprintd-compatible daemon for the ELAN 04f3:0c90.
//!
//! Owns `net.reactivated.Fprint` on the system bus and implements the exact
//! interface in `docs/dbus-device.xml`. One USB claim for the process,
//! armed once, behind a mutex: a second client gets `AlreadyInUse`.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use zbus::object_server::SignalContext;
use zbus::{interface, Connection};

mod access;
mod status;
mod worker;

use worker::{DeleteIntent, OpEvent, OpKind, OpSink, OpState, OpToken, Worker, WorkerError};

/// The daemon always drives the real sensor. Tests drive a fake transport.
type UsbWorker = Worker<elanmoc_usb::Device>;

const BUS_NAME: &str = "net.reactivated.Fprint";
const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
const DEVICE_PATH: &str = "/net/reactivated/Fprint/Device/0";
const DEVICE_NAME: &str = "ELAN 04f3:0c90";

/// fprintd error names, live from `docs/dbus-device.xml`.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "net.reactivated.Fprint.Error")]
enum FprintError {
    ClaimDevice(String),
    AlreadyInUse,
    Internal(String),
    PermissionDenied(String),
    NoEnrolledPrints,
    NoActionInProgress,
    InvalidFingername,
    PrintsNotDeleted(String),
}

impl From<WorkerError> for FprintError {
    fn from(e: WorkerError) -> Self {
        match e {
            WorkerError::Busy => Self::AlreadyInUse,
            WorkerError::Unclaimed => Self::ClaimDevice("device is not claimed".to_string()),
            WorkerError::BadFinger(_) => Self::InvalidFingername,
            WorkerError::NoPrints(_) | WorkerError::NotEnrolled(_) => Self::NoEnrolledPrints,
            WorkerError::AlreadyEnrolled(f) => Self::Internal(format!("{f} is already enrolled")),
            WorkerError::Cancelled => Self::NoActionInProgress,
            WorkerError::Usb(e) => Self::Internal(e.to_string()),
            WorkerError::Proto(e) => Self::Internal(e.to_string()),
            WorkerError::Store(e) => Self::PrintsNotDeleted(e.to_string()),
            WorkerError::Enroll(e) => Self::Internal(e.to_string()),
        }
    }
}

struct Manager;

#[interface(name = "net.reactivated.Fprint.Manager")]
impl Manager {
    async fn get_devices(&self) -> Vec<zbus::zvariant::ObjectPath<'_>> {
        vec![zbus::zvariant::ObjectPath::from_str_unchecked(DEVICE_PATH)]
    }

    async fn get_default_device(&self) -> zbus::zvariant::ObjectPath<'_> {
        zbus::zvariant::ObjectPath::from_str_unchecked(DEVICE_PATH)
    }
}

struct Device {
    worker: Arc<Mutex<UsbWorker>>,
    state: Arc<OpState>,
    op_cancel: Arc<Mutex<Option<CancellationToken>>>,
    conn: Connection,
}

impl Device {
    /// uid of whoever sent the message, from the bus, not from the message.
    async fn caller_uid(&self, hdr: &zbus::message::Header<'_>) -> Result<u32, FprintError> {
        let Some(sender) = hdr.sender() else {
            return Err(FprintError::PermissionDenied(
                "message carries no sender".to_string(),
            ));
        };
        let dbus = zbus::fdo::DBusProxy::new(&self.conn)
            .await
            .map_err(|e| FprintError::PermissionDenied(format!("cannot reach the bus: {e}")))?;
        dbus.get_connection_unix_user(sender.to_owned().into())
            .await
            .map_err(|e| FprintError::PermissionDenied(format!("cannot read the caller uid: {e}")))
    }

    /// Refuse callers who are neither root nor the owner of the prints.
    async fn guard(
        &self,
        hdr: &zbus::message::Header<'_>,
        target: &str,
    ) -> Result<(), FprintError> {
        let uid = self.caller_uid(hdr).await?;
        if access::may_act(uid, access::lookup_uid(target)) {
            return Ok(());
        }
        tracing::warn!("refused uid {uid} acting on {target}'s fingerprints");
        Err(FprintError::PermissionDenied(format!(
            "uid {uid} may not act on {target}'s fingerprints"
        )))
    }

    /// The claimed user, or a claim error. Never locks the worker.
    fn claimed_target(&self) -> Result<String, FprintError> {
        self.state
            .claimed_user()
            .ok_or_else(|| FprintError::ClaimDevice("device is not claimed".to_string()))
    }
    /// Take the busy flag once, here, without locking the worker.
    ///
    /// A running operation holds the worker for as long as it waits for a
    /// finger, so locking here would make a second call hang instead of
    /// answering `AlreadyInUse`.
    async fn start_op(&self) -> Result<(OpToken, CancellationToken), FprintError> {
        let op = self.state.try_begin()?;
        let cancel = CancellationToken::new();
        *self.op_cancel.lock().await = Some(cancel.clone());
        Ok((op, cancel))
    }

    async fn stop_running(&self) {
        if let Some(cancel) = self.op_cancel.lock().await.take() {
            cancel.cancel();
        }
    }
}

#[interface(name = "net.reactivated.Fprint.Device")]
impl Device {
    async fn claim(
        &mut self,
        username: String,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
    ) -> Result<(), FprintError> {
        self.guard(&hdr, &username).await?;
        self.worker.lock().await.claim(username).await?;
        Ok(())
    }

    async fn release(&mut self) -> Result<(), FprintError> {
        self.stop_running().await;
        self.worker.lock().await.release().await;
        Ok(())
    }

    async fn list_enrolled_fingers(&mut self, username: String) -> Result<Vec<String>, FprintError> {
        Ok(self.worker.lock().await.list(&username)?)
    }

    async fn delete_enrolled_fingers(
        &mut self,
        username: String,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
    ) -> Result<(), FprintError> {
        self.guard(&hdr, &username).await?;
        let fingers = self.worker.lock().await.list(&username)?;
        for finger in fingers {
            let intent = DeleteIntent::from_user_request(&username, &finger);
            self.worker.lock().await.delete_finger(&intent).await?;
        }
        Ok(())
    }

    async fn delete_enrolled_fingers2(&mut self) -> Result<(), FprintError> {
        Err(FprintError::PermissionDenied("username is required".to_string()))
    }

    async fn delete_enrolled_finger(
        &mut self,
        finger_name: String,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
    ) -> Result<(), FprintError> {
        let user = self.claimed_target()?;
        self.guard(&hdr, &user).await?;
        let intent = DeleteIntent::from_user_request(&user, &finger_name);
        self.worker.lock().await.delete_finger(&intent).await?;
        Ok(())
    }

    async fn verify_start(
        &mut self,
        finger_name: String,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> Result<(), FprintError> {
        self.guard(&hdr, &self.claimed_target()?).await?;
        let (op, cancel) = self.start_op().await?;
        let (tx, mut rx) = mpsc::channel::<OpEvent>(16);
        let worker = self.worker.clone();
        let ender = self.op_cancel.clone();
        tokio::spawn(async move {
            {
                let mut w = worker.lock().await;
                let sink = OpSink::new(OpKind::Verify, tx);
                if let Err(e) = w.verify(&op, finger_name, sink, cancel).await {
                    tracing::warn!("verify ended: {e}");
                }
                drop(op);
            }
            *ender.lock().await = None;
        });
        let ctxt = ctxt.to_owned();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match &event {
                    OpEvent::VerifyStatus { result, done } => {
                        let _ = Device::verify_status(&ctxt, result, *done).await;
                    }
                    OpEvent::VerifyFingerSelected { finger } => {
                        let _ = Device::verify_finger_selected(&ctxt, finger).await;
                    }
                    _ => {}
                }
            }
        });
        Ok(())
    }

    async fn verify_stop(&mut self) -> Result<(), FprintError> {
        self.stop_running().await;
        Ok(())
    }

    async fn enroll_start(
        &mut self,
        finger_name: String,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> Result<(), FprintError> {
        self.guard(&hdr, &self.claimed_target()?).await?;
        let (op, cancel) = self.start_op().await?;
        let (tx, mut rx) = mpsc::channel::<OpEvent>(32);
        let worker = self.worker.clone();
        let ender = self.op_cancel.clone();
        tokio::spawn(async move {
            {
                let mut w = worker.lock().await;
                let sink = OpSink::new(OpKind::Enroll, tx);
                if let Err(e) = w.enroll(&op, finger_name, sink, cancel).await {
                    tracing::warn!("enroll ended: {e}");
                }
                drop(op);
            }
            *ender.lock().await = None;
        });
        let ctxt = ctxt.to_owned();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let OpEvent::EnrollStatus { result, done } = &event {
                    let _ = Device::enroll_status(&ctxt, result, *done).await;
                }
            }
        });
        Ok(())
    }

    async fn enroll_stop(&mut self) -> Result<(), FprintError> {
        self.stop_running().await;
        Ok(())
    }

    #[zbus(property, name = "name")]
    async fn name(&self) -> String {
        DEVICE_NAME.to_string()
    }

    #[zbus(property, name = "scan-type")]
    async fn scan_type(&self) -> String {
        "press".to_string()
    }

    #[zbus(property, name = "num-enroll-stages")]
    async fn num_enroll_stages(&self) -> i32 {
        if self.state.is_claimed() {
            elanmoc_proto::TOTAL_ENROLL_ATTEMPTS as i32
        } else {
            -1
        }
    }

    #[zbus(property, name = "finger-present")]
    async fn finger_present(&self) -> bool {
        false
    }

    #[zbus(property, name = "finger-needed")]
    async fn finger_needed(&self) -> bool {
        self.state.is_busy()
    }

    #[zbus(signal)]
    async fn enroll_status(
        ctxt: &SignalContext<'_>,
        result: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn verify_status(
        ctxt: &SignalContext<'_>,
        result: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn verify_finger_selected(
        ctxt: &SignalContext<'_>,
        finger_name: &str,
    ) -> zbus::Result<()>;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let store_path = std::env::var("ELANMOC_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(elanmoc_store::DEFAULT_PATH));
    let worker = UsbWorker::new(store_path);
    let state = worker.state();
    let worker = Arc::new(Mutex::new(worker));
    let op_cancel: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));

    let session = std::env::var("ELANMOC_BUS").is_ok_and(|v| v == "session");
    let conn = if session {
        Connection::session().await?
    } else {
        Connection::system().await?
    };
    conn.object_server().at(MANAGER_PATH, Manager).await?;
    conn.object_server()
        .at(
            DEVICE_PATH,
            Device {
                worker: worker.clone(),
                state: state.clone(),
                op_cancel: op_cancel.clone(),
                conn: conn.clone(),
            },
        )
        .await?;
    conn.request_name(BUS_NAME).await?;
    tracing::info!("claimed {BUS_NAME}, serving {DEVICE_PATH}");

    let sleep_worker = worker.clone();
    let sleep_cancel = op_cancel.clone();
    let sleep_conn = conn.clone();
    tokio::spawn(async move {
        sleep_listener(sleep_conn, sleep_worker, sleep_cancel).await;
    });

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = term.recv() => {},
    }
    tracing::info!("shutting down, aborting chip session");
    if let Some(cancel) = op_cancel.lock().await.take() {
        cancel.cancel();
    }
    worker.lock().await.release().await;
    Ok(())
}

/// Watch logind sleep signals. Suspend drops the handle, resume reopens lazily.
async fn sleep_listener(
    conn: Connection,
    worker: Arc<Mutex<UsbWorker>>,
    op_cancel: Arc<Mutex<Option<CancellationToken>>>,
) {
    use futures_util::StreamExt as _;
    let rule: zbus::MatchRule<'_> = match zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.login1.Manager")
        .and_then(|b| b.member("PrepareForSleep"))
        .and_then(|b| b.path("/org/freedesktop/login1"))
        .map(|b| b.build())
    {
        Ok(rule) => rule,
        Err(e) => {
            tracing::error!("bad sleep match rule: {e}");
            return;
        }
    };
    let mut stream = match zbus::MessageStream::for_match_rule(rule, &conn, None).await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::error!("sleep subscribe failed: {e}");
            return;
        }
    };
    while let Some(Ok(msg)) = stream.next().await {
        let Ok(suspending) = msg.body().deserialize::<bool>() else {
            continue;
        };
        if suspending {
            if let Some(cancel) = op_cancel.lock().await.take() {
                cancel.cancel();
            }
            worker.lock().await.suspend().await;
            tracing::info!("suspended, handle dropped");
        } else {
            worker.lock().await.resume();
            tracing::info!("resumed, reopen on next claim");
        }
    }
}
