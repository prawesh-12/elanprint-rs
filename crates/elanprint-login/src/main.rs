//! Desktop fingerprint login, demo front end over the daemon.
//!
//! Granting here unlocks this window only. Real session auth stays on the PAM
//! path. Needs whatever owns `net.reactivated.Fprint` on the system bus.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod app;
mod client;
mod copy;
mod fx;
mod icon;
mod keyring_tab;
mod sensor;

use app::LoginApp;

const ICON: &[u8] = include_bytes!("../../../assets/icon-256.png");

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();
    let _guard = runtime.enter();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([400.0, 600.0])
        .with_resizable(false)
        .with_maximize_button(false)
        .with_app_id(copy::TITLE);
    // 256 is the largest size any shell asks for, and the source art would
    // decode to megabytes of RGBA for no gain.
    match eframe::icon_data::from_png_bytes(ICON) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => tracing::warn!("window icon did not load: {e}"),
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let run = eframe::run_native(
        copy::TITLE,
        options,
        Box::new(move |_cc| Ok(Box::new(LoginApp::new(handle)) as Box<dyn eframe::App>)),
    );
    if let Err(e) = run {
        eprintln!("elanprint-login: {e}");
        std::process::exit(1);
    }
    Ok(())
}
