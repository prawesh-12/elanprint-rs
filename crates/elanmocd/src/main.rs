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

mod status;
mod worker;

use worker::{OpEvent, Worker, WorkerError};

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
    worker: Arc<Mutex<Worker>>,
    op_cancel: Arc<Mutex<Option<CancellationToken>>>,
}

impl Device {
    async fn start_op(&self) -> Result<CancellationToken, FprintError> {
        let mut worker = self.worker.lock().await;
        worker.try_begin()?;
        let cancel = CancellationToken::new();
        *self.op_cancel.lock().await = Some(cancel.clone());
        Ok(cancel)
    }

    async fn stop_running(&self) {
        if let Some(cancel) = self.op_cancel.lock().await.take() {
            cancel.cancel();
        }
    }
}

#[interface(name = "net.reactivated.Fprint.Device")]
impl Device {
    async fn claim(&mut self, username: String) -> Result<(), FprintError> {
        self.worker.lock().await.claim(username).await?;
        Ok(())
    }

    async fn release(&mut self) -> Result<(), FprintError> {
        self.stop_running().await;
        self.worker.lock().await.release();
        Ok(())
    }

    async fn list_enrolled_fingers(&mut self, username: String) -> Result<Vec<String>, FprintError> {
        Ok(self.worker.lock().await.list(&username)?)
    }

    async fn delete_enrolled_fingers(&mut self, username: String) -> Result<(), FprintError> {
        let fingers = self.worker.lock().await.list(&username)?;
        for finger in fingers {
            self.worker.lock().await.delete_finger(&username, &finger).await?;
        }
        Ok(())
    }

    async fn delete_enrolled_fingers2(&mut self) -> Result<(), FprintError> {
        Err(FprintError::PermissionDenied("username is required".to_string()))
    }

    async fn delete_enrolled_finger(&mut self, finger_name: String) -> Result<(), FprintError> {
        let user = self
            .worker
            .lock()
            .await
            .claimed_user()
            .ok_or(FprintError::ClaimDevice("device is not claimed".to_string()))?;
        self.worker.lock().await.delete_finger(&user, &finger_name).await?;
        Ok(())
    }

    async fn verify_start(
        &mut self,
        finger_name: String,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> Result<(), FprintError> {
        let cancel = self.start_op().await?;
        let (tx, mut rx) = mpsc::channel::<OpEvent>(16);
        let worker = self.worker.clone();
        let ender = self.op_cancel.clone();
        tokio::spawn(async move {
            {
                let mut w = worker.lock().await;
                let _ = w.verify(finger_name, tx, cancel).await;
                w.finish();
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
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> Result<(), FprintError> {
        let cancel = self.start_op().await?;
        let (tx, mut rx) = mpsc::channel::<OpEvent>(32);
        let worker = self.worker.clone();
        let ender = self.op_cancel.clone();
        tokio::spawn(async move {
            {
                let mut w = worker.lock().await;
                let _ = w.enroll(finger_name, tx, cancel).await;
                w.finish();
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

    #[zbus(property)]
    async fn name(&self) -> String {
        DEVICE_NAME.to_string()
    }

    #[zbus(property)]
    async fn scan_type(&self) -> String {
        "press".to_string()
    }

    #[zbus(property)]
    async fn num_enroll_stages(&self) -> i32 {
        if self.worker.lock().await.is_claimed() {
            elanmoc_proto::TOTAL_ENROLL_ATTEMPTS as i32
        } else {
            -1
        }
    }

    #[zbus(property)]
    async fn finger_present(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn finger_needed(&self) -> bool {
        self.worker.lock().await.is_busy()
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
    let worker = Arc::new(Mutex::new(Worker::new(store_path)));

    let conn = Connection::system().await?;
    conn.object_server().at(MANAGER_PATH, Manager).await?;
    conn.object_server()
        .at(
            DEVICE_PATH,
            Device {
                worker,
                op_cancel: Arc::new(Mutex::new(None)),
            },
        )
        .await?;
    conn.request_name(BUS_NAME).await?;
    tracing::info!("claimed {BUS_NAME}, serving {DEVICE_PATH}");

    std::future::pending::<()>().await;
    Ok(())
}
