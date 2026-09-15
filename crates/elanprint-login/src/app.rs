//! Fingerprint enrolment window over the daemon.
//!
//! The stage owns the one status line. Verify is a self-test; real login is
//! handled by PAM.

use std::sync::Arc;
use std::time::Instant;

use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{AuthEvent, EnrollEvent, FingerprintClient};
use crate::copy;
use crate::fx::{Fx, Pulse};
use crate::icon;
use crate::keyring_tab::{self, KeyringTab, Stage};
use crate::sensor::{self, Found};

/// One spacing unit. Every gap below is a multiple of it.
const U: f32 = 8.0;
/// Content column, centred in the window.
const COL: f32 = 320.0;

const ACCENT: egui::Color32 = egui::Color32::from_rgb(58, 125, 233);
const GOOD: egui::Color32 = egui::Color32::from_rgb(64, 190, 120);
const WARN: egui::Color32 = egui::Color32::from_rgb(232, 165, 42);
const BAD: egui::Color32 = egui::Color32::from_rgb(226, 86, 86);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    Disconnected,
    Ready,
    Waiting,
    Granted,
    Denied,
    Locked,
    Failed,
}

// duplicates elanprint-store FINGER_NAMES, kept to avoid the dep
const ENROLL_FINGERS: [&str; 10] = [
    "left-thumb",
    "left-index-finger",
    "left-middle-finger",
    "left-ring-finger",
    "left-little-finger",
    "right-thumb",
    "right-index-finger",
    "right-middle-finger",
    "right-ring-finger",
    "right-little-finger",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Verify,
    Enroll,
    Keyring,
}

/// What the mark and the status line are saying right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Dim,
    Neutral,
    Retry,
    Good,
    Bad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnrollTone {
    Idle,
    Hint,
    Done,
    Error,
}

type ConnectOutcome = Result<(Arc<FingerprintClient>, Vec<String>, i32), String>;

pub struct LoginApp {
    user: String,
    finger: String,
    fingers: Vec<String>,
    status: String,
    screen: Screen,
    pending: Option<std::sync::mpsc::Receiver<ConnectOutcome>>,
    events: Option<mpsc::Receiver<AuthEvent>>,
    cancel: Option<CancellationToken>,
    handle: Handle,
    client: Option<Arc<FingerprintClient>>,
    fx: Fx,
    pulse: Pulse,
    mode: Mode,
    enroll_finger: String,
    enroll_events: Option<mpsc::Receiver<EnrollEvent>>,
    enroll_cancel: Option<CancellationToken>,
    enroll_status: String,
    enroll_tone: EnrollTone,
    enroll_done: u8,
    enroll_total: u8,
    delete_armed: Option<String>,
    delete_pending: bool,
    list_pending: Option<std::sync::mpsc::Receiver<Result<Vec<String>, String>>>,
    sensor: Found,
    keyring: KeyringTab,
}

impl LoginApp {
    /// The unix user comes from the process, never from the window.
    pub fn new(handle: Handle) -> Self {
        let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
        Self {
            user,
            finger: "any".to_string(),
            fingers: vec!["any".to_string()],
            status: "connect to the reader first".to_string(),
            screen: Screen::Disconnected,
            pending: None,
            events: None,
            cancel: None,
            handle,
            client: None,
            fx: Fx::new(),
            pulse: Pulse::Off,
            mode: Mode::Enroll,
            enroll_finger: ENROLL_FINGERS[6].to_string(),
            enroll_events: None,
            enroll_cancel: None,
            enroll_status: String::new(),
            enroll_tone: EnrollTone::Idle,
            enroll_done: 0,
            enroll_total: 0,
            delete_armed: None,
            delete_pending: false,
            list_pending: None,
            sensor: sensor::scan(),
            keyring: KeyringTab::new(),
        }
    }

    fn tone(&self) -> Tone {
        match self.mode {
            Mode::Verify => match self.screen {
                Screen::Disconnected => Tone::Dim,
                Screen::Granted => Tone::Good,
                Screen::Denied | Screen::Locked | Screen::Failed => Tone::Bad,
                _ => Tone::Neutral,
            },
            Mode::Enroll => {
                if self.client.is_none() {
                    return Tone::Dim;
                }
                match self.enroll_tone {
                    EnrollTone::Hint => Tone::Retry,
                    EnrollTone::Done => Tone::Good,
                    EnrollTone::Error => Tone::Bad,
                    EnrollTone::Idle => Tone::Neutral,
                }
            }
            Mode::Keyring => match self.keyring.stage {
                Stage::Done => Tone::Good,
                Stage::Failed(_) => Tone::Bad,
                Stage::Idle if !self.keyring.looks_enabled() => Tone::Dim,
                _ => Tone::Neutral,
            },
        }
    }

    /// `Some(true)` draws the check, `Some(false)` the cross.
    fn outcome(&self) -> Option<bool> {
        match self.mode {
            Mode::Verify => match self.screen {
                Screen::Granted => Some(true),
                Screen::Denied => Some(false),
                _ => None,
            },
            Mode::Enroll if self.enroll_events.is_none() => match self.enroll_tone {
                EnrollTone::Done => Some(true),
                EnrollTone::Error => Some(false),
                _ => None,
            },
            Mode::Enroll => None,
            Mode::Keyring => match self.keyring.stage {
                Stage::Done => Some(true),
                Stage::Failed(_) => Some(false),
                _ => None,
            },
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        if self.pending.is_some() {
            return;
        }
        let user = self.user.clone();
        let handle = self.handle.clone();
        let ctx = ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel::<ConnectOutcome>();
        std::thread::spawn(move || {
            let outcome = handle.block_on(async {
                let client = FingerprintClient::connect()
                    .await
                    .map_err(|e| e.friendly())?;
                let fingers = client.list(&user).await.map_err(|e| e.friendly())?;
                let stages = client.stages().await.unwrap_or(-1);
                Ok::<_, String>((Arc::new(client), fingers, stages))
            });
            let _ = tx.send(outcome);
            ctx.request_repaint();
        });
        self.pending = Some(rx);
        self.status = "connecting on the system bus".to_string();
    }

    fn start_login(&mut self) {
        if self.events.is_some() {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.status = "connect first".to_string();
            return;
        };
        let (tx, rx) = mpsc::channel::<AuthEvent>(32);
        let cancel = CancellationToken::new();
        let user = self.user.clone();
        let finger = self.finger.clone();
        let cancel_task = cancel.clone();
        self.handle.spawn(async move {
            if let Err(e) = client.verify(&user, &finger, tx.clone(), cancel_task).await {
                let _ = tx.send(AuthEvent::Failed(e.friendly())).await;
            }
        });
        self.events = Some(rx);
        self.cancel = Some(cancel);
        self.screen = Screen::Waiting;
        self.status = copy::TOUCH.to_string();
    }

    fn cancel_login(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.events = None;
        self.screen = Screen::Ready;
        self.status = "cancelled".to_string();
    }

    fn logout(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.events = None;
        self.screen = Screen::Ready;
        self.status = "signed out".to_string();
    }

    fn start_enroll(&mut self) {
        if self.enroll_events.is_some() {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.enroll_status = "connect first".to_string();
            self.enroll_tone = EnrollTone::Error;
            return;
        };
        let (tx, rx) = mpsc::channel::<EnrollEvent>(32);
        let cancel = CancellationToken::new();
        let user = self.user.clone();
        let finger = self.enroll_finger.clone();
        let cancel_task = cancel.clone();
        self.handle.spawn(async move {
            if let Err(e) = client.enroll(&user, &finger, tx.clone(), cancel_task).await {
                let _ = tx.send(EnrollEvent::Failed(e.friendly())).await;
            }
        });
        self.enroll_events = Some(rx);
        self.enroll_cancel = Some(cancel);
        self.enroll_done = 0;
        self.enroll_tone = EnrollTone::Idle;
        self.enroll_status = copy::TOUCH_FIRST.to_string();
    }

    fn cancel_enroll(&mut self) {
        if let Some(cancel) = self.enroll_cancel.take() {
            cancel.cancel();
        }
        self.enroll_events = None;
        self.enroll_done = 0;
        self.enroll_status = copy::PICK_ENROLL.to_string();
        self.enroll_tone = EnrollTone::Idle;
    }

    fn refresh_fingers(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let user = self.user.clone();
        let handle = self.handle.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<String>, String>>();
        std::thread::spawn(move || {
            let outcome =
                handle.block_on(async { client.list(&user).await.map_err(|e| e.friendly()) });
            let _ = tx.send(outcome);
        });
        self.list_pending = Some(rx);
    }

    fn request_delete(&mut self, finger: String) {
        if self.delete_pending || self.enroll_events.is_some() {
            return;
        }
        if self.delete_armed.as_ref() != Some(&finger) {
            self.delete_armed = Some(finger.clone());
            self.enroll_status = format!("click {finger} again to confirm delete");
            self.enroll_tone = EnrollTone::Hint;
            return;
        }
        let Some(client) = self.client.clone() else {
            self.enroll_status = "connect first".to_string();
            self.enroll_tone = EnrollTone::Error;
            return;
        };
        self.delete_armed = None;
        let user = self.user.clone();
        let handle = self.handle.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<String>, String>>();
        std::thread::spawn(move || {
            let outcome = handle.block_on(async {
                client
                    .delete_finger(&user, &finger)
                    .await
                    .map_err(|e| e.friendly())?;
                client.list(&user).await.map_err(|e| e.friendly())
            });
            let _ = tx.send(outcome);
        });
        self.list_pending = Some(rx);
        self.delete_pending = true;
        self.enroll_status = "deleting".to_string();
        self.enroll_tone = EnrollTone::Idle;
    }

    fn poll(&mut self) {
        self.poll_enroll();
        self.poll_list_pending();
        if let Some(rx) = self.pending.as_ref() {
            if let Ok(outcome) = rx.try_recv() {
                self.pending = None;
                match outcome {
                    Ok((client, fingers, stages)) => {
                        self.client = Some(client);
                        self.fingers = fingers;
                        if !self.fingers.contains(&self.finger) {
                            self.finger = "any".to_string();
                        }
                        if stages > 0 {
                            self.enroll_total = stages.min(255) as u8;
                        }
                        self.screen = Screen::Ready;
                        self.status = "reader ready".to_string();
                    }
                    Err(text) => {
                        self.screen = Screen::Disconnected;
                        self.status = text;
                    }
                }
            }
        }
        let mut done = false;
        if let Some(rx) = self.events.as_mut() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    AuthEvent::Prompt(text) => self.status = text,
                    AuthEvent::Retry(hint) => {
                        self.status = hint;
                        self.fx.flash(WARN);
                    }
                    AuthEvent::Finger(finger) => {
                        self.status = format!("verifying {finger}");
                    }
                    AuthEvent::Granted => {
                        self.screen = Screen::Granted;
                        self.fx.flash(GOOD);
                        done = true;
                    }
                    AuthEvent::Denied(_) => {
                        self.screen = Screen::Denied;
                        self.fx.shake();
                        done = true;
                    }
                    AuthEvent::Locked => {
                        self.screen = Screen::Locked;
                        self.fx.set_pulse(Pulse::Off);
                        self.pulse = Pulse::Off;
                        done = true;
                    }
                    AuthEvent::Failed(text) => {
                        self.screen = Screen::Failed;
                        self.status = text;
                        done = true;
                    }
                    AuthEvent::Finished => done = true,
                }
            }
        }
        if done {
            self.events = None;
            self.cancel = None;
        }
    }

    fn poll_enroll(&mut self) {
        let mut done = false;
        let mut need_refresh = false;
        if let Some(rx) = self.enroll_events.as_mut() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    EnrollEvent::Stage { done: d, total: t } => {
                        self.enroll_done = d;
                        if t > 0 {
                            self.enroll_total = t;
                        }
                        self.enroll_status = copy::touch_again(self.enroll_done, self.enroll_total);
                        self.enroll_tone = EnrollTone::Idle;
                        self.fx.flash(ACCENT);
                    }
                    EnrollEvent::Retry(hint) => {
                        self.enroll_status = hint;
                        self.enroll_tone = EnrollTone::Hint;
                        self.fx.flash(WARN);
                    }
                    EnrollEvent::Completed => {
                        self.enroll_done = self.enroll_total;
                        self.enroll_status = format!("{} enrolled", self.enroll_finger);
                        self.enroll_tone = EnrollTone::Done;
                        self.fx.flash(GOOD);
                        done = true;
                        need_refresh = true;
                    }
                    EnrollEvent::Failed(text) => {
                        self.enroll_status = text;
                        self.enroll_tone = EnrollTone::Error;
                        self.fx.flash(BAD);
                        self.fx.shake();
                        done = true;
                    }
                    EnrollEvent::Finished => done = true,
                }
            }
        }
        if done {
            self.enroll_events = None;
            self.enroll_cancel = None;
        }
        if need_refresh {
            self.refresh_fingers();
        }
    }

    fn poll_list_pending(&mut self) {
        if let Some(rx) = self.list_pending.as_ref() {
            if let Ok(outcome) = rx.try_recv() {
                self.list_pending = None;
                self.delete_pending = false;
                match outcome {
                    Ok(fingers) => {
                        self.fingers = fingers;
                        if !self.fingers.contains(&self.finger) {
                            self.finger = "any".to_string();
                        }
                    }
                    Err(text) => {
                        self.enroll_status = text;
                        self.enroll_tone = EnrollTone::Error;
                    }
                }
            }
        }
    }
}

impl eframe::App for LoginApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.keyring.poll();
        if self.keyring.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
        polish(ctx);
        let active = self.events.is_some() || self.enroll_events.is_some();
        let want = if active { Pulse::Breathe } else { Pulse::Off };
        if want != self.pulse {
            self.fx.set_pulse(want);
            self.pulse = want;
        }
        let now = Instant::now();
        if self.client.is_some() {
            egui::TopBottomPanel::bottom("sensor_info")
                .show_separator_line(false)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(COL);
                        ui.add_space(U * 0.5);
                        sensor_info(ui, self);
                        ui.add_space(U * 0.5);
                    });
                });
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.set_max_width(COL);
                ui.add_space(U);
                header(ui, self);
                ui.add_space(U * 2.0);
                stage(ui, self, now);
                ui.add_space(U * 2.0);
                controls(ui, self, ctx);
            });
        });
        if self.fx.fast_animating(now) {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        } else if active || self.pending.is_some() || self.list_pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

/// The status line leads. Everything else sits a size below it.
fn polish(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(12.0, egui::FontFamily::Proportional),
    );
    style.spacing.item_spacing = egui::vec2(U, U);
    style.spacing.button_padding = egui::vec2(U * 1.5, U);
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);
    ctx.set_style(style);
}

fn header(ui: &mut egui::Ui, app: &LoginApp) {
    let (colour, text) = if app.client.is_some() {
        (GOOD, copy::CONNECTED)
    } else {
        (ui.visuals().weak_text_color(), copy::NOT_CONNECTED)
    };
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(text).size(12.0).color(colour));
        if app.client.is_some() {
            ui.add_space(U * 0.25);
            ui.label(egui::RichText::new(&app.user).size(12.0).weak());
        }
    });
}

/// Mark, status line, dots. One centred column, nothing else moves.
fn stage(ui: &mut egui::Ui, app: &LoginApp, now: Instant) {
    let dimmed = match app.mode {
        Mode::Verify => matches!(app.screen, Screen::Disconnected | Screen::Locked),
        Mode::Enroll => app.client.is_none(),
        Mode::Keyring => !app.keyring.looks_enabled(),
    };
    let tone = app.tone();
    let outcome = app.outcome();
    let (done, total) = match app.mode {
        Mode::Enroll => (app.enroll_done, app.enroll_total.max(1)),
        Mode::Verify | Mode::Keyring => (0, 1),
    };

    // the mark's colour is its state, so no second accent element is needed
    let colour = match tone {
        Tone::Dim => ui.visuals().weak_text_color(),
        Tone::Neutral => ui.visuals().text_color(),
        Tone::Retry => WARN,
        Tone::Good => GOOD,
        Tone::Bad => BAD,
    };
    let colour = app.fx.flash_now(now).unwrap_or(colour);

    let dx = app.fx.shake_dx(now);
    let size = 84.0;
    ui.vertical_centered(|ui| {
        let (slot, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        let rect = slot.translate(egui::vec2(dx, 0.0));
        match outcome {
            Some(true) => icon::check(ui, rect.shrink(size * 0.22), GOOD),
            Some(false) => icon::cross(ui, rect.shrink(size * 0.24), BAD),
            None => {
                let mut m = icon::Mark::new(size, colour);
                m.alpha = if dimmed {
                    0.45
                } else {
                    app.fx.pulse_alpha(now)
                };
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                let _ = icon::mark(&mut child, &m);
            }
        }

        ui.add_space(U * 2.0);
        ui.label(
            egui::RichText::new(stage_line(app))
                .size(20.0)
                .color(match tone {
                    Tone::Retry => WARN,
                    Tone::Good => GOOD,
                    Tone::Bad => BAD,
                    Tone::Dim => ui.visuals().weak_text_color(),
                    Tone::Neutral => ui.visuals().text_color(),
                }),
        );

        ui.add_space(U);
        if app.mode == Mode::Enroll && app.client.is_some() && total > 1 {
            dots(ui, done, total);
        } else {
            ui.allocate_exact_size(egui::vec2(0.0, U), egui::Sense::hover());
        }

        if app.client.is_none() {
            ui.add_space(U * 1.5);
            sensor_identity(ui, &app.sensor);
        }
    });
}

/// Name the hardware. Without a daemon this is all the window can say.
fn sensor_identity(ui: &mut egui::Ui, found: &Found) {
    match found {
        Found::Supported(s) => {
            ui.label(egui::RichText::new(s.title()).size(13.0));
            ui.label(egui::RichText::new(s.address()).size(12.0).weak());
        }
        Found::Unsupported(s) => {
            ui.label(
                egui::RichText::new(format!(
                    "Found ELAN {}, which this driver does not support.",
                    s.ids()
                ))
                .size(13.0)
                .color(WARN),
            );
            ui.label(
                egui::RichText::new("Only 04f3:0c90 is supported.")
                    .size(12.0)
                    .weak(),
            );
        }
        Found::None => {
            ui.label(
                egui::RichText::new("No ELAN fingerprint sensor on this machine.")
                    .size(12.0)
                    .weak(),
            );
        }
    }
}

/// Collapsed by default: the name is the useful part, the rest is detail.
fn sensor_info(ui: &mut egui::Ui, app: &LoginApp) {
    let Found::Supported(s) = &app.sensor else {
        return;
    };
    egui::CollapsingHeader::new(egui::RichText::new("Sensor info").size(12.0).weak())
        .id_salt("sensor_info")
        .default_open(false)
        .show(ui, |ui| {
            // the window cannot grow, so expanding must not push the button out
            ui.spacing_mut().item_spacing.y = 1.0;
            let templates = app.fingers.iter().filter(|f| f.as_str() != "any").count();
            let stages = if app.enroll_total > 0 {
                app.enroll_total.to_string()
            } else {
                "unknown until claimed".to_string()
            };
            for (k, v) in [
                ("Device", s.title()),
                (
                    "Firmware",
                    s.firmware.clone().unwrap_or_else(|| "unknown".into()),
                ),
                ("Enrol stages", stages),
                ("Templates enrolled", templates.to_string()),
            ] {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(k).size(12.0).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(v).size(12.0));
                    });
                });
            }
        });
}

fn stage_line(app: &LoginApp) -> String {
    match app.mode {
        Mode::Verify => match app.screen {
            Screen::Disconnected => copy::READER_NOT_FOUND.to_string(),
            Screen::Ready => copy::PICK_VERIFY.to_string(),
            Screen::Waiting => app.status.clone(),
            Screen::Granted => copy::VERIFIED.to_string(),
            Screen::Denied => copy::NOT_RECOGNISED.to_string(),
            Screen::Locked => copy::PASSWORD_FALLBACK.to_string(),
            Screen::Failed => app.status.clone(),
        },
        Mode::Enroll => {
            if app.client.is_none() {
                return copy::READER_NOT_FOUND.to_string();
            }
            if app.enroll_events.is_some() || !app.enroll_status.is_empty() {
                return app.enroll_status.clone();
            }
            copy::PICK_ENROLL.to_string()
        }
        Mode::Keyring => match &app.keyring.stage {
            Stage::Idle if app.keyring.looks_enabled() => copy::KEYRING_ON.to_string(),
            Stage::Idle => copy::KEYRING_OFF.to_string(),
            Stage::Confirm => copy::KEYRING_CONFIRM.to_string(),
            Stage::ConfirmRotate => copy::KEYRING_REPLACE.to_string(),
            Stage::Working(_) => copy::KEYRING_WORKING.to_string(),
            Stage::Saved { .. } => copy::KEYRING_SAVE_CODE.to_string(),
            Stage::Rekey { .. } => copy::KEYRING_REKEY.to_string(),
            Stage::ShowKey(_) => copy::KEYRING_KEY.to_string(),
            Stage::Rotated(_) => copy::KEYRING_REPLACED.to_string(),
            Stage::Done => copy::KEYRING_DONE.to_string(),
            Stage::Failed(why) => why.clone(),
        },
    }
}

fn dots(ui: &mut egui::Ui, done: u8, total: u8) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = U * 0.75;
        for i in 0..total {
            let colour = if i < done {
                ACCENT
            } else {
                ui.visuals().weak_text_color().gamma_multiply(0.4)
            };
            dot_mark(ui, colour, 4.0);
        }
    });
}

fn controls(ui: &mut egui::Ui, app: &mut LoginApp, ctx: &egui::Context) {
    let locked = app.events.is_some() || app.enroll_events.is_some() || app.keyring.busy();
    segmented(ui, &mut app.mode, !locked);
    ui.add_space(U * 2.0);
    match app.mode {
        Mode::Verify => verify_controls(ui, app),
        Mode::Enroll => enroll_controls(ui, app, ctx),
        Mode::Keyring => keyring_controls(ui, app),
    }
}

fn verify_controls(ui: &mut egui::Ui, app: &mut LoginApp) {
    let editing = matches!(app.screen, Screen::Disconnected | Screen::Ready);
    let armed = matches!(app.screen, Screen::Ready);

    // The daemon refuses any uid but the caller's, so a typed name could only
    // produce a confusing failure. The process knows who it is.
    let known = app.fingers.clone();
    labelled_combo(
        ui,
        "Finger",
        "finger",
        &mut app.finger,
        editing,
        |ui, current| {
            for name in known.clone() {
                ui.selectable_value(current, name.clone(), copy::finger_label(&name));
            }
        },
    );
    ui.add_space(U * 3.0);

    match app.screen {
        Screen::Waiting => {
            if full_button(ui, copy::CANCEL).clicked() {
                app.cancel_login();
            }
        }
        Screen::Granted => {
            if full_button(ui, copy::SIGNOUT).clicked() {
                app.logout();
            }
        }
        _ => {
            let retry = matches!(app.screen, Screen::Denied | Screen::Failed | Screen::Locked);
            let label = if retry {
                copy::TRY_AGAIN
            } else {
                copy::VERIFY_ACTION
            };
            if primary_button(ui, label, armed || retry).clicked() {
                app.start_login();
            }
        }
    }

    ui.add_space(U * 1.5);
    ui.label(egui::RichText::new(copy::SELF_TEST).size(12.0).weak());

    if app.fingers.len() == 1 && app.fingers.first().is_some_and(|f| f == "any") {
        ui.add_space(U);
        ui.label(egui::RichText::new(copy::NO_FINGERS).size(12.0).weak());
    }
    if matches!(app.screen, Screen::Locked | Screen::Failed) {
        ui.add_space(U);
        ui.label(
            egui::RichText::new(copy::PASSWORD_FALLBACK)
                .size(12.0)
                .weak(),
        );
    }
}

fn enroll_controls(ui: &mut egui::Ui, app: &mut LoginApp, ctx: &egui::Context) {
    if app.client.is_none() {
        if primary_button(ui, copy::CONNECT, true).clicked() {
            app.connect(ctx);
        }
        if app.pending.is_some() {
            ui.label(egui::RichText::new("connecting").small().weak());
        }
        return;
    }
    let busy = app.enroll_events.is_some() || app.delete_pending;
    let enrolled = app.fingers.clone();
    labelled_combo(
        ui,
        "Finger",
        "enroll_finger",
        &mut app.enroll_finger,
        !busy,
        |ui, current| {
            for name in ENROLL_FINGERS {
                if enrolled.iter().any(|f| f == name) {
                    ui.add_enabled_ui(false, |ui| {
                        ui.label(format!("{name} (enrolled)"));
                    });
                } else {
                    ui.selectable_value(current, name.to_string(), name);
                }
            }
        },
    );
    ui.add_space(U * 3.0);
    if app.enroll_events.is_some() {
        if full_button(ui, copy::CANCEL_ENROLL).clicked() {
            app.cancel_enroll();
        }
    } else if primary_button(ui, copy::START_ENROLL, !app.delete_pending).clicked() {
        app.start_enroll();
    }
    ui.add_space(U * 1.5);
    ui.label(egui::RichText::new(copy::ENROLLED_LIST).size(12.0).weak());
    let listed: Vec<String> = app
        .fingers
        .iter()
        .filter(|f| f.as_str() != "any")
        .cloned()
        .collect();
    if listed.is_empty() {
        ui.label(egui::RichText::new(copy::NO_FINGERS).size(12.0).weak());
        return;
    }
    let room = (ui.available_height() - U).max(0.0);
    egui::ScrollArea::vertical()
        .max_height(room)
        .show(ui, |ui| {
            for name in listed {
                ui.horizontal(|ui| {
                    ui.set_width(COL);
                    ui.label(&name);
                    let armed = app.delete_armed.as_ref() == Some(&name);
                    let label = if armed {
                        copy::CONFIRM_DELETE
                    } else {
                        copy::DELETE
                    };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            if ui.button(label).clicked() {
                                app.request_delete(name.clone());
                            }
                        });
                    });
                });
            }
        });
}

fn keyring_row(ui: &mut egui::Ui, label: &str, value: &str, good: bool) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(11.0).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let colour = if good {
                GOOD
            } else {
                ui.visuals().weak_text_color()
            };
            ui.label(egui::RichText::new(value).size(11.0).color(colour));
        });
    });
}

fn keyring_note(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(11.0)
            .color(ui.visuals().weak_text_color()),
    );
}

fn keyring_controls(ui: &mut egui::Ui, app: &mut LoginApp) {
    match std::mem::replace(&mut app.keyring.stage, Stage::Idle) {
        Stage::Idle if app.keyring.looks_enabled() => keyring_enabled(ui, app),
        Stage::Idle => keyring_offer(ui, app),
        Stage::Confirm => keyring_confirm(ui, app),
        Stage::Working(note) => {
            keyring_note(ui, note);
            app.keyring.stage = Stage::Working(note);
        }
        Stage::Saved { secret, typed } => keyring_saved(ui, app, secret, typed),
        Stage::Rekey { secret, password } => keyring_rekey(ui, app, secret, password),
        Stage::ShowKey(secret) => {
            keyring_note(ui, "Opens the keyring if the TPM ever stops unsealing.");
            ui.add_space(U);
            keyring_secret(ui, &secret);
            ui.add_space(U * 0.75);
            if app.keyring.recovery_file {
                if full_button(ui, "Delete the copy in /root").clicked() {
                    app.keyring.start_forget_file();
                    return;
                }
                ui.add_space(U * 0.75);
            }
            if full_button(ui, "Close").clicked() {
                return;
            }
            app.keyring.stage = Stage::ShowKey(secret);
        }
        Stage::ConfirmRotate => {
            keyring_note(
                ui,
                "A new key is sealed and the keyring is re-keyed to it. The old \
                 key stops working, including any copy you saved.",
            );
            ui.add_space(U * 1.5);
            if primary_button(ui, "Replace the key", true).clicked() {
                app.keyring.start_rotate();
                return;
            }
            ui.add_space(U * 0.5);
            if full_button(ui, copy::CANCEL).clicked() {
                return;
            }
            app.keyring.stage = Stage::ConfirmRotate;
        }
        Stage::Rotated(secret) => {
            keyring_note(ui, "Replaced. This is your new recovery key. Save it.");
            ui.add_space(U);
            keyring_secret(ui, &secret);
            ui.add_space(U * 1.5);
            if full_button(ui, "Close").clicked() {
                return;
            }
            app.keyring.stage = Stage::Rotated(secret);
        }
        Stage::Done => {
            keyring_note(
                ui,
                "Log out and back in with your finger. Your saved passwords open with it from now on.",
            );
            ui.add_space(U * 1.5);
            if full_button(ui, "Close").clicked() {
                app.keyring.refresh();
                return;
            }
            app.keyring.stage = Stage::Done;
        }
        Stage::Failed(why) => {
            keyring_note(ui, &why);
            ui.add_space(U * 1.5);
            if full_button(ui, "Back").clicked() {
                app.keyring.refresh();
                return;
            }
            app.keyring.stage = Stage::Failed(why);
        }
    }
}

fn keyring_status_rows(ui: &mut egui::Ui, app: &LoginApp) {
    let report = &app.keyring.report;
    keyring_row(ui, "TPM 2.0", if report.tpm { "yes" } else { "no" }, report.tpm);
    keyring_row(
        ui,
        "PAM module",
        if report.module.is_some() { "installed" } else { "missing" },
        report.module.is_some(),
    );
    keyring_row(
        ui,
        "Fingerprint login",
        if report.fingerprint_wired { "wired" } else { "not wired" },
        report.fingerprint_wired,
    );
    keyring_row(
        ui,
        "Password login",
        if report.password_wired { "wired" } else { "not wired" },
        report.password_wired,
    );
}

fn keyring_enabled(ui: &mut egui::Ui, app: &mut LoginApp) {
    keyring_note(ui, "Your saved passwords open when you touch the sensor.");
    ui.add_space(U * 1.5);

    let mut action = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = U;
        if half_button(ui, "Show key").clicked() {
            action = Some(Action::Reveal);
        }
        if half_button(ui, "Replace key").clicked() {
            action = Some(Action::Rotate);
        }
    });

    if !app.keyring.report.password_wired {
        ui.add_space(U * 0.75);
        if full_button(ui, "Unlock on password login too").clicked() {
            action = Some(Action::WirePassword);
        }
    }

    ui.add_space(U * 0.75);
    if full_button(ui, "Turn off").clicked() {
        action = Some(Action::Disable);
    }

    match action {
        Some(Action::Reveal) => app.keyring.start_reveal(),
        Some(Action::Rotate) => app.keyring.stage = Stage::ConfirmRotate,
        Some(Action::WirePassword) => app.keyring.start_wire_password(),
        Some(Action::Disable) => app.keyring.start_disable(),
        None => {}
    }
}

enum Action {
    Reveal,
    Rotate,
    WirePassword,
    Disable,
}

fn keyring_offer(ui: &mut egui::Ui, app: &mut LoginApp) {
    keyring_status_rows(ui, app);
    ui.add_space(U * 2.0);
    if let Some(why) = keyring_tab::ready_error(&app.keyring.report) {
        keyring_note(ui, &why);
        return;
    }
    keyring_note(
        ui,
        "A fingerprint cannot open the keyring on its own. This keeps a secret in the TPM and hands it over when your finger matches.",
    );
    ui.add_space(U * 1.5);
    if primary_button(ui, "Set up", true).clicked() {
        app.keyring.stage = Stage::Confirm;
    }
}

fn keyring_confirm(ui: &mut egui::Ui, app: &mut LoginApp) {
    keyring_note(
        ui,
        "Your account password stops opening the keyring. The TPM secret opens it instead, handed over when your finger matches.",
    );
    ui.add_space(U);
    keyring_note(
        ui,
        "You get a recovery key, also written to /root/keyring-key.txt. Without it, a TPM that stops unsealing means the keyring is lost.",
    );
    ui.add_space(U * 1.5);
    let mut also_password = !app.keyring.fingerprint_only;
    if ui
        .checkbox(&mut also_password, "Also unlock on password login")
        .changed()
    {
        app.keyring.fingerprint_only = !also_password;
    }
    ui.add_space(U);
    if primary_button(ui, "Continue", true).clicked() {
        app.keyring.start_provision();
        return;
    }
    ui.add_space(U * 0.5);
    if full_button(ui, copy::CANCEL).clicked() {
        return;
    }
    app.keyring.stage = Stage::Confirm;
}

fn keyring_saved(ui: &mut egui::Ui, app: &mut LoginApp, secret: String, mut typed: String) {
    keyring_secret(ui, &secret);
    ui.add_space(U);
    keyring_note(ui, "Type it back to confirm you have saved it.");
    ui.add_space(U * 0.5);
    ui.add(
        egui::TextEdit::singleline(&mut typed)
            .desired_width(COL)
            .font(egui::TextStyle::Monospace),
    );
    ui.add_space(U);
    let matches = typed.trim() == secret;
    if primary_button(ui, "I have saved it", matches).clicked() {
        app.keyring.stage = Stage::Rekey {
            secret,
            password: String::new(),
        };
        return;
    }
    app.keyring.stage = Stage::Saved { secret, typed };
}

fn keyring_rekey(ui: &mut egui::Ui, app: &mut LoginApp, secret: String, mut password: String) {
    keyring_note(
        ui,
        "Your keyring's current password, which is your account password unless you have changed it. This is the keyring's, not a login.",
    );
    ui.add_space(U);
    ui.add(
        egui::TextEdit::singleline(&mut password)
            .desired_width(COL)
            .password(true),
    );
    ui.add_space(U);
    if primary_button(ui, "Finish", !password.is_empty()).clicked() {
        app.keyring.start_rekey(secret, password);
        return;
    }
    app.keyring.stage = Stage::Rekey { secret, password };
}

fn keyring_secret(ui: &mut egui::Ui, secret: &str) {
    ui.add(egui::Label::new(egui::RichText::new(secret).monospace().size(11.0)).wrap());
    ui.add_space(U * 0.75);
    if full_button(ui, "Copy").clicked() {
        ui.ctx().copy_text(secret.to_string());
    }
}

/// Equal treatment, active one filled.
fn segmented(ui: &mut egui::Ui, mode: &mut Mode, enabled: bool) {
    let seg = (COL - U * 2.0) / 3.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = U;
        for (m, label) in [
            (Mode::Verify, "Verify"),
            (Mode::Enroll, "Enrol"),
            (Mode::Keyring, "Keyring"),
        ] {
            let active = *mode == m;
            let text = if active {
                egui::RichText::new(label).color(ui.visuals().strong_text_color())
            } else {
                egui::RichText::new(label).color(ui.visuals().weak_text_color())
            };
            let fill = if active {
                ACCENT.gamma_multiply(0.22)
            } else {
                ui.visuals().widgets.inactive.bg_fill.gamma_multiply(0.5)
            };
            let button = egui::Button::new(text)
                .fill(fill)
                .min_size(egui::vec2(seg, U * 4.5));
            if ui.add_enabled(enabled, button).clicked() {
                *mode = m;
            }
        }
    });
}

/// The one accent-filled action. Inputs stay neutral.
fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let button = egui::Button::new(
        egui::RichText::new(text)
            .color(egui::Color32::WHITE)
            .strong(),
    )
    .fill(ACCENT)
    .min_size(egui::vec2(COL, U * 5.5));
    ui.add_enabled(enabled, button)
}

/// Label above, left aligned, control the same width as the button.
fn labelled_combo(
    ui: &mut egui::Ui,
    label: &str,
    id: &str,
    current: &mut String,
    enabled: bool,
    options: impl Fn(&mut egui::Ui, &mut String),
) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).size(12.0).weak());
        });
        ui.add_space(U * 0.25);
        ui.add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt(id)
                .selected_text(copy::finger_label(current))
                .width(COL)
                .show_ui(ui, |ui| options(ui, current));
        });
    });
}

/// Drawn, not a glyph: the default font lacks a filled dot.
fn dot_mark(ui: &mut egui::Ui, color: egui::Color32, radius: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(radius * 2.0, radius * 2.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

/// Neutral full-width action, for anything that is not the primary one.
fn half_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized(
        egui::vec2((COL - U) / 2.0, U * 5.5),
        egui::Button::new(egui::RichText::new(text)),
    )
}

fn full_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized(
        egui::vec2(COL, U * 5.5),
        egui::Button::new(egui::RichText::new(text)),
    )
}
