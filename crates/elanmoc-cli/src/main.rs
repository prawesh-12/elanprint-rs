use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use elanmoc_proto::{Command as Cmd, ReplyEndpoint, Response, SlotState, Status};
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
    },
    /// Wait for a touch and ask the chip whether it matches an enrolled finger.
    ///
    /// Read only. With nothing enrolled, `0xfd` is the expected answer.
    Verify {
        /// Seconds to wait for a touch, overriding the protocol default.
        #[arg(long)]
        wait: Option<u64>,
    },
    /// Read every slot record from 0 to `upto`.
    Slots {
        /// Highest id to read.
        #[arg(long, default_value_t = 9)]
        upto: u8,
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
        Action::FingerInfo { id } => {
            session(&cancel, move |d, c| Box::pin(finger_info(d, c, id))).await
        }
        Action::Verify { wait } => {
            session(&cancel, move |d, c| Box::pin(verify(d, c, wait))).await
        }
        Action::Slots { upto } => session(&cancel, move |d, c| Box::pin(slots(d, c, upto))).await,
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
            Ok((raw, _)) => println!("abort:         {}", elanmoc_usb::hex(&raw)),
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

async fn finger_info(device: &Device, cancel: &CancellationToken, id: u8) -> Result<()> {
    let (raw, parsed) = send(device, Cmd::FingerInfo(id), cancel).await?;
    println!("out:           {}", elanmoc_usb::hex(&Cmd::FingerInfo(id).encode()));
    println!("in ({:>2}):       {}", raw.len(), elanmoc_usb::hex(&raw));
    if let Response::FingerInfo { state, .. } = parsed {
        println!("slot {id}:        {}", describe(&state));
    }
    Ok(())
}

async fn verify(device: &Device, cancel: &CancellationToken, wait: Option<u64>) -> Result<()> {
    let cmd = Cmd::Verify;
    let timeout = wait.map_or_else(|| cmd.timeout(), Duration::from_secs);
    println!("out:           {}  on 0x01", elanmoc_usb::hex(&cmd.encode()));
    println!("waiting up to {timeout:?} for a touch, reply expected on 0x84 ...");

    let started = Instant::now();
    let (raw, parsed) = send_waiting(device, cmd, timeout, cancel).await?;
    let elapsed = started.elapsed();

    println!("in:            {}  after {elapsed:.2?}", elanmoc_usb::hex(&raw));
    if let Response::Verify { byte0, status } = parsed {
        println!("byte 0:        0x{byte0:02x}");
        match status {
            Status::NotEnrolled => {
                println!("status:        0xfd, finger not enrolled. Expected with 0 enrolled.");
            }
            Status::Ok(id) => println!("status:        match on finger id {id}"),
            Status::Retry(r) => println!("status:        retry, {r:?}"),
            Status::MaxEnrolled => println!("status:        0xdd, maximum enrolled reached"),
            Status::Unknown(b) => println!("status:        undocumented 0x{b:02x}, logged not assumed"),
        }
    }
    Ok(())
}

async fn slots(device: &Device, cancel: &CancellationToken, upto: u8) -> Result<()> {
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
