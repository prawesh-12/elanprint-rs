//! D-Bus client for the fingerprint daemon.
//!
//! Talks to whatever owns `net.reactivated.Fprint` using the interface in
//! `docs/dbus-device.xml`. Every daemon signal feeds the shared
//! [`elanmoc_algo::VerifySession`], so the grant logic is the tested engine,
//! not UI code.

use elanmoc_algo::{Action, Signal, VerifySession};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zbus::proxy::Proxy;
use zbus::zvariant::OwnedObjectPath;
use zbus::Connection;

const BUS_NAME: &str = "net.reactivated.Fprint";
const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
const MANAGER_IFACE: &str = "net.reactivated.Fprint.Manager";
const DEVICE_IFACE: &str = "net.reactivated.Fprint.Device";

/// UI-facing authentication events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEvent {
    Prompt(String),
    Granted,
    Denied(String),
    Locked,
    Failed(String),
    Finger(String),
    Finished,
}

/// Everything daemon calls can fail with.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("D-Bus error: {0}")]
    Dbus(#[from] zbus::Error),
    #[error("no fingerprint reader on the bus")]
    NoReader,
}

impl ClientError {
    /// Short human text for the status line. Raw D-Bus names never show.
    pub fn friendly(&self) -> String {
        match self {
            Self::NoReader => "no fingerprint reader on the bus".to_string(),
            Self::Dbus(e) if is_named(e, "NoEnrolledPrints") => {
                "no fingerprints enrolled yet".to_string()
            }
            Self::Dbus(e) if is_named(e, "PermissionDenied") => {
                "not allowed, check permissions".to_string()
            }
            Self::Dbus(_) => "reader error, see log".to_string(),
        }
    }

    /// Listing prints for a user with none is normal, not a failure.
    fn empty_when_no_prints(self) -> Result<Vec<String>, Self> {
        match &self {
            Self::Dbus(e) if is_named(e, "NoEnrolledPrints") => Ok(Vec::new()),
            _ => Err(self),
        }
    }
}

/// Whether a D-Bus error carries the given fprintd error name.
fn is_named(error: &zbus::Error, name: &str) -> bool {
    matches!(error, zbus::Error::MethodError(error_name, _, _)
        if error_name.as_str().ends_with(name))
}

/// Connected handle to the reader.
pub struct FingerprintClient {
    conn: Connection,
    device: OwnedObjectPath,
}

impl FingerprintClient {
    /// Connect on the system bus and take the first advertised device.
    pub async fn connect() -> Result<Self, ClientError> {
        let conn = Connection::system().await?;
        let manager = Proxy::new(&conn, BUS_NAME, MANAGER_PATH, MANAGER_IFACE).await?;
        let devices: Vec<OwnedObjectPath> = manager.call("GetDevices", &()).await?;
        let device = devices.into_iter().next().ok_or(ClientError::NoReader)?;
        Ok(Self { conn, device })
    }

    /// Fingers the daemon tracks for `user`. Empty when none are enrolled.
    pub async fn list(&self, user: &str) -> Result<Vec<String>, ClientError> {
        let proxy = self.device_proxy().await?;
        let result: Result<Vec<String>, zbus::Error> =
            proxy.call("ListEnrolledFingers", &(user)).await;
        match result {
            Ok(fingers) => Ok(fingers),
            Err(e) => ClientError::Dbus(e).empty_when_no_prints(),
        }
    }

    /// Run one verify to a terminal answer, emitting UI events on the way.
    pub async fn verify(
        &self,
        user: &str,
        finger: &str,
        tx: mpsc::Sender<AuthEvent>,
        cancel: CancellationToken,
    ) -> Result<(), ClientError> {
        let mut session = VerifySession::new(user);
        self.emit(&tx, AuthEvent::Prompt(Self::text(&session.begin()))).await;

        let proxy = self.device_proxy().await?;
        proxy.call::<_, _, ()>("Claim", &(user)).await?;
        let started = proxy.call::<_, _, ()>("VerifyStart", &(finger)).await;
        if let Err(e) = started {
            let _ = proxy.call::<_, _, ()>("Release", &()).await;
            return Err(ClientError::Dbus(e));
        }

        let mut statuses = proxy.receive_signal("VerifyStatus").await?;
        let mut selected = proxy.receive_signal("VerifyFingerSelected").await?;
        let outcome = self
            .drive(&mut session, &mut statuses, &mut selected, &tx, &cancel)
            .await;

        let _ = proxy.call::<_, _, ()>("VerifyStop", &()).await;
        let _ = proxy.call::<_, _, ()>("Release", &()).await;
        let _ = tx.send(AuthEvent::Finished).await;
        outcome
    }

    async fn drive(
        &self,
        session: &mut VerifySession,
        statuses: &mut zbus::proxy::SignalStream<'_>,
        selected: &mut zbus::proxy::SignalStream<'_>,
        tx: &mpsc::Sender<AuthEvent>,
        cancel: &CancellationToken,
    ) -> Result<(), ClientError> {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    return Ok(());
                }
                next = statuses.next() => {
                    let Some(msg) = next else { return Ok(()) };
                    let (result, done): (String, bool) = msg.body().deserialize()?;
                    if self.apply(session, &Signal::Status { result, done }, tx).await? {
                        return Ok(());
                    }
                }
                next = selected.next() => {
                    let Some(msg) = next else { continue };
                    let finger: String = msg.body().deserialize()?;
                    let _ = session.step(&Signal::FingerSelected { finger: finger.clone() });
                    self.emit(tx, AuthEvent::Finger(finger)).await;
                }
            }
        }
    }

    /// Feed one signal. Returns true on a terminal answer.
    async fn apply(
        &self,
        session: &mut VerifySession,
        signal: &Signal,
        tx: &mpsc::Sender<AuthEvent>,
    ) -> Result<bool, ClientError> {
        match session.step(signal) {
            Ok(Action::Prompt(text)) => {
                self.emit(tx, AuthEvent::Prompt(text)).await;
                Ok(false)
            }
            Ok(Action::Granted(_)) => {
                self.emit(tx, AuthEvent::Granted).await;
                Ok(true)
            }
            Ok(Action::Deny(text)) => {
                self.emit(tx, AuthEvent::Denied(text)).await;
                Ok(true)
            }
            Ok(Action::Locked) => {
                self.emit(tx, AuthEvent::Locked).await;
                Ok(true)
            }
            Ok(Action::Failed(text)) => {
                self.emit(tx, AuthEvent::Failed(text)).await;
                Ok(true)
            }
            Ok(Action::Idle) => Ok(true),
            Err(e) => {
                self.emit(tx, AuthEvent::Failed(e.to_string())).await;
                Ok(true)
            }
        }
    }

    async fn device_proxy(&self) -> Result<Proxy<'_>, ClientError> {
        Ok(Proxy::new(&self.conn, BUS_NAME, &self.device, DEVICE_IFACE).await?)
    }

    async fn emit(&self, tx: &mpsc::Sender<AuthEvent>, event: AuthEvent) {
        let _ = tx.send(event).await;
    }

    fn text(action: &Action) -> String {
        match action {
            Action::Prompt(text) | Action::Deny(text) | Action::Failed(text) => text.clone(),
            Action::Granted(user) => format!("granted for {user}"),
            Action::Locked => "no tries left".to_string(),
            Action::Idle => "idle".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live bus, no daemon device: fprintd advertises zero devices, so
    /// connect must report NoReader instead of hanging or granting.
    /// Ignored by default: it needs the system bus and fails once a real
    /// daemon serves a device, which is the correct outcome then.
    #[tokio::test]
    #[ignore]
    async fn connect_reports_no_reader_without_daemon() {
        match FingerprintClient::connect().await {
            Err(ClientError::NoReader) => {}
            Err(e) => panic!("expected NoReader, got {e}"),
            Ok(_) => panic!("expected NoReader, got a reader"),
        }
    }
}
