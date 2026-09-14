//! One claimed USB session shared by every D-Bus call.
//!
//! The claim is held for the daemon's lifetime and armed once, per D-012.
//! A second client while one operation runs gets `AlreadyInUse`, never a
//! second claim, since two sessions desync the protocol.
//!
//! Erase is reachable from exactly one function, [`Worker::delete_finger`],
//! and only with a [`DeleteIntent`]. Nothing derived from the store can build
//! one, so the host file can never drive a flash erase. See D-023.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use elanmoc_proto::{Command, Enroll, EnrollAction, Response, SlotState, Status};
use elanmoc_store::Store;
use elanmoc_usb::{EndpointIn, Transport, UsbError};
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

impl OpEvent {
    /// Whether this event ends the operation for the client.
    pub fn is_terminal(&self) -> bool {
        match self {
            Self::EnrollStatus { done, .. } | Self::VerifyStatus { done, .. } => *done,
            Self::VerifyFingerSelected { .. } => false,
        }
    }
}

/// Which operation a sink reports on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Enroll,
    Verify,
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
    #[error("finger '{0}' is already enrolled, delete it first")]
    AlreadyEnrolled(String),
    #[error("enroll failed: {0}")]
    Enroll(#[from] elanmoc_proto::EnrollError),
    #[error("cancelled")]
    Cancelled,
}

/// The terminal enroll string for any worker error.
pub fn enroll_terminal(e: &WorkerError) -> &'static str {
    match e {
        WorkerError::Enroll(inner) => dbus::enroll_failure(inner),
        WorkerError::Usb(UsbError::Disconnected) => dbus::enroll::DISCONNECTED,
        WorkerError::AlreadyEnrolled(_) => dbus::enroll::DUPLICATE,
        _ => dbus::enroll::FAILED,
    }
}

/// The terminal verify string for any worker error.
pub fn verify_terminal(e: &WorkerError) -> &'static str {
    match e {
        WorkerError::Usb(UsbError::Disconnected) => dbus::verify::DISCONNECTED,
        WorkerError::NoPrints(_) | WorkerError::NotEnrolled(_) => dbus::verify::NO_MATCH,
        _ => dbus::verify::UNKNOWN_ERROR,
    }
}

/// Carries op events and guarantees exactly one terminal event.
///
/// [`Worker::enroll`] and [`Worker::verify`] take it by value and close it on
/// every return path, so no operation can end without telling the client.
pub struct OpSink {
    tx: mpsc::Sender<OpEvent>,
    kind: OpKind,
    terminal_sent: bool,
}

impl OpSink {
    /// A sink for one operation.
    pub fn new(kind: OpKind, tx: mpsc::Sender<OpEvent>) -> Self {
        Self {
            tx,
            kind,
            terminal_sent: false,
        }
    }

    async fn emit(&mut self, event: OpEvent) {
        if event.is_terminal() {
            self.terminal_sent = true;
        }
        let _ = self.tx.send(event).await;
    }

    /// Emit the terminal event for `result` unless one already went out.
    async fn close(mut self, result: &Result<(), WorkerError>) {
        if self.terminal_sent {
            return;
        }
        let text = match (self.kind, result) {
            (OpKind::Enroll, Err(e)) => enroll_terminal(e),
            (OpKind::Verify, Err(e)) => verify_terminal(e),
            (OpKind::Enroll, Ok(())) => dbus::enroll::UNKNOWN_ERROR,
            (OpKind::Verify, Ok(())) => dbus::verify::UNKNOWN_ERROR,
        };
        let event = match self.kind {
            OpKind::Enroll => OpEvent::EnrollStatus {
                result: text.to_string(),
                done: true,
            },
            OpKind::Verify => OpEvent::VerifyStatus {
                result: text.to_string(),
                done: true,
            },
        };
        self.emit(event).await;
    }
}

/// Claim and busy flags, readable without locking the worker.
///
/// A device operation holds the worker mutex for its whole run, which can be
/// minutes of waiting for a finger. Any D-Bus property getter that locked the
/// worker would block for exactly that long, and a client that read one
/// between `EnrollStart` and subscribing to `EnrollStatus` would never
/// subscribe. See D-028.
#[derive(Debug, Default)]
pub struct OpState {
    claimed: AtomicBool,
    busy: AtomicBool,
    /// Held for the length of a field copy, never across an await.
    user: std::sync::Mutex<Option<String>>,
}

impl OpState {
    /// Whether a user holds the claim.
    pub fn is_claimed(&self) -> bool {
        self.claimed.load(Ordering::Acquire)
    }

    /// Whether an enroll or verify is running.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    /// The claiming user, readable without locking the worker.
    pub fn claimed_user(&self) -> Option<String> {
        match self.user.lock() {
            Ok(held) => held.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn set_claimed_user(&self, user: Option<String>) {
        match self.user.lock() {
            Ok(mut held) => *held = user,
            Err(poisoned) => *poisoned.into_inner() = user,
        }
    }

    /// Take the busy flag for one operation. Never touches the worker.
    pub fn try_begin(self: &Arc<Self>) -> Result<OpToken, WorkerError> {
        if !self.is_claimed() {
            return Err(WorkerError::Unclaimed);
        }
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| WorkerError::Busy)?;
        Ok(OpToken(self.clone()))
    }
}

/// Proof that an operation holds the busy flag. Clears it on drop.
///
/// Only [`OpState::try_begin`] makes one, so the flag is taken exactly once
/// per operation and released even on an early return or a panic. Taking it
/// twice was what stopped every spawned enroll and verify from reaching the
/// device.
#[derive(Debug)]
pub struct OpToken(Arc<OpState>);

impl Drop for OpToken {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

/// Proof that a person asked for one named finger to be erased.
///
/// The only key to [`Worker::delete_finger`], which is the only function that
/// sends an erase. It names the user and finger a human chose, so nothing
/// derived from the store, a reconcile pass or a slot scan can build one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteIntent {
    user: String,
    finger: String,
}

impl DeleteIntent {
    /// Build one from an explicit delete request made by `user`.
    pub fn from_user_request(user: &str, finger: &str) -> Self {
        Self {
            user: user.to_string(),
            finger: finger.to_string(),
        }
    }

    /// The user who asked.
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The finger they named.
    pub fn finger(&self) -> &str {
        &self.finger
    }
}

/// Holds the USB claim, the arming and the claimed user.
///
/// `armed` tracks whether this handle answered `enrolled_num` since it was
/// opened or last aborted. Every touch-wait runs only while it holds.
pub struct Worker<T: Transport> {
    usb: Option<T>,
    claimed: Option<String>,
    armed: bool,
    store_path: PathBuf,
    state: Arc<OpState>,
}

impl<T: Transport> Worker<T> {
    /// Idle, nothing claimed, store at the default path.
    pub fn new(store_path: PathBuf) -> Self {
        Self {
            usb: None,
            claimed: None,
            armed: false,
            store_path,
            state: Arc::new(OpState::default()),
        }
    }

    /// The flags the D-Bus layer reads without locking this worker.
    pub fn state(&self) -> Arc<OpState> {
        self.state.clone()
    }

    fn usb(&self) -> Result<&T, WorkerError> {
        self.usb.as_ref().ok_or(WorkerError::Unclaimed)
    }

    fn claim_user(&self) -> Result<&str, WorkerError> {
        self.claimed.as_deref().ok_or(WorkerError::Unclaimed)
    }

    /// Open the sensor, arm the session, remember the user.
    ///
    /// Re-claim by the same user re-arms. A different user gets `Busy`.
    pub async fn claim(&mut self, user: String) -> Result<(), WorkerError> {
        if self.state.is_busy() {
            return Err(WorkerError::Busy);
        }
        if let Some(other) = self.claimed.as_ref() {
            if *other != user {
                return Err(WorkerError::Busy);
            }
        }
        if self.usb.is_none() {
            let usb = T::open().await?;
            self.usb = Some(usb);
        }
        self.send_arm().await?;
        self.armed = true;
        self.state.set_claimed_user(Some(user.clone()));
        self.claimed = Some(user);
        self.state.claimed.store(true, Ordering::Release);
        Ok(())
    }

    /// Send `enrolled_num` on the held handle. Read only.
    async fn send_arm(&self) -> Result<u8, WorkerError> {
        let usb = self.usb()?;
        let cancel = CancellationToken::new();
        let cmd = Command::EnrolledNum;
        let raw = usb
            .cmd(
                &cmd.encode(),
                EndpointIn::Status,
                cmd.expected_len(),
                cmd.timeout(),
                &cancel,
            )
            .await?;
        match Response::parse(&cmd, &raw)? {
            Response::EnrolledNum { count } => Ok(count),
            _ => Err(WorkerError::Enroll(
                elanmoc_proto::EnrollError::UnexpectedReply,
            )),
        }
    }

    /// End the chip session without dropping the handle. Best effort.
    async fn abort_session(&self) {
        if let Ok(usb) = self.usb() {
            let cancel = CancellationToken::new();
            let cmd = Command::Abort;
            let _ = usb
                .cmd(
                    &cmd.encode(),
                    EndpointIn::Status,
                    cmd.expected_len(),
                    cmd.timeout(),
                    &cancel,
                )
                .await;
        }
    }

    /// Release the claim, ending the chip session first.
    ///
    /// The USB handle stays open for the next claim, disarmed.
    pub async fn release(&mut self) {
        self.abort_session().await;
        self.armed = false;
        self.claimed = None;
        self.state.set_claimed_user(None);
        self.state.claimed.store(false, Ordering::Release);
    }

    /// Drop the held handle for suspend. Keeps the claim and the store.
    pub async fn suspend(&mut self) {
        self.abort_session().await;
        self.usb = None;
        self.armed = false;
    }

    /// Mark resume. Drops any stale handle without touching USB.
    ///
    /// Reopen happens lazily in `claim()`, the single open-plus-arm path.
    pub fn resume(&mut self) {
        self.usb = None;
        self.armed = false;
    }

    /// Fingers the store tracks for `user`.
    pub fn list(&self, user: &str) -> Result<Vec<String>, WorkerError> {
        let store = Store::open(&self.store_path)?;
        match store.prints().get(user) {
            Some(fingers) if !fingers.is_empty() => Ok(fingers.keys().cloned().collect()),
            _ => Err(WorkerError::NoPrints(user.to_string())),
        }
    }

    /// Erase one slot, then drop its store entry. The only erase path.
    ///
    /// Takes a [`DeleteIntent`], so it cannot be reached from a store
    /// comparison. The store says which slot the named finger lives in; it
    /// never decides that an erase should happen.
    pub async fn delete_finger(&mut self, intent: &DeleteIntent) -> Result<(), WorkerError> {
        let (user, finger) = (intent.user(), intent.finger());
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
        tracing::warn!("erasing slot {slot} for {user}/{finger} on an explicit request");
        let raw = self
            .usb()?
            .cmd(
                &cmd.encode(),
                EndpointIn::Status,
                cmd.expected_len(),
                cmd.timeout(),
                &cancel,
            )
            .await?;
        let Response::Delete { status, .. } = Response::parse(&cmd, &raw)? else {
            return Err(WorkerError::Enroll(
                elanmoc_proto::EnrollError::UnexpectedReply,
            ));
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
    ///
    /// Re-arms first when the flag is clear, aborts and disarms on error.
    /// Happy paths send no extra bytes. `sink` always emits a terminal event.
    pub async fn verify(
        &mut self,
        _op: &OpToken,
        finger: String,
        mut sink: OpSink,
        cancel: CancellationToken,
    ) -> Result<(), WorkerError> {
        let result = self.verify_inner(&finger, &mut sink, &cancel).await;
        if result.is_err() {
            self.abort_session().await;
            self.armed = false;
        }
        sink.close(&result).await;
        result
    }

    async fn verify_inner(
        &mut self,
        finger: &str,
        sink: &mut OpSink,
        cancel: &CancellationToken,
    ) -> Result<(), WorkerError> {
        if !self.armed {
            self.send_arm().await?;
            self.armed = true;
        }
        let user = self.claim_user()?.to_string();
        if finger != "any" && !elanmoc_store::is_valid_finger(finger) {
            return Err(WorkerError::BadFinger(finger.to_string()));
        }
        // `verify` (40 ff 03) carries no finger id: the chip matches against
        // every template and returns whichever hit. So a request for one
        // named finger has to be checked against the id that comes back.
        let wanted_slot = {
            let store = Store::open(&self.store_path)?;
            let has = store
                .prints()
                .get(&user)
                .map(|f| !f.is_empty())
                .unwrap_or(false);
            if !has {
                return Err(WorkerError::NoPrints(user));
            }
            if finger == "any" {
                None
            } else {
                match store.prints().get(&user).and_then(|f| f.get(finger)) {
                    Some(slot) => Some(*slot),
                    None => return Err(WorkerError::NotEnrolled(finger.to_string())),
                }
            }
        };
        loop {
            let raw = self
                .round(
                    &Command::Verify.encode(),
                    EndpointIn::TouchWait,
                    2,
                    Command::Verify.timeout(),
                    cancel,
                )
                .await?;
            let Response::Verify { status, .. } = Response::parse(&Command::Verify, &raw)? else {
                sink.emit(OpEvent::VerifyStatus {
                    result: dbus::verify::UNKNOWN_ERROR.to_string(),
                    done: true,
                })
                .await;
                return Ok(());
            };
            let done = !matches!(
                elanmoc_proto::VerifyOutcome::classify(status),
                Ok(elanmoc_proto::VerifyOutcome::Retry(_))
            );
            let mut result = dbus::verify_status(status).unwrap_or(dbus::verify::UNKNOWN_ERROR);
            if let Ok(elanmoc_proto::VerifyOutcome::Match(id)) =
                elanmoc_proto::VerifyOutcome::classify(status)
            {
                if wanted_slot.is_some_and(|want| want != id) {
                    tracing::warn!(
                        "asked for {finger}, the chip matched slot {id}, reporting no match"
                    );
                    result = dbus::verify::NO_MATCH;
                } else {
                    let name = self.finger_for_slot(&user, id);
                    sink.emit(OpEvent::VerifyFingerSelected { finger: name })
                        .await;
                }
            }
            sink.emit(OpEvent::VerifyStatus {
                result: result.to_string(),
                done,
            })
            .await;
            if done {
                return Ok(());
            }
        }
    }

    /// Run one enroll to commit, emitting per-sample progress.
    ///
    /// Re-arms first when the flag is clear, aborts and disarms on error.
    /// Happy paths send no extra bytes. `sink` always emits a terminal event.
    pub async fn enroll(
        &mut self,
        _op: &OpToken,
        finger: String,
        mut sink: OpSink,
        cancel: CancellationToken,
    ) -> Result<(), WorkerError> {
        let result = self.enroll_inner(&finger, &mut sink, &cancel).await;
        if result.is_err() {
            self.abort_session().await;
            self.armed = false;
        }
        sink.close(&result).await;
        result
    }

    async fn enroll_inner(
        &mut self,
        finger: &str,
        sink: &mut OpSink,
        cancel: &CancellationToken,
    ) -> Result<(), WorkerError> {
        let user = self.claim_user()?.to_string();
        if !elanmoc_store::is_valid_finger(finger) {
            return Err(WorkerError::BadFinger(finger.to_string()));
        }
        {
            let store = Store::open(&self.store_path)?;
            if store
                .prints()
                .get(&user)
                .is_some_and(|f| f.contains_key(finger))
            {
                return Err(WorkerError::AlreadyEnrolled(finger.to_string()));
            }
        }
        let slot = self.free_slot(cancel).await?;
        let mut sm = Enroll::new(slot);
        let EnrollAction::Send(mut out) = sm.start() else {
            return Err(WorkerError::Enroll(
                elanmoc_proto::EnrollError::UnexpectedReply,
            ));
        };
        loop {
            let Some((ep, len, timeout)) = sm.pending_read() else {
                return Err(WorkerError::Enroll(
                    elanmoc_proto::EnrollError::UnexpectedReply,
                ));
            };
            let raw = self.round(&out, endpoint(ep), len, timeout, cancel).await?;
            match sm.step(&raw) {
                EnrollAction::EmitProgress { done, total } => {
                    sink.emit(OpEvent::EnrollStatus {
                        result: dbus::enroll::STAGE_PASSED.to_string(),
                        done: false,
                    })
                    .await;
                    tracing::info!("sample {done} of {total} accepted");
                    out = self.take_send(&mut sm)?;
                }
                EnrollAction::EmitRetry(r) => {
                    sink.emit(OpEvent::EnrollStatus {
                        result: dbus::enroll_retry(r).to_string(),
                        done: false,
                    })
                    .await;
                    out = self.take_send(&mut sm)?;
                }
                EnrollAction::Send(next) => out = next,
                EnrollAction::Complete(id) => {
                    return self.record(sink, finger, &user, id).await;
                }
                EnrollAction::Fail(e) => return Err(WorkerError::Enroll(e)),
            }
        }
    }

    fn take_send(&self, sm: &mut Enroll) -> Result<Vec<u8>, WorkerError> {
        match sm.take_send() {
            Some(EnrollAction::Send(next)) => Ok(next),
            _ => Err(WorkerError::Enroll(
                elanmoc_proto::EnrollError::UnexpectedReply,
            )),
        }
    }

    async fn record(
        &self,
        sink: &mut OpSink,
        finger: &str,
        user: &str,
        id: u8,
    ) -> Result<(), WorkerError> {
        let mut store = Store::open(&self.store_path)?;
        store.insert(user, finger, id)?;
        store.save()?;
        sink.emit(OpEvent::EnrollStatus {
            result: dbus::enroll::COMPLETED.to_string(),
            done: true,
        })
        .await;
        Ok(())
    }

    /// Lowest slot enrollment may write into.
    ///
    /// The device's own count sets the floor: `enrolled_num` is the only
    /// reading that proves a slot is in use, since 0c90 answers `finger_info`
    /// with `40 ff` for occupied and empty alike. The store can only rule more
    /// slots out. A 70 byte record vetoes a candidate as well.
    async fn free_slot(&mut self, cancel: &CancellationToken) -> Result<u8, WorkerError> {
        let count = self.send_arm().await?;
        self.armed = true;
        let tracked = {
            let store = Store::open(&self.store_path)?;
            elanmoc_store::tracked_slots(store.prints())
        };
        for id in elanmoc_store::writable_slots(count, &tracked) {
            let cmd = Command::FingerInfo(id);
            let raw = self
                .round(
                    &cmd.encode(),
                    EndpointIn::Status,
                    cmd.expected_len(),
                    cmd.timeout(),
                    cancel,
                )
                .await?;
            match Response::parse(&cmd, &raw) {
                Ok(Response::FingerInfo {
                    state: SlotState::Enrolled { .. },
                    ..
                }) => {}
                _ => return Ok(id),
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
            self.armed = false;
        }
        result
    }
}

/// The transport endpoint for a reply endpoint the protocol crate names.
fn endpoint(ep: elanmoc_proto::ReplyEndpoint) -> EndpointIn {
    match ep {
        elanmoc_proto::ReplyEndpoint::Image => EndpointIn::Image,
        elanmoc_proto::ReplyEndpoint::Status => EndpointIn::Status,
        elanmoc_proto::ReplyEndpoint::TouchWait => EndpointIn::TouchWait,
    }
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

#[cfg(test)]
mod tests;
