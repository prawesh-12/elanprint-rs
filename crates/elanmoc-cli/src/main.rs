use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};
use elanmoc_usb::{Device, EndpointIn, UsbError};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "elanmoc-cli", about = "ELAN 04f3:0c90 fingerprint sensor")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open the device, print what it is, release it. Sends no protocol bytes.
    Probe {
        /// Also read `0x83` with nothing pending, to prove the timeout path works.
        #[arg(long)]
        check_timeout: bool,
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
        Command::Probe { check_timeout } => probe(check_timeout, &cancel).await,
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
