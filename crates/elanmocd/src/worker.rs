//! One claimed USB session shared by every D-Bus call.
//!
//! The claim is held for the daemon's lifetime and armed once, per D-012.
//! A second client while one operation runs gets `AlreadyInUse`, never a
//! second claim, since two sessions desync the protocol.

use std::path::PathBuf;
use std::time::Duration;

use elanmoc_proto::{
    Command, Enroll, EnrollAction, Response, SlotState, Status, TOTAL_ENROLL_ATTEMPTS,
};
use elanmoc_store::{MAX_SLOT, Store};
use elanmoc_usb::{Device, EndpointIn, UsbError};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::status as dbus;

/// Events the D-Bus layer turns into signals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpEvent {
    EnrollStatus {
        result: String,
        done: bool,
    },
    VerifyStatus {
        result: String,
        done: bool,
    },
    VerifyFingerSelected {
        finger: String,
    },
}

/// Everything daemon operations can fail with.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("device error: {0}")]
    Usb(#[from] UsbError),
    #[error("protocol error: {0}")]
    Proto(#[from] elanmoc_proto::ProtoError),
    #[error("store error: {0}")]
    Store(#[from] elanmoc_store::StoreError),
    #[error("device already in use")]
    Busy,
    #[error("not claimed")]
    Unclaimed,
    #[error("unknown finger '{0}'")]
    BadFinger(String),
    #[error("no prints enrolled for '{0}'")]
    NoPrints(String),
    #[error("finger '{0}' is not enrolled")]
    NotEnrolled(String),
    #[error("enroll failed: {0}")]
    Enroll(#[from] elanmoc_proto::EnrollError),
    #[error("cancelled")]
    Cancelled,
}

/// Holds the USB claim, the arming and the claimed user.
pub struct Worker {
    usb: Option<Device>,
    claimed: Option<String>,
    busy: bool,
    store_path: PathBuf,
}

impl Worker {
    /// Idle, nothing claimed, store at the default path.
    pub fn new(store_path: PathBuf) -> Self {
        Self {
            usb: None,
            claimed: None,
            busy: false,
            store_path,
        }
    }

    /// Whether a user currently holds the claim.
    pub fn is_claimed(&self) -> bool {
        self.claimed.is_some()
    }

    /// Whether an enroll or verify is running.
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// The claiming user, if any.
    pub fn claimed_user(&self) -> Option<String> {
        self.claimed.clone()
    }

    /// Mark an operation running. The caller must call [`Worker::finish`].
    pub fn try_begin(&mut self) -> Result<(), WorkerError> {
        if self.usb.is_none() {
            return Err(WorkerError::Unclaimed);
        }
        if self.busy {
            return Err(WorkerError::Busy);
        }
        self.busy = true;
        Ok(())
    }

    /// Mark the operation done.
    pub fn finish(&mut self) {
        self.busy = false;
    }

    fn usb(&self) -> Result<&Device, WorkerError> {
        self.usb.as_ref().ok_or(WorkerError::Unclaimed)
    }

    fn claim_user(&self) -> Result<&str, WorkerError> {
        self.claimed.as_deref().ok_or(WorkerError::Unclaimed)
    }

    /// Open the sensor, arm the session, remember the user.
    pub async fn claim(&mut self, user: String) -> Result<(), WorkerError> {
        if self.busy {
            return Err(WorkerError::Busy);
        }
        if self.usb.is_none() {
            let usb = Device::open().await?;
            self.usb = Some(usb);
        }
        let cancel = CancellationToken::new();
        let out = Command::EnrolledNum.encode();
        let armed = match self.usb.as_ref() {
            Some(usb) => {
                usb.cmd(
                    &out,
                    EndpointIn::Status,
                    Command::EnrolledNum.expected_len(),
                    Command::EnrolledNum.timeout(),
                    &cancel,
                )
                .await
            }
            None => return Err(WorkerError::Unclaimed),
        };
        armed?;
        self.claimed = Some(user);
        Ok(())
    }

    /// Release the claim. The USB handle stays open for the next claim.
    pub fn release(&mut self) {
        self.claimed = None;
        self.busy = false;
    }

    /// Fingers the store tracks for `user`.
    pub fn list(&self, user: &str) -> Result<Vec<String>, WorkerError> {
        let store = Store::open(&self.store_path)?;
        match store.prints().get(user) {
            Some(fingers) if !fingers.is_empty() => {
                Ok(fingers.keys().cloned().collect())
            }
            _ => Err(WorkerError::NoPrints(user.to_string())),
        }
    }

    /// Delete one finger: device slot first, store entry after.
    pub async fn delete_finger(&mut self, user: &str, finger: &str) -> Result<(), WorkerError> {
        if !elanmoc_store::is_valid_finger(finger) {
            return Err(WorkerError::BadFinger(finger.to_string()));
        }
        let slot = {
            let store = Store::open(&self.store_path)?;
            match store.prints().get(user).and_then(|f| f.get(finger)) {
                Some(s) => *s,
                None => return Err(WorkerError::NotEnrolled(finger.to_string())),
            }
        };
        let cancel = CancellationToken::new();
        let cmd = Command::Delete(slot);
        let raw = self.usb()?.cmd(
            &cmd.encode(),
            EndpointIn::Status,
            cmd.expected_len(),
            cmd.timeout(),
            &cancel,
        )
        .await?;
        let Response::Delete { status, .. } = Response::parse(&cmd, &raw)? else {
            return Err(WorkerError::Enroll(elanmoc_proto::EnrollError::UnexpectedReply));
        };
        if !matches!(status, Status::Ok(0)) {
            return Err(WorkerError::Enroll(elanmoc_proto::EnrollError::Device {
                command: "delete",
                code: status_code(status),
            }));
        }
        let mut store = Store::open(&self.store_path)?;
        store.remove(user, finger);
        store.save()?;
        Ok(())
    }

    /// Run one verify to a terminal answer, emitting signals on the way.
    pub async fn verify(
        &mut self,
        finger: String,
        tx: mpsc::Sender<OpEvent>,
        cancel: CancellationToken,
    ) -> Result<(), WorkerError> {
        self.try_begin()?;
        let result = self.verify_loop(&finger, &tx, &cancel).await;
        self.finish();
        result
    }

    async fn verify_loop(
        &mut self,
        finger: &str,
        tx: &mpsc::Sender<OpEvent>,
        cancel: &CancellationToken,
    ) -> Result<(), WorkerError> {
        let user = self.claim_user()?.to_string();
        if finger != "any" && !elanmoc_store::is_valid_finger(finger) {
            return Err(WorkerError::BadFinger(finger.to_string()));
        }
        {
            let store = Store::open(&self.store_path)?;
            let has = store
                .prints()
                .get(&user)
                .map(|f| !f.is_empty())
                .unwrap_or(false);
            if !has {
                return Err(WorkerError::NoPrints(user));
            }
            if finger != "any"
                && store
                    .prints()
                    .get(&user)
                    .and_then(|f| f.get(finger))
                    .is_none()
            {
                return Err(WorkerError::NotEnrolled(finger.to_string()));
            }
        }
        loop {
            let raw = match self
                .round(&Command::Verify.encode(), EndpointIn::TouchWait, 2, Command::Verify.timeout(), cancel)
                .await
            {
                Ok(bytes) => bytes,
                Err(e) => {
                    self.verify_signal_error(tx, &e).await;
                    return Err(e);
                }
            };
            let Response::Verify { status, .. } = Response::parse(&Command::Verify, &raw)? else {
                self.emit(tx, OpEvent::VerifyStatus {
                    result: dbus::verify::UNKNOWN_ERROR.to_string(),
                    done: true,
                })
                .await;
                return Ok(());
            };
            if let Ok(elanmoc_proto::VerifyOutcome::Match(id)) =
                elanmoc_proto::VerifyOutcome::classify(status)
            {
                let name = self.finger_for_slot(&user, id);
                self.emit(tx, OpEvent::VerifyFingerSelected { finger: name })
                    .await;
            }
            let done = !matches!(
                elanmoc_proto::VerifyOutcome::classify(status),
                Ok(elanmoc_proto::VerifyOutcome::Retry(_))
            );
            let result = dbus::verify_status(status).unwrap_or(dbus::verify::UNKNOWN_ERROR);
            self.emit(tx, OpEvent::VerifyStatus {
                result: result.to_string(),
                done,
            })
            .await;
            if done {
                return Ok(());
            }
        }
    }

    /// `verify-disconnected` when the device is gone, then the error itself.
    async fn verify_signal_error(&self, tx: &mpsc::Sender<OpEvent>, e: &WorkerError) {
        if matches!(e, WorkerError::Usb(UsbError::Disconnected)) {
            self.emit(tx, OpEvent::VerifyStatus {
                result: dbus::verify::DISCONNECTED.to_string(),
                done: true,
            })
            .await;
        }
    }

    /// Run one enroll to commit, emitting per-sample progress.
    pub async fn enroll(
        &mut self,
        finger: String,
        tx: mpsc::Sender<OpEvent>,
        cancel: CancellationToken,
    ) -> Result<(), WorkerError> {
        self.try_begin()?;
        let result = self.enroll_loop(&finger, &tx, &cancel).await;
        self.finish();
        result
    }

    async fn enroll_loop(
        &mut self,
        finger: &str,
        tx: &mpsc::Sender<OpEvent>,
        cancel: &CancellationToken,
    ) -> Result<(), WorkerError> {
        let user = self.claim_user()?.to_string();
        if !elanmoc_store::is_valid_finger(finger) {
            return Err(WorkerError::BadFinger(finger.to_string()));
        }
        let slot = self.free_slot(cancel).await?;
        let mut sm = Enroll::new(slot);
        let EnrollAction::Send(mut out) = sm.start() else {
            return self.enroll_failed(tx, elanmoc_proto::EnrollError::UnexpectedReply).await;
        };
        loop {
            let raw = match self
                .round(&out, EndpointIn::TouchWait, 2, enroll_sample_timeout(), cancel)
                .await
            {
                Ok(bytes) => bytes,
                Err(e) => {
                    if matches!(e, WorkerError::Usb(UsbError::Disconnected)) {
                        self.emit(tx, OpEvent::EnrollStatus {
                            result: dbus::enroll::DISCONNECTED.to_string(),
                            done: true,
                        })
                        .await;
                    }
                    return Err(e);
                }
            };
            match sm.step(&raw) {
                EnrollAction::EmitProgress { done, total } => {
                    self.emit(tx, OpEvent::EnrollStatus {
                        result: dbus::enroll::STAGE_PASSED.to_string(),
                        done: false,
                    })
                    .await;
                    tracing::info!("sample {done} of {total} accepted");
                    let Some(EnrollAction::Send(next)) = sm.take_send() else {
                        return self.enroll_failed(tx, elanmoc_proto::EnrollError::UnexpectedReply).await;
                    };
                    out = next;
                }
                EnrollAction::EmitRetry(r) => {
                    self.emit(tx, OpEvent::EnrollStatus {
                        result: dbus::enroll_retry(r).to_string(),
                        done: false,
                    })
                    .await;
                    let Some(EnrollAction::Send(next)) = sm.take_send() else {
                        return self.enroll_failed(tx, elanmoc_proto::EnrollError::UnexpectedReply).await;
                    };
                    out = next;
                }
                EnrollAction::Send(next) => {
                    // First send after the last sample is the collision
                    // check (3 bytes), whose reply queues the commit.
                    if next.len() == 3 && next.starts_with(&[0x40, 0xff, 0x10]) {
                        let raw = self
                            .round(&next, EndpointIn::Status, 3, Command::CheckCollision.timeout(), cancel)
                            .await?;
                        match sm.step(&raw) {
                            EnrollAction::Send(commit) => {
                                let done = self.commit_round(&commit, tx, &mut sm, finger, &user, cancel).await;
                                return done;
                            }
                            EnrollAction::Fail(e) => {
                                return self.enroll_failed(tx, e).await;
                            }
                            _ => {
                                return self.enroll_failed(tx, elanmoc_proto::EnrollError::UnexpectedReply).await;
                            }
                        }
                    }
                    let done = self.commit_round(&next, tx, &mut sm, finger, &user, cancel).await;
                    return done;
                }
                EnrollAction::Complete(id) => {
                    let mut store = Store::open(&self.store_path)?;
                    store.insert(&user, finger, id)?;
                    store.save()?;
                    self.emit(tx, OpEvent::EnrollStatus {
                        result: dbus::enroll::COMPLETED.to_string(),
                        done: true,
                    })
                    .await;
                    return Ok(());
                }
                EnrollAction::Fail(e) => {
                    return self.enroll_failed(tx, e).await;
                }
            }
        }
    }

    async fn commit_round(
        &mut self,
        commit: &[u8],
        tx: &mpsc::Sender<OpEvent>,
        sm: &mut Enroll,
        finger: &str,
        user: &str,
        cancel: &CancellationToken,
    ) -> Result<(), WorkerError> {
        let raw = match self
            .round(commit, EndpointIn::Status, 2, Command::commit(0).timeout(), cancel)
            .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                if matches!(e, WorkerError::Usb(UsbError::Disconnected)) {
                    self.emit(tx, OpEvent::EnrollStatus {
                        result: dbus::enroll::DISCONNECTED.to_string(),
                        done: true,
                    })
                    .await;
                }
                return Err(e);
            }
        };
        match sm.step(&raw) {
            EnrollAction::Complete(id) => {
                let mut store = Store::open(&self.store_path)?;
                store.insert(user, finger, id)?;
                store.save()?;
                self.emit(tx, OpEvent::EnrollStatus {
                    result: dbus::enroll::COMPLETED.to_string(),
                    done: true,
                })
                .await;
                Ok(())
            }
            EnrollAction::Fail(e) => self.enroll_failed(tx, e).await,
            _ => self.enroll_failed(tx, elanmoc_proto::EnrollError::UnexpectedReply).await,
        }
    }

    async fn enroll_failed(
        &self,
        tx: &mpsc::Sender<OpEvent>,
        e: elanmoc_proto::EnrollError,
    ) -> Result<(), WorkerError> {
        self.emit(tx, OpEvent::EnrollStatus {
            result: dbus::enroll_failure(&e).to_string(),
            done: true,
        })
        .await;
        Err(WorkerError::Enroll(e))
    }

    /// First slot without a 70 byte occupied record.
    async fn free_slot(&mut self, cancel: &CancellationToken) -> Result<u8, WorkerError> {
        for id in 0..=MAX_SLOT {
            let raw = self
                .round(
                    &Command::FingerInfo(id).encode(),
                    EndpointIn::Status,
                    Command::FingerInfo(id).expected_len(),
                    Command::FingerInfo(id).timeout(),
                    cancel,
                )
                .await?;
            let parsed = match Response::parse(&Command::FingerInfo(id), &raw) {
                Ok(Response::FingerInfo { state, .. }) => state,
                Ok(_) => continue,
                Err(_) => continue,
            };
            if !matches!(parsed, SlotState::Enrolled { .. }) {
                return Ok(id);
            }
        }
        Err(WorkerError::Enroll(elanmoc_proto::EnrollError::MaxEnrolled))
    }

    fn finger_for_slot(&self, user: &str, slot: u8) -> String {
        match Store::open(&self.store_path) {
            Ok(store) => store
                .prints()
                .get(user)
                .and_then(|f| f.iter().find(|(_, s)| **s == slot).map(|(n, _)| n.clone()))
                .unwrap_or_else(|| "any".to_string()),
            Err(_) => "any".to_string(),
        }
    }

    async fn round(
        &mut self,
        out: &[u8],
        ep: EndpointIn,
        len: usize,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, WorkerError> {
        let usb = self.usb()?;
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(WorkerError::Cancelled),
            out = usb.cmd(out, ep, len, timeout, cancel) => {
                match out {
                    Ok(bytes) => Ok(bytes),
                    Err(UsbError::ShortRead { data, .. }) => Ok(data),
                    Err(UsbError::Cancelled) => Err(WorkerError::Cancelled),
                    Err(e) => Err(WorkerError::Usb(e)),
                }
            }
        };
        if matches!(result, Err(WorkerError::Usb(UsbError::Disconnected))) {
            self.usb = None;
        }
        result
    }

    async fn emit(&self, tx: &mpsc::Sender<OpEvent>, event: OpEvent) {
        let _ = tx.send(event).await;
    }
}

fn enroll_sample_timeout() -> Duration {
    Command::Enroll {
        finger_id: 0,
        total_attempts: TOTAL_ENROLL_ATTEMPTS,
        attempts_done: 0,
    }
    .timeout()
}

fn status_code(status: Status) -> u8 {
    match status {
        Status::Ok(b) => b,
        Status::Retry(_) => 0xfe,
        Status::MaxEnrolled => 0xdd,
        Status::NotEnrolled => 0xfd,
        Status::Unknown(b) => b,
    }
}
