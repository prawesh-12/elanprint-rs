use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use elanmoc_proto::{
    Command as Cmd, Enroll, EnrollAction, ReplyEndpoint, Response, SlotState, Status,
};
use elanmoc_store::FINGER_NAMES;
use elanmoc_usb::{Device, EndpointIn, UsbError};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "elanmoc-cli", about = "ELAN 04f3:0c90 fingerprint sensor")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Open the device, print what it is, release it. Sends no protocol bytes.
    Probe {
        /// Also read `0x83` with nothing pending, to prove the timeout path works.
        #[arg(long)]
        check_timeout: bool,
    },
    /// Send `fw_ver` and nothing else. The GO / NO-GO probe.
    FwVer,
    /// Firmware version, sensor size and enrolled count.
    Info,
    /// Read one slot's record.
    FingerInfo {
        /// Finger id.
        id: u8,
        /// Send `enrolled_num` first, in the same session.
        #[arg(long)]
        prime: bool,
    },
    /// Wait for a touch and ask the chip whether it matches an enrolled finger.
    ///
    /// Read only. With nothing enrolled, `0xfd` is the expected answer.
    Verify {
        /// Seconds to wait for a touch, overriding the protocol default.
        #[arg(long)]
        wait: Option<u64>,
        /// Post reads on 0x83 and 0x84 at once and report which one answers.
        #[arg(long)]
        dual: bool,
        /// Send `enrolled_num` first, in the same session, as the source's
        /// enroll sequence does. On without it the touch-wait hangs.
        #[arg(long, default_value_t = true, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        prime: bool,
        /// How many times to run `verify` in the same claim. Arming is sent
        /// once, at the front, never between repeats.
        #[arg(long, default_value_t = 1)]
        repeat: u8,
    },
    /// Send `abort` on its own, from an idle session.
    ///
    /// Read only. Tests whether the chip answers `abort` when nothing is pending.
    Abort {
        /// Seconds to wait for the reply, overriding the protocol default.
        #[arg(long)]
        wait: Option<u64>,
    },
    /// Read every slot record from 0 to `upto`.
    Slots {
        /// Highest id to read.
        #[arg(long, default_value_t = 9)]
        upto: u8,
        /// Send `enrolled_num` first, in the same session.
        #[arg(long)]
        prime: bool,
    },
    /// Compare the host store against the device. Prunes nothing by default.
    Sync {
        /// Path to prints.json. Defaults to /var/lib/elanmoc/prints.json.
        #[arg(long)]
        store: Option<String>,
        /// Drop entries the device proves stale. Ambiguous ones stay.
        #[arg(long)]
        prune: bool,
    },
    /// Print the planned enroll bytes for a slot. Sends nothing.
    EnrollPlan {
        /// On-chip slot to enroll into.
        #[arg(long)]
        slot: u8,
    },
    /// Enroll one finger. Writes flash. Needs a touch per sample.
    Enroll {
        /// fprintd finger name, for the host store.
        #[arg(long)]
        finger: String,
        /// On-chip slot to enroll into. Picked from `slots --prime`.
        #[arg(long)]
        slot: u8,
        /// Seconds per touch wait, overriding the protocol defaults.
        #[arg(long)]
        wait: Option<u64>,
        /// Stop after the collision check and wait for /tmp/elanmoc-commit-go
        /// before sending commit. Same claim held throughout.
        #[arg(long)]
        hold_before_commit: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let cancel = CancellationToken::new();
    spawn_signal_handler(cancel.clone());

    match cli.command {
        Action::Probe { check_timeout } => probe(check_timeout, &cancel).await,
        Action::FwVer => session(&cancel, |d, c| Box::pin(fw_ver(d, c))).await,
        Action::Info => session(&cancel, |d, c| Box::pin(info(d, c))).await,
        Action::FingerInfo { id, prime } => {
            session(&cancel, move |d, c| Box::pin(finger_info(d, c, id, prime))).await
        }
        Action::Verify {
            wait,
            dual,
            prime,
            repeat,
        } => {
            session(&cancel, move |d, c| {
                Box::pin(verify(d, c, wait, dual, prime, repeat))
            })
            .await
        }
        Action::Abort { wait } => {
            session(&cancel, move |d, c| Box::pin(abort_alone(d, c, wait))).await
        }
        Action::Slots { upto, prime } => {
            session(&cancel, move |d, c| Box::pin(slots(d, c, upto, prime))).await
        }
        Action::EnrollPlan { slot } => {
            enroll_plan(slot);
            Ok(())
        }
        Action::Enroll { finger, slot, wait, hold_before_commit } => {
            session(&cancel, move |d, c| {
                Box::pin(enroll(d, c, finger, slot, wait, hold_before_commit))
            })
            .await
        }
        Action::Sync { store, prune } => {
            session(&cancel, move |d, c| {
                Box::pin(sync(d, c, store, prune))
            })
            .await
        }
    }
}

fn spawn_signal_handler(cancel: CancellationToken) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("interrupted, cancelling");
            cancel.cancel();
        }
    });
}

type Job<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>>;

/// Open, run `job`, and always send `abort` before releasing if it failed.
///
/// Leaving the chip mid-session makes the next command read a stale response.
async fn session<F>(cancel: &CancellationToken, job: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a Device, &'a CancellationToken) -> Job<'a>,
{
    let device = Device::open().await?;
    let result = job(&device, cancel).await;

    if result.is_err() {
        match send(&device, Cmd::Abort, cancel).await {
            Ok(_) => println!("abort sent (0c90 sends no reply)"),
            Err(e) => println!("abort failed:  {e}"),
        }
    }
    result
}

/// Send one command and parse its reply. Prints the raw bytes either way.
async fn send(
    device: &Device,
    cmd: Cmd,
    cancel: &CancellationToken,
) -> Result<(Vec<u8>, Response)> {
    send_waiting(device, cmd, cmd.timeout(), cancel).await
}

/// Same, with the wait overridden. The bytes sent are identical either way.
async fn send_waiting(
    device: &Device,
    cmd: Cmd,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(Vec<u8>, Response)> {
    let out = cmd.encode();
    let raw = match device
        .cmd(
            &out,
            endpoint(cmd.reply_endpoint()),
            cmd.expected_len(),
            timeout,
            cancel,
        )
        .await
    {
        Ok(bytes) => bytes,
        // finger_info has a documented two byte form. Keep whatever arrived and
        // let the parser judge it rather than discarding the evidence.
        Err(UsbError::ShortRead { data, .. }) => data,
        Err(e) => bail!("{}: {e}", cmd.name()),
    };

    let parsed = Response::parse(&cmd, &raw)?;
    Ok((raw, parsed))
}

fn endpoint(r: ReplyEndpoint) -> EndpointIn {
    match r {
        ReplyEndpoint::Image => EndpointIn::Image,
        ReplyEndpoint::Status => EndpointIn::Status,
        ReplyEndpoint::TouchWait => EndpointIn::TouchWait,
    }
}

async fn fw_ver(device: &Device, cancel: &CancellationToken) -> Result<()> {
    let cmd = Cmd::FwVersion;
    println!("out:           {}", elanmoc_usb::hex(&cmd.encode()));
    let (raw, parsed) = send(device, cmd, cancel).await?;
    println!("in:            {}", elanmoc_usb::hex(&raw));
    if let Response::FwVersion { major, minor } = parsed {
        println!("fw_ver:        {major}.{minor}  (major {major}, minor {minor})");
    }
    Ok(())
}

async fn info(device: &Device, cancel: &CancellationToken) -> Result<()> {
    fw_ver(device, cancel).await?;

    let (raw, parsed) = send(device, Cmd::SensorSize, cancel).await?;
    println!("sensor_size:   {}", elanmoc_usb::hex(&raw));
    if let Response::SensorSize { width, height } = parsed {
        println!("dimensions:    {width} x {height}");
    }

    let (raw, parsed) = send(device, Cmd::EnrolledNum, cancel).await?;
    println!("enrolled_num:  {}", elanmoc_usb::hex(&raw));
    if let Response::EnrolledNum { count } = parsed {
        println!("enrolled:      {count}");
    }
    Ok(())
}

async fn finger_info(
    device: &Device,
    cancel: &CancellationToken,
    id: u8,
    prime: bool,
) -> Result<()> {
    if prime {
        let (raw, _) = send(device, Cmd::EnrolledNum, cancel).await?;
        println!("enrolled_num:  {}", elanmoc_usb::hex(&raw));
    }
    let (raw, parsed) = send(device, Cmd::FingerInfo(id), cancel).await?;
    println!("out:           {}", elanmoc_usb::hex(&Cmd::FingerInfo(id).encode()));
    println!("in ({:>2}):       {}", raw.len(), elanmoc_usb::hex(&raw));
    if let Response::FingerInfo { state, .. } = parsed {
        println!("slot {id}:        {}", describe(&state));
    }
    Ok(())
}

async fn verify(
    device: &Device,
    cancel: &CancellationToken,
    wait: Option<u64>,
    dual: bool,
    prime: bool,
    repeat: u8,
) -> Result<()> {
    if prime {
        let (raw, parsed) = send(device, Cmd::EnrolledNum, cancel).await?;
        println!("enrolled_num:  {}", elanmoc_usb::hex(&raw));
        if let Response::EnrolledNum { count } = parsed {
            println!("enrolled:      {count}");
        }
    }

    for round in 1..=repeat {
        if repeat > 1 {
            println!("--- verify {round} of {repeat}, no re-arm between rounds ---");
        }
        one_verify(device, cancel, wait, dual).await?;
    }
    Ok(())
}

async fn one_verify(
    device: &Device,
    cancel: &CancellationToken,
    wait: Option<u64>,
    dual: bool,
) -> Result<()> {
    let cmd = Cmd::Verify;
    let timeout = wait.map_or_else(|| cmd.timeout(), Duration::from_secs);
    println!("out:           {}  on 0x01", elanmoc_usb::hex(&cmd.encode()));

    let started = Instant::now();
    let (raw, parsed) = if dual {
        println!("waiting up to {timeout:?}, reads posted on BOTH 0x83 and 0x84 ...");
        device.send(&cmd.encode(), Duration::from_secs(1), cancel).await?;
        let (ep, bytes) = device
            .recv_first(
                EndpointIn::Status,
                EndpointIn::TouchWait,
                cmd.expected_len(),
                timeout,
                cancel,
            )
            .await?;
        println!("answered on:   0x{:02x}", ep.address());
        let parsed = Response::parse(&cmd, &bytes)?;
        (bytes, parsed)
    } else {
        println!("waiting up to {timeout:?} for a touch, reply expected on 0x84 ...");
        send_waiting(device, cmd, timeout, cancel).await?
    };
    let elapsed = started.elapsed();

    println!("in:            {}  after {elapsed:.2?}", elanmoc_usb::hex(&raw));
    if let Response::Verify { byte0, status } = parsed {
        println!("byte 0:        0x{byte0:02x}");
        match elanmoc_proto::VerifyOutcome::classify(status) {
            Ok(elanmoc_proto::VerifyOutcome::NoMatch) => {
                println!("status:        0xfd, finger not enrolled. Expected with 0 enrolled.");
            }
            Ok(elanmoc_proto::VerifyOutcome::Match(id)) => {
                println!("status:        match on finger id {id}");
            }
            Ok(elanmoc_proto::VerifyOutcome::Retry(r)) => {
                println!("status:        retry, {r:?}");
            }
            Err(e) => println!("status:        unusable as verify answer: {e}"),
        }
    }
    Ok(())
}

async fn abort_alone(
    device: &Device,
    cancel: &CancellationToken,
    wait: Option<u64>,
) -> Result<()> {
    let cmd = Cmd::Abort;
    let timeout = wait.map_or_else(|| cmd.timeout(), Duration::from_secs);
    println!("out:           {}  on 0x01", elanmoc_usb::hex(&cmd.encode()));
    println!("waiting up to {timeout:?} on 0x83, nothing else pending ...");

    let started = Instant::now();
    let result = send_waiting(device, cmd, timeout, cancel).await;
    let elapsed = started.elapsed();

    match result {
        Ok((raw, parsed)) => {
            println!("in:            {}  after {elapsed:.2?}", elanmoc_usb::hex(&raw));
            if let Response::Abort { status } = parsed {
                match status {
                    Some(s) => println!("status:        {s:?}"),
                    None => println!("status:        no reply, as expected on 0c90"),
                }
            }
            Ok(())
        }
        Err(e) => {
            println!("no reply after {elapsed:.2?}: {e}");
            println!("abort returned nothing from idle.");
            Ok(())
        }
    }
}

async fn slots(
    device: &Device,
    cancel: &CancellationToken,
    upto: u8,
    prime: bool,
) -> Result<()> {
    if prime {
        let (raw, _) = send(device, Cmd::EnrolledNum, cancel).await?;
        println!("enrolled_num:  {}", elanmoc_usb::hex(&raw));
    }
    for id in 0..=upto {
        match send(device, Cmd::FingerInfo(id), cancel).await {
            Ok((raw, Response::FingerInfo { state, .. })) => {
                println!("slot {id}: {:>2} bytes  {}", raw.len(), describe(&state));
                tracing::debug!(id, bytes = %elanmoc_usb::hex(&raw));
            }
            Ok((raw, other)) => println!("slot {id}: unexpected {other:?} from {}", elanmoc_usb::hex(&raw)),
            Err(e) => {
                println!("slot {id}: {e}");
                return Err(e);
            }
        }
    }
    Ok(())
}

fn describe(state: &SlotState) -> String {
    match state {
        SlotState::Empty => "empty".to_string(),
        SlotState::Stuck => "byte 1 is 0xff, sensor in the stuck state".to_string(),
        SlotState::Error(code) => format!("error 0x{code:02x}"),
        SlotState::Enrolled { raw } => {
            format!("enrolled, {}", elanmoc_usb::hex(raw))
        }
    }
}

/// Print the exact bytes an enroll of `slot` will send. No device I/O.
fn enroll_plan(slot: u8) {
    use elanmoc_proto::{TOTAL_ENROLL_ATTEMPTS, sub_id};
    println!("plan for slot {slot}, one claim, armed once, never re-armed:");
    println!("  arm:        {}", elanmoc_usb::hex(&Cmd::EnrolledNum.encode()));
    println!("  pre-check:  {}", elanmoc_usb::hex(&Cmd::Verify.encode()));
    for done in 0..TOTAL_ENROLL_ATTEMPTS {
        let cmd = Cmd::Enroll {
            finger_id: slot,
            total_attempts: TOTAL_ENROLL_ATTEMPTS,
            attempts_done: done,
        };
        println!("  sample {done}:  {}", elanmoc_usb::hex(&cmd.encode()));
    }
    println!("  collision:  {}", elanmoc_usb::hex(&Cmd::CheckCollision.encode()));
    let commit = Cmd::commit(sub_id(slot));
    println!("  commit:     {} bytes", commit.encode().len());
    println!("  commit:     {}", elanmoc_usb::hex(&commit.encode()));
    println!("  sub_id:     0x{:02x}", sub_id(slot));
}

/// Run the full enroll sequence in one claim. Writes flash.
async fn enroll(
    device: &Device,
    cancel: &CancellationToken,
    finger: String,
    slot: u8,
    wait: Option<u64>,
    hold_before_commit: bool,
) -> Result<()> {
    use elanmoc_proto::{TOTAL_ENROLL_ATTEMPTS, sub_id};
    if !FINGER_NAMES.contains(&finger.as_str()) {
        bail!("unknown finger '{finger}', want one of: {}", FINGER_NAMES.join(", "));
    }
    if hold_before_commit {
        let _ = std::fs::remove_file("/tmp/elanmoc-commit-go");
    }
    let touch_wait = wait.map(Duration::from_secs);

    let (_, parsed) = send(device, Cmd::EnrolledNum, cancel).await?;
    let before = match parsed {
        Response::EnrolledNum { count } => count,
        other => bail!("enrolled_num gave unexpected {other:?}"),
    };
    println!("armed once. enrolled: {before}");

    let (_, parsed) = send(device, Cmd::FingerInfo(slot), cancel).await?;
    match parsed {
        Response::FingerInfo {
            state: SlotState::Enrolled { .. },
            ..
        } => bail!("slot {slot} already holds a 70 byte record, pick another"),
        Response::FingerInfo { state, .. } => {
            println!("slot {slot}: {} (2 byte form reads as free)", describe(&state));
        }
        other => bail!("finger_info gave unexpected {other:?}"),
    }

    println!("touch the {finger} now, pre-check, reply 0xfd means it is new ...");
    let verify_wait = touch_wait.unwrap_or_else(|| Cmd::Verify.timeout());
    loop {
        let (_, parsed) = send_waiting(device, Cmd::Verify, verify_wait, cancel).await?;
        match parsed {
            Response::Verify { status: Status::NotEnrolled, .. } => {
                println!("pre-check: 0xfd, finger is new");
                break;
            }
            Response::Verify { status: Status::Retry(r), .. } => {
                println!("pre-check retry ({r:?}), touch again ...");
            }
            Response::Verify { status: Status::Ok(id), .. } => {
                bail!("finger already enrolled as id {id}, stopping");
            }
            Response::Verify { status, .. } => {
                bail!("pre-check gave {status:?}, stopping, no enroll sent");
            }
            other => bail!("verify gave unexpected {other:?}"),
        }
    }

    let mut sm = Enroll::new(slot);
    let EnrollAction::Send(mut out) = sm.start() else {
        bail!("enroll machine did not start with a send");
    };
    println!("samples: {TOTAL_ENROLL_ATTEMPTS} touches, no re-arm between them ...");
    loop {
        println!("touch now, attempt {} of {TOTAL_ENROLL_ATTEMPTS} ...", sm.collected());
        let raw = machine_round(device, &out, touch_wait, cancel).await?;
        match sm.step(&raw) {
            EnrollAction::EmitProgress { done, total } => {
                println!("sample {done} of {total} accepted");
                let Some(EnrollAction::Send(next)) = sm.take_send() else {
                    bail!("machine owes a send after progress");
                };
                out = next;
            }
            EnrollAction::EmitRetry(r) => {
                println!("sample rejected ({r:?}), same attempt again ...");
                let Some(EnrollAction::Send(next)) = sm.take_send() else {
                    bail!("machine owes a resend after retry");
                };
                out = next;
            }
            EnrollAction::Send(next) => {
                println!("collision check clear, commit bytes: {}", elanmoc_usb::hex(&next));
                if hold_before_commit {
                    wait_for_go(cancel).await?;
                }
                println!("sending commit ...");
                let raw = machine_round(device, &next, None, cancel).await?;
                drain_trailing(device, cancel).await;
                match sm.step(&raw) {
                    EnrollAction::Complete(id) => {
                        println!("commit status 0, template id {id}");
                        break;
                    }
                    EnrollAction::Fail(e) => bail!("commit failed: {e}"),
                    other => bail!("unexpected {other:?} after commit"),
                }
            }
            EnrollAction::Complete(id) => {
                println!("complete, template id {id}");
                break;
            }
            EnrollAction::Fail(e) => bail!("enroll failed: {e}"),
        }
    }

    let (_, parsed) = send(device, Cmd::EnrolledNum, cancel).await?;
    if let Response::EnrolledNum { count } = parsed {
        println!("enrolled now: {count} (was {before})");
    }
    let (raw, parsed) = send(device, Cmd::FingerInfo(slot), cancel).await?;
    if let Response::FingerInfo { state, .. } = parsed {
        println!("slot {slot} now: {} bytes, {}", raw.len(), describe(&state));
    }
    println!("sub_id 0x{:02x}, finger {finger}, record this mapping in the store", sub_id(slot));
    Ok(())
}

/// Send one machine-produced command and read its reply.
async fn machine_round(
    device: &Device,
    out: &[u8],
    touch_wait: Option<Duration>,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    let (ep, in_len, timeout) = machine_io(out, touch_wait)?;
    println!("out: {}", elanmoc_usb::hex(out));
    device.send(out, Duration::from_secs(1), cancel).await?;
    match device.recv(ep, in_len, timeout, cancel).await {
        Ok(bytes) => {
            println!("in:  {}", elanmoc_usb::hex(&bytes));
            Ok(bytes)
        }
        Err(UsbError::ShortRead { data, .. }) => {
            println!("in (short): {}", elanmoc_usb::hex(&data));
            Ok(data)
        }
        Err(e) => bail!("touch wait failed ({e}), aborting, no re-arm, report and stop"),
    }
}

/// Endpoint, reply length and timeout for bytes the machine produced.
fn machine_io(out: &[u8], touch_wait: Option<Duration>) -> Result<(EndpointIn, usize, Duration)> {
    if out.len() == 7 && out.starts_with(&[0x40, 0xff, 0x01]) {
        let wait = touch_wait.unwrap_or_else(|| {
            Cmd::Enroll {
                finger_id: 0,
                total_attempts: 8,
                attempts_done: 0,
            }
            .timeout()
        });
        return Ok((EndpointIn::TouchWait, 2, wait));
    }
    if out == [0x40, 0xff, 0x10] {
        return Ok((EndpointIn::Status, 3, Cmd::CheckCollision.timeout()));
    }
    if out.len() == 72 && out.starts_with(&[0x40, 0xff, 0x11]) {
        return Ok((EndpointIn::Status, 2, Cmd::commit(0).timeout()));
    }
    bail!("machine produced bytes outside protocol.md")
}

/// Hold the armed claim until /tmp/elanmoc-commit-go appears.
async fn wait_for_go(cancel: &CancellationToken) -> Result<()> {
    println!("HOLD before commit: create /tmp/elanmoc-commit-go to continue");
    loop {
        if std::path::Path::new("/tmp/elanmoc-commit-go").exists() {
            println!("go received, sending commit on the held claim");
            return Ok(());
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => bail!("cancelled during hold, aborting"),
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

/// Read past the commit reply in case the chip sends trailing packets.
async fn drain_trailing(device: &Device, cancel: &CancellationToken) {
    match device
        .recv(EndpointIn::Status, 64, Duration::from_millis(500), cancel)
        .await
    {
        Ok(extra) if !extra.is_empty() => {
            println!("trailing: {} bytes: {}", extra.len(), elanmoc_usb::hex(&extra));
        }
        Ok(_) => println!("trailing: none"),
        Err(UsbError::Timeout(_)) => println!("trailing: none in 500ms"),
        Err(UsbError::ShortRead { data, .. }) if !data.is_empty() => {
            println!("trailing: {} bytes: {}", data.len(), elanmoc_usb::hex(&data));
        }
        Err(e) => println!("trailing: unreadable ({e})"),
    }
}

/// Compare the host store against the device. Read only unless `--prune`.
async fn sync(
    device: &Device,
    cancel: &CancellationToken,
    store_path: Option<String>,
    prune: bool,
) -> Result<()> {
    use elanmoc_store::{MAX_SLOT, Store, reconcile};
    let path = store_path.unwrap_or_else(|| elanmoc_store::DEFAULT_PATH.to_string());

    let (_, parsed) = send(device, Cmd::EnrolledNum, cancel).await?;
    let Response::EnrolledNum { count } = parsed else {
        bail!("enrolled_num gave unexpected {parsed:?}");
    };
    println!("device counts: {count}");

    let mut records = Vec::new();
    for id in 0..=MAX_SLOT {
        match send(device, Cmd::FingerInfo(id), cancel).await {
            Ok((_, Response::FingerInfo { state, .. })) => records.push((id, state)),
            Ok((_, other)) => bail!("finger_info({id}) gave unexpected {other:?}"),
            Err(e) => bail!("finger_info({id}): {e}"),
        }
    }

    let store = Store::open(std::path::Path::new(&path))?;
    let report = reconcile(store.prints(), count, &records);
    for e in &report.confirmed {
        println!("confirmed:   {}/{} in slot {} ({})", e.user, e.finger, e.slot, e.reason);
    }
    for e in &report.unconfirmed {
        println!("unconfirmed: {}/{} in slot {} ({})", e.user, e.finger, e.slot, e.reason);
    }
    for e in &report.stale {
        println!("stale:       {}/{} in slot {} ({})", e.user, e.finger, e.slot, e.reason);
    }
    for w in &report.warnings {
        println!("warning:     {w}");
    }
    if report.confirmed.is_empty() && report.unconfirmed.is_empty() && report.stale.is_empty() {
        println!("store holds nothing");
    }
    if prune && !report.stale.is_empty() {
        let mut store = store;
        for e in &report.stale {
            store.remove(&e.user, &e.finger);
        }
        store.save()?;
        println!("pruned {} entries proven stale, ambiguous ones kept", report.stale.len());
    } else if prune {
        println!("nothing proven stale, file unchanged");
    }
    Ok(())
}

async fn probe(check_timeout: bool, cancel: &CancellationToken) -> Result<()> {
    let device = Device::open().await?;
    let (bus, address) = device.location();

    println!("device   04f3:0c90 on bus {bus:03} device {address:03}");
    println!("interface 0 claimed");
    println!("endpoints:");
    for ep in device.endpoints() {
        let dir = if ep.is_in { "IN " } else { "OUT" };
        println!(
            "  0x{:02x}  {dir}  {}  wMaxPacketSize {}  bInterval {}",
            ep.address, ep.transfer_type, ep.max_packet_size, ep.interval
        );
    }

    if check_timeout {
        let wait = Duration::from_secs(1);
        let started = Instant::now();
        let result = device.recv(EndpointIn::Status, 2, wait, cancel).await;
        let elapsed = started.elapsed();
        match result {
            Err(UsbError::Timeout(_)) => {
                println!("timeout check: Timeout after {elapsed:.2?}, as expected");
            }
            Err(UsbError::Cancelled) => println!("timeout check: cancelled after {elapsed:.2?}"),
            Err(e) => println!("timeout check: unexpected error after {elapsed:.2?}: {e}"),
            Ok(bytes) => println!(
                "timeout check: unexpected {} bytes: {}",
                bytes.len(),
                elanmoc_usb::hex(&bytes)
            ),
        }
    }

    println!("released cleanly");
    Ok(())
}
