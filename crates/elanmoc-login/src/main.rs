//! Desktop fingerprint login, demo front end over the daemon.
//!
//! Granting here unlocks this window only. Real session auth stays on the PAM
//! path. Needs whatever owns `net.reactivated.Fprint` on the system bus.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod app;
mod client;

use app::LoginApp;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();
    let _guard = runtime.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([480.0, 640.0]),
        ..Default::default()
    };
    let run = eframe::run_native(
        "elanmoc login",
        options,
        Box::new(move |_cc| Ok(Box::new(LoginApp::new(handle)) as Box<dyn eframe::App>)),
    );
    if let Err(e) = run {
        eprintln!("elanmoc-login: {e}");
        std::process::exit(1);
    }
    Ok(())
}
