//! Regression tests for the ways slot 0 could be erased.
//!
//! Every test drives a fake transport that records the bytes the worker
//! sends, so an erase is proven present or absent from the wire rather than
//! from reading the code. No hardware, no device.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::path::Path;
use std::sync::{Arc, Mutex};

use elanmoc_store::Store;
use tokio::sync::mpsc;

use super::*;

/// Erase opcodes from `docs/protocol.md`. None may appear on the wire except
/// from an explicit delete.
const ERASE_PREFIXES: [&[u8]; 3] = [
    &[0x40, 0xff, 0x05],       // delete, per protocol.md
    &[0x40, 0xff, 0x13],       // delete_subsid, per protocol.md
    &[0x40, 0xff, 0x99],       // wipe_all, per protocol.md
];

#[derive(Default)]
struct Wire {
    sent: Vec<Vec<u8>>,
    reads: Vec<(Vec<u8>, EndpointIn, usize)>,
    replies: Vec<Result<Vec<u8>, ()>>,
}

/// Replays queued replies and records everything written.
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Wire>>);

impl Fake {
    fn new(replies: Vec<Vec<u8>>) -> Self {
        let wire = Wire {
            sent: Vec::new(),
            reads: Vec::new(),
            replies: replies.into_iter().map(Ok).collect(),
        };
        Self(Arc::new(Mutex::new(wire)))
    }

    /// Replies until the queue runs out, then a transfer fault on every read.
    fn failing_after(replies: Vec<Vec<u8>>) -> Self {
        Self::new(replies)
    }

    fn sent(&self) -> Vec<Vec<u8>> {
        match self.0.lock() {
            Ok(w) => w.sent.clone(),
            Err(e) => panic!("wire lock: {e}"),
        }
    }

    /// Which endpoint and length each command's reply was read on.
    fn reads(&self) -> Vec<(Vec<u8>, EndpointIn, usize)> {
        match self.0.lock() {
            Ok(w) => w.reads.clone(),
            Err(e) => panic!("wire lock: {e}"),
        }
    }

    fn erases(&self) -> Vec<Vec<u8>> {
        self.sent()
            .into_iter()
            .filter(|out| ERASE_PREFIXES.iter().any(|p| out.starts_with(p)))
            .collect()
    }
}

impl Transport for Fake {
    fn open() -> impl std::future::Future<Output = Result<Self, UsbError>> + Send {
        // Tests inject the handle, so the lazy reopen path is never taken.
        std::future::ready(Err(UsbError::NotFound))
    }

    fn cmd<'a>(
        &'a self,
        out: &'a [u8],
        ep: EndpointIn,
        in_len: usize,
        _timeout: Duration,
        _cancel: &'a CancellationToken,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, UsbError>> + Send + 'a {
        let mut wire = match self.0.lock() {
            Ok(w) => w,
            Err(e) => panic!("wire lock: {e}"),
        };
        wire.sent.push(out.to_vec());
        wire.reads.push((out.to_vec(), ep, in_len));
        let reply = if wire.replies.is_empty() {
            Err(UsbError::Timeout(Duration::from_secs(1)))
        } else {
            match wire.replies.remove(0) {
                Ok(bytes) => Ok(bytes),
                Err(()) => Err(UsbError::Disconnected),
            }
        };
        std::future::ready(reply)
    }
}

fn store_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("elanmocd-test-{}-{tag}.json", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn write_store(path: &PathBuf, entries: &[(&str, &str, u8)]) {
    let mut prints: elanmoc_store::Prints = BTreeMap::new();
    for (user, finger, slot) in entries {
        prints
            .entry((*user).to_string())
            .or_default()
            .insert((*finger).to_string(), *slot);
    }
    let text = match serde_json::to_string(&prints) {
        Ok(t) => t,
        Err(e) => panic!("serialise store: {e}"),
    };
    if let Err(e) = std::fs::write(path, text) {
        panic!("write store: {e}");
    }
}

fn read_store(path: &Path) -> elanmoc_store::Prints {
    match Store::open(path) {
        Ok(s) => s.prints().clone(),
        Err(e) => panic!("read store: {e}"),
    }
}

/// A claimed, armed worker over `fake`, without calling `claim`.
fn claimed(fake: Fake, path: PathBuf) -> Worker<Fake> {
    let mut w = Worker::new(path);
    w.usb = Some(fake);
    w.claimed = Some("u".to_string());
    w.armed = true;
    w.state.claimed.store(true, Ordering::Release);
    w
}

/// Take the busy flag the way the D-Bus layer does.
fn begin(w: &Worker<Fake>) -> OpToken {
    match w.state().try_begin() {
        Ok(op) => op,
        Err(e) => panic!("begin: {e}"),
    }
}

fn enrolled_num(count: u8) -> Vec<u8> {
    vec![0x40, count]
}

/// Drain a channel into a list, for asserting on what the client saw.
async fn drain(rx: &mut mpsc::Receiver<OpEvent>) -> Vec<OpEvent> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    let _ = rx;
    out
}

/// The store says slot 0 is free, the device says one finger is enrolled.
///
/// This is the shape that lost the slot 0 template: a store emptied by a
/// delete, a device that still holds a print, and `finger_info` answering
/// `40 ff` for occupied and empty alike. The enrollment must not touch slot 0.
#[tokio::test]
async fn empty_store_never_hands_out_an_occupied_slot() {
    let path = store_path("disagree");
    write_store(&path, &[]);
    let fake = Fake::new(vec![
        enrolled_num(1),           // free_slot reads the count
        vec![0x40, 0xff],          // finger_info(1), the 2 byte form
        vec![0x40, 0x00],          // first enroll sample, then the queue dries up
    ]);
    let mut w = claimed(fake.clone(), path.clone());
    let op = begin(&w);
    let (tx, mut rx) = mpsc::channel(32);
    let sink = OpSink::new(OpKind::Enroll, tx);
    let _ = w
        .enroll(
            &op,
            "left-index-finger".to_string(),
            sink,
            CancellationToken::new(),
        )
        .await;

    let sent = fake.sent();
    let samples: Vec<&Vec<u8>> = sent
        .iter()
        .filter(|out| out.starts_with(&[0x40, 0xff, 0x01]))
        .collect();
    assert!(!samples.is_empty(), "the enroll ran: {sent:?}");
    for sample in &samples {
        assert_ne!(sample[3], 0, "enrolled into slot 0 while the device held one print");
    }
    assert_eq!(samples[0][3], 1, "first free slot above the device count");
    let _ = drain(&mut rx).await;
    let _ = std::fs::remove_file(&path);
}

/// A store that disagrees with the device never causes a device delete.
///
/// Enroll, verify and list all run against a store that contradicts the
/// device in both directions. No erase opcode may reach the wire.
#[tokio::test]
async fn a_disagreeing_store_never_erases() {
    let path = store_path("no-erase");
    write_store(&path, &[("u", "right-index-finger", 3)]);
    let fake = Fake::new(vec![
        enrolled_num(0), // device says empty, store claims slot 3
        vec![0x40, 0xff],
        vec![0x40, 0x00],
    ]);
    let mut w = claimed(fake.clone(), path.clone());
    let op = begin(&w);
    let (tx, mut rx) = mpsc::channel(32);
    let _ = w
        .enroll(
            &op,
            "left-thumb".to_string(),
            OpSink::new(OpKind::Enroll, tx),
            CancellationToken::new(),
        )
        .await;
    drop(op);

    let op = begin(&w);
    let (vtx, mut vrx) = mpsc::channel(32);
    let _ = w
        .verify(
            &op,
            "right-index-finger".to_string(),
            OpSink::new(OpKind::Verify, vtx),
            CancellationToken::new(),
        )
        .await;
    drop(op);
    let _ = w.list("u");
    w.release().await;

    assert!(
        fake.erases().is_empty(),
        "an erase reached the wire: {:?}",
        fake.erases()
    );
    assert_eq!(
        read_store(&path)["u"]["right-index-finger"],
        3,
        "the store entry survived"
    );
    let _ = drain(&mut rx).await;
    let _ = drain(&mut vrx).await;
    let _ = std::fs::remove_file(&path);
}

/// A failed enrol never erases an existing slot.
#[tokio::test]
async fn a_failed_enroll_erases_nothing() {
    let path = store_path("enroll-fail");
    write_store(&path, &[]);
    let fake = Fake::failing_after(vec![
        enrolled_num(1),
        vec![0x40, 0xff],
        vec![0x40, 0xdd], // slot limit, a hard failure mid-loop
    ]);
    let mut w = claimed(fake.clone(), path.clone());
    let op = begin(&w);
    let (tx, mut rx) = mpsc::channel(32);
    let result = w
        .enroll(
            &op,
            "left-thumb".to_string(),
            OpSink::new(OpKind::Enroll, tx),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err(), "the enroll failed");
    assert!(
        fake.erases().is_empty(),
        "a failed enroll erased: {:?}",
        fake.erases()
    );
    let _ = drain(&mut rx).await;
    let _ = std::fs::remove_file(&path);
}

/// A cancelled enrol never erases an existing slot.
#[tokio::test]
async fn a_cancelled_enroll_erases_nothing() {
    let path = store_path("enroll-cancel");
    write_store(&path, &[]);
    let fake = Fake::new(vec![enrolled_num(1), vec![0x40, 0xff]]);
    let mut w = claimed(fake.clone(), path.clone());
    let op = begin(&w);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (tx, mut rx) = mpsc::channel(32);
    let result = w
        .enroll(
            &op,
            "left-thumb".to_string(),
            OpSink::new(OpKind::Enroll, tx),
            cancel,
        )
        .await;
    assert!(result.is_err(), "the enroll was cancelled");
    assert!(
        fake.erases().is_empty(),
        "a cancelled enroll erased: {:?}",
        fake.erases()
    );
    let _ = drain(&mut rx).await;
    let _ = std::fs::remove_file(&path);
}

/// Claim, release, claim again does not change what the device holds.
///
/// The cycle may send only `enrolled_num` and `abort`, and the count the
/// device reports must be the same at the end as at the start.
#[tokio::test]
async fn claim_release_claim_leaves_the_count_alone() {
    let path = store_path("cycle");
    write_store(&path, &[("u", "right-index-finger", 0)]);
    let fake = Fake::new(vec![
        enrolled_num(1),
        vec![],
        enrolled_num(1),
        vec![],
        enrolled_num(1),
        vec![],
        enrolled_num(1),
        vec![],
        enrolled_num(1),
        vec![],
        enrolled_num(1),
    ]);
    let mut w = claimed(fake.clone(), path.clone());
    for _ in 0..5 {
        if let Err(e) = w.claim("u".to_string()).await {
            panic!("claim: {e}");
        }
        w.release().await;
    }
    let count = match w.send_arm().await {
        Ok(n) => n,
        Err(e) => panic!("final count read: {e}"),
    };

    assert_eq!(count, 1, "the count is unchanged after five cycles");
    assert!(fake.erases().is_empty(), "a cycle erased: {:?}", fake.erases());
    for out in fake.sent() {
        assert!(
            out == vec![0x40, 0xff, 0x04] || out == vec![0x40, 0xff, 0x02],
            "claim/release sent more than arm and abort: {out:?}"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// Enrolling an already-enrolled finger name is refused before any device I/O.
#[tokio::test]
async fn a_duplicate_finger_name_is_refused() {
    let path = store_path("dup");
    write_store(&path, &[("u", "left-thumb", 2)]);
    let fake = Fake::new(vec![enrolled_num(1)]);
    let mut w = claimed(fake.clone(), path.clone());
    let op = begin(&w);
    let (tx, mut rx) = mpsc::channel(32);
    let result = w
        .enroll(
            &op,
            "left-thumb".to_string(),
            OpSink::new(OpKind::Enroll, tx),
            CancellationToken::new(),
        )
        .await;

    match result {
        Err(WorkerError::AlreadyEnrolled(f)) => assert_eq!(f, "left-thumb"),
        other => panic!("expected AlreadyEnrolled, got {other:?}"),
    }
    let events = drain(&mut rx).await;
    assert_eq!(
        events.last(),
        Some(&OpEvent::EnrollStatus {
            result: "enroll-duplicate".to_string(),
            done: true,
        }),
        "the client was told why"
    );
    let sends: Vec<Vec<u8>> = fake
        .sent()
        .into_iter()
        .filter(|out| out != &vec![0x40, 0xff, 0x02])
        .collect();
    assert!(
        sends.is_empty(),
        "the refusal sent more than the teardown abort: {sends:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// Taking the busy flag twice used to stop every op before it sent a byte.
#[tokio::test]
async fn the_busy_flag_is_taken_once_per_op() {
    let path = store_path("token");
    write_store(&path, &[]);
    let fake = Fake::new(vec![enrolled_num(0), vec![0x40, 0xff], vec![0x40, 0x00]]);
    let mut w = claimed(fake.clone(), path.clone());
    let op = begin(&w);
    assert!(w.state().try_begin().is_err(), "a second op is refused");
    let (tx, mut rx) = mpsc::channel(32);
    let _ = w
        .enroll(
            &op,
            "left-thumb".to_string(),
            OpSink::new(OpKind::Enroll, tx),
            CancellationToken::new(),
        )
        .await;
    assert!(
        fake.sent().iter().any(|o| o.starts_with(&[0x40, 0xff, 0x01])),
        "the enroll reached the device: {:?}",
        fake.sent()
    );
    let _ = drain(&mut rx).await;
    let _ = std::fs::remove_file(&path);
}

/// Every enroll error path emits a terminal status.
#[tokio::test]
async fn every_enroll_error_emits_a_terminal_status() {
    let cases: [(&str, Vec<Vec<u8>>, &str); 3] = [
        ("unclaimed store", vec![enrolled_num(0)], "left-thumb"),
        (
            "device says slot limit",
            vec![enrolled_num(1), vec![0x40, 0xff], vec![0x40, 0xdd]],
            "left-thumb",
        ),
        ("bad finger name", vec![], "not-a-finger"),
    ];
    for (name, replies, finger) in cases {
        let path = store_path(&format!("term-{}", name.replace(' ', "-")));
        write_store(&path, &[]);
        let fake = Fake::new(replies);
        let mut w = claimed(fake, path.clone());
        let op = begin(&w);
        let (tx, mut rx) = mpsc::channel(32);
        let result = w
            .enroll(
                &op,
                finger.to_string(),
                OpSink::new(OpKind::Enroll, tx),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "{name} failed");
        let events = drain(&mut rx).await;
        assert!(
            events.last().is_some_and(OpEvent::is_terminal),
            "{name} ended with no terminal status: {events:?}"
        );
        let _ = std::fs::remove_file(&path);
    }
}

/// Every verify error path emits a terminal status.
#[tokio::test]
async fn every_verify_error_emits_a_terminal_status() {
    type VerifyCase = (&'static str, Vec<(&'static str, &'static str, u8)>, &'static str);
    let cases: [VerifyCase; 3] = [
        ("no prints", vec![], "any"),
        (
            "finger not enrolled",
            vec![("u", "left-thumb", 1)],
            "right-thumb",
        ),
        (
            "device stops answering",
            vec![("u", "left-thumb", 1)],
            "left-thumb",
        ),
    ];
    for (name, entries, finger) in cases {
        let path = store_path(&format!("vterm-{}", name.replace(' ', "-")));
        write_store(&path, &entries);
        let fake = Fake::new(Vec::new());
        let mut w = claimed(fake, path.clone());
        let op = begin(&w);
        let (tx, mut rx) = mpsc::channel(32);
        let result = w
            .verify(
                &op,
                finger.to_string(),
                OpSink::new(OpKind::Verify, tx),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "{name} failed");
        let events = drain(&mut rx).await;
        assert!(
            events.last().is_some_and(OpEvent::is_terminal),
            "{name} ended with no terminal status: {events:?}"
        );
        let _ = std::fs::remove_file(&path);
    }
}

/// The one path that may erase does erase, and only with an intent.
#[tokio::test]
async fn an_explicit_delete_is_the_only_thing_that_erases() {
    let path = store_path("delete");
    write_store(&path, &[("u", "right-index-finger", 0)]);
    let fake = Fake::new(vec![vec![0x40, 0x00]]);
    let mut w = claimed(fake.clone(), path.clone());
    let intent = DeleteIntent::from_user_request("u", "right-index-finger");
    if let Err(e) = w.delete_finger(&intent).await {
        panic!("delete: {e}");
    }
    assert_eq!(
        fake.erases(),
        vec![vec![0x40, 0xff, 0x05, 0x00, 0x00]],
        "the delete sent exactly one erase"
    );
    assert!(read_store(&path).is_empty(), "the store entry went with it");
    let _ = std::fs::remove_file(&path);
}

/// An intent for a finger the store does not track erases nothing.
#[tokio::test]
async fn a_delete_for_an_untracked_finger_erases_nothing() {
    let path = store_path("delete-miss");
    write_store(&path, &[("u", "right-index-finger", 0)]);
    let fake = Fake::new(vec![vec![0x40, 0x00]]);
    let mut w = claimed(fake.clone(), path.clone());
    let intent = DeleteIntent::from_user_request("u", "left-thumb");
    assert!(w.delete_finger(&intent).await.is_err(), "refused");
    assert!(fake.erases().is_empty(), "nothing was erased");
    let _ = std::fs::remove_file(&path);
}

/// A full enrol drives to commit and reads every reply where it belongs.
///
/// The daemon read the collision check on `0x84` for 2 bytes, the sample's
/// endpoint, because the loop assumed every queued send was a touch wait.
/// The chip answers `40 ff 10` on `0x83` with 3 bytes, so the read waited
/// out the 30 second touch timeout and the commit was never sent.
#[tokio::test]
async fn an_enrol_reads_every_reply_on_the_right_endpoint() {
    let path = store_path("endpoints");
    write_store(&path, &[]);
    let mut replies = vec![enrolled_num(0), vec![0x40, 0xff]];
    replies.extend(std::iter::repeat_n(vec![0x40, 0x00], 8)); // eight samples
    replies.push(vec![0x40, 0x00, 0xff]); // collision check, no clash
    replies.push(vec![0x40, 0x00]); // commit
    let fake = Fake::new(replies);
    let mut w = claimed(fake.clone(), path.clone());
    let op = begin(&w);
    let (tx, mut rx) = mpsc::channel(32);
    if let Err(e) = w
        .enroll(
            &op,
            "left-index-finger".to_string(),
            OpSink::new(OpKind::Enroll, tx),
            CancellationToken::new(),
        )
        .await
    {
        panic!("enrol drove to commit: {e}");
    }

    for (out, ep, len) in fake.reads() {
        let (want_ep, want_len) = match out.as_slice() {
            [0x40, 0xff, 0x01, ..] => (EndpointIn::TouchWait, 2),
            [0x40, 0xff, 0x10] => (EndpointIn::Status, 3),
            [0x40, 0xff, 0x11, ..] => (EndpointIn::Status, 2),
            [0x40, 0xff, 0x12, ..] => (EndpointIn::Status, 70),
            _ => continue,
        };
        assert_eq!(ep, want_ep, "wrong endpoint for {out:02x?}");
        assert_eq!(len, want_len, "wrong length for {out:02x?}");
    }
    assert!(
        fake.sent().iter().any(|o| o.starts_with(&[0x40, 0xff, 0x11])),
        "commit was sent: {:?}",
        fake.sent()
    );
    assert!(fake.erases().is_empty(), "the enrol erased nothing");
    assert_eq!(read_store(&path)["u"]["left-index-finger"], 0);
    let events = drain(&mut rx).await;
    assert_eq!(
        events.last(),
        Some(&OpEvent::EnrollStatus {
            result: "enroll-completed".to_string(),
            done: true,
        })
    );
    let _ = std::fs::remove_file(&path);
}

/// Property flags stay readable while an operation holds the worker.
///
/// The D-Bus property getters used to lock the worker, which a running op
/// holds for as long as it waits for a finger. A client that read
/// `num-enroll-stages` between `EnrollStart` and subscribing to
/// `EnrollStatus` blocked there and never subscribed, so the enrol ran on the
/// chip with nothing listening. The UI sat on "touch the sensor" forever.
#[tokio::test]
async fn flags_are_readable_while_the_worker_is_held() {
    let path = store_path("flags");
    write_store(&path, &[("u", "left-index-finger", 0)]);
    let fake = Fake::new(Vec::new());
    let worker = Arc::new(tokio::sync::Mutex::new(claimed(fake, path.clone())));
    let state = worker.lock().await.state();
    let op = match state.try_begin() {
        Ok(op) => op,
        Err(e) => panic!("begin: {e}"),
    };

    assert!(state.is_claimed());
    assert!(state.is_busy(), "the op holds the flag");

    let held = worker.clone();
    let guard = tokio::spawn(async move {
        let _w = held.lock().await;
        tokio::time::sleep(Duration::from_millis(400)).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The worker is locked here. Reading the flags must not wait for it.
    let read = tokio::time::timeout(Duration::from_millis(50), async {
        (state.is_claimed(), state.is_busy())
    })
    .await;
    match read {
        Ok((claimed, busy)) => {
            assert!(claimed, "claim flag readable while the worker is held");
            assert!(busy, "busy flag readable while the worker is held");
        }
        Err(_) => panic!("reading the flags blocked on the worker mutex"),
    }

    // A second op is refused straight away, not queued behind the worker.
    let second = tokio::time::timeout(Duration::from_millis(50), async {
        state.try_begin().is_err()
    })
    .await;
    match second {
        Ok(refused) => assert!(refused, "a second op gets Busy"),
        Err(_) => panic!("try_begin blocked on the worker mutex"),
    }

    if guard.await.is_err() {
        panic!("guard task works");
    }
    drop(op);
    assert!(!state.is_busy(), "dropping the token clears the flag");
    let _ = std::fs::remove_file(&path);
}

/// The token clears the busy flag even when the operation returns early.
#[tokio::test]
async fn an_early_return_still_releases_the_busy_flag() {
    let path = store_path("early");
    write_store(&path, &[]);
    let fake = Fake::new(Vec::new());
    let mut w = claimed(fake, path.clone());
    let state = w.state();
    {
        let op = begin(&w);
        let (tx, _rx) = mpsc::channel(32);
        let result = w
            .enroll(
                &op,
                "not-a-finger".to_string(),
                OpSink::new(OpKind::Enroll, tx),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "the enrol was refused");
    }
    assert!(!state.is_busy(), "the flag is clear after the early return");
    assert!(state.try_begin().is_ok(), "the next op can start");
    let _ = std::fs::remove_file(&path);
}
