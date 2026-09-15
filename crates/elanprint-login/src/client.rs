//! D-Bus client for the fingerprint daemon.
//!
//! Talks to whatever owns `net.reactivated.Fprint`, per
//! `docs/dbus-device.xml`. Signals feed the shared
//! [`elanprint_algo::VerifySession`], so grant logic is not UI code.

use elanprint_algo::{Action, Signal, VerifySession};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollEvent {
    Stage {
        done: u8,
        total: u8,
    },
    Retry(String),
    Completed,
    Failed(String),
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEvent {
    Prompt(String),
    Retry(String),
    Granted,
    Denied(String),
    Locked,
    Failed(String),
    Finger(String),
    Finished,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("D-Bus error: {0}")]
    Dbus(#[from] zbus::Error),
    #[error("bad property: {0}")]
    Variant(#[from] zbus::zvariant::Error),
    #[error("no fingerprint reader on the bus")]
    NoReader,
}

impl ClientError {
    /// Raw D-Bus names never show.
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
            Self::Variant(_) => "reader error, see log".to_string(),
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

fn is_named(error: &zbus::Error, name: &str) -> bool {
    matches!(error, zbus::Error::MethodError(error_name, _, _)
        if error_name.as_str().ends_with(name))
}

pub struct FingerprintClient {
    conn: Connection,
    device: OwnedObjectPath,
}

impl FingerprintClient {
    /// Connect and take the first advertised device. `ELANPRINT_BUS=session`
    /// runs against a dev daemon instead of the system bus.
    pub async fn connect() -> Result<Self, ClientError> {
        let session = std::env::var("ELANPRINT_BUS").is_ok_and(|v| v == "session");
        let conn = if session {
            Connection::session().await?
        } else {
            Connection::system().await?
        };
        let manager = Proxy::new(&conn, BUS_NAME, MANAGER_PATH, MANAGER_IFACE).await?;
        let devices: Vec<OwnedObjectPath> = manager.call("GetDevices", &()).await?;
        let device = devices.into_iter().next().ok_or(ClientError::NoReader)?;
        Ok(Self { conn, device })
    }

    /// Empty when none are enrolled, not an error.
    pub async fn list(&self, user: &str) -> Result<Vec<String>, ClientError> {
        let proxy = self.device_proxy().await?;
        let result: Result<Vec<String>, zbus::Error> =
            proxy.call("ListEnrolledFingers", &(user)).await;
        match result {
            Ok(fingers) => Ok(fingers),
            Err(e) => ClientError::Dbus(e).empty_when_no_prints(),
        }
    }

    /// Total enroll stages. -1 means unclaimed, per docs/dbus-device.xml.
    pub async fn stages(&self) -> Result<i32, ClientError> {
        let props = Proxy::new(
            &self.conn,
            BUS_NAME,
            &self.device,
            "org.freedesktop.DBus.Properties",
        )
        .await?;
        let value: zbus::zvariant::OwnedValue = props
            .call("Get", &(DEVICE_IFACE, "num-enroll-stages"))
            .await?;
        Ok(i32::try_from(&value)?)
    }

    /// Claim, DeleteEnrolledFinger, Release.
    pub async fn delete_finger(&self, user: &str, finger: &str) -> Result<(), ClientError> {
        let proxy = self.device_proxy().await?;
        proxy.call::<_, _, ()>("Claim", &(user)).await?;
        let result: Result<(), zbus::Error> =
            proxy.call("DeleteEnrolledFinger", &(finger)).await;
        let _ = proxy.call::<_, _, ()>("Release", &()).await;
        result.map_err(ClientError::Dbus)
    }

    /// Run one enroll to a terminal answer, emitting UI events on the way.
    pub async fn enroll(
        &self,
        user: &str,
        finger: &str,
        tx: mpsc::Sender<EnrollEvent>,
        cancel: CancellationToken,
    ) -> Result<(), ClientError> {
        let proxy = self.device_proxy().await?;
        proxy.call::<_, _, ()>("Claim", &(user)).await?;
        // subscribe before the op starts, a reply beating the stream is lost
        let total = match self.stages().await {
            Ok(n) if n > 0 => n as u8,
            _ => 0,
        };
        let mut statuses = proxy.receive_signal("EnrollStatus").await?;
        let started = proxy.call::<_, _, ()>("EnrollStart", &(finger)).await;
        if let Err(e) = started {
            let _ = proxy.call::<_, _, ()>("Release", &()).await;
            return Err(ClientError::Dbus(e));
        }
        let mut done: u8 = 0;
        let outcome = self
            .drive_enroll(&mut statuses, total, &mut done, &tx, &cancel)
            .await;
        let _ = proxy.call::<_, _, ()>("EnrollStop", &()).await;
        let _ = proxy.call::<_, _, ()>("Release", &()).await;
        let _ = tx.send(EnrollEvent::Finished).await;
        outcome
    }

    async fn drive_enroll(
        &self,
        statuses: &mut zbus::proxy::SignalStream<'_>,
        total: u8,
        done: &mut u8,
        tx: &mpsc::Sender<EnrollEvent>,
        cancel: &CancellationToken,
    ) -> Result<(), ClientError> {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    return Ok(());
                }
                next = statuses.next() => {
                    let Some(msg) = next else { return Ok(()); };
                    let (result, finished): (String, bool) = msg.body().deserialize()?;
                    match (result.as_str(), finished) {
                        ("enroll-stage-passed", false) => {
                            *done = done.saturating_add(1);
                            let _ = tx.send(EnrollEvent::Stage { done: *done, total }).await;
                        }
                        ("enroll-completed", true) => {
                            let _ = tx.send(EnrollEvent::Completed).await;
                            return Ok(());
                        }
                        ("enroll-failed" | "enroll-data-full" | "enroll-duplicate"
                        | "enroll-unknown-error" | "enroll-disconnected", true) => {
                            let text = crate::copy::enroll_failure(&result);
                            let _ = tx.send(EnrollEvent::Failed(text)).await;
                            return Ok(());
                        }
                        ("enroll-retry-scan", false) => {
                            let _ = tx.send(EnrollEvent::Retry("try again".to_string())).await;
                        }
                        ("enroll-swipe-too-short", false) => {
                            let _ = tx
                                .send(EnrollEvent::Retry(
                                    "hold your finger still and retry".to_string(),
                                ))
                                .await;
                        }
                        ("enroll-finger-not-centered", false) => {
                            let _ = tx
                                .send(EnrollEvent::Retry(
                                    "centre your finger and retry".to_string(),
                                ))
                                .await;
                        }
                        ("enroll-remove-and-retry", false) => {
                            let _ = tx
                                .send(EnrollEvent::Retry("lift your finger and retry".to_string()))
                                .await;
                        }
                        _ => {
                            let _ = tx.send(EnrollEvent::Failed(
                                format!("unexpected enroll status {result}"),
                            )).await;
                            return Ok(());
                        }
                    }
                }
            }
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
        // subscribe first, a terminal reply can arrive in under a millisecond
        let mut statuses = proxy.receive_signal("VerifyStatus").await?;
        let mut selected = proxy.receive_signal("VerifyFingerSelected").await?;
        let started = proxy.call::<_, _, ()>("VerifyStart", &(finger)).await;
        if let Err(e) = started {
            let _ = proxy.call::<_, _, ()>("Release", &()).await;
            return Err(ClientError::Dbus(e));
        }
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
                if text == "touch the sensor" {
                    self.emit(tx, AuthEvent::Prompt(text)).await;
                } else {
                    self.emit(tx, AuthEvent::Retry(text)).await;
                }
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
