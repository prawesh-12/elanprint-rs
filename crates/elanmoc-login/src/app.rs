//! Login window: username, finger picker, touch prompt, verdict.
//!
//! A demo front end over the daemon. Granting here unlocks this window only.
//! Real session auth stays on the PAM path in Phase 7.

use std::sync::Arc;
use std::time::Instant;

use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{AuthEvent, EnrollEvent, FingerprintClient};
use crate::copy;
use crate::fx::{Fx, Pulse};
use crate::icon;

/// Screen state.
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

// If elanmoc-login gains an elanmoc-store dep, replace with FINGER_NAMES.
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

/// Verify or enroll side of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Verify,
    Enroll,
}

/// Enroll status tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnrollTone {
    Idle,
    Hint,
    Done,
    Error,
}

/// Outcome of the background connect task.
type ConnectOutcome = Result<(Arc<FingerprintClient>, Vec<String>), String>;

/// Fingerprint login window.
pub struct LoginApp {
    user: String,
    finger: String,
    fingers: Vec<String>,
    status: String,
    screen: Screen,
    last_screen: Screen,
    pending: Option<std::sync::mpsc::Receiver<ConnectOutcome>>,
    events: Option<mpsc::Receiver<AuthEvent>>,
    cancel: Option<CancellationToken>,
    handle: Handle,
    client: Option<Arc<FingerprintClient>>,
    fx: Fx,
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
}

impl LoginApp {
    /// Window with the current unix user prefilled.
    pub fn new(handle: Handle) -> Self {
        let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
        Self {
            user,
            finger: "any".to_string(),
            fingers: vec!["any".to_string()],
            status: "connect to the reader first".to_string(),
            screen: Screen::Disconnected,
            last_screen: Screen::Disconnected,
            pending: None,
            events: None,
            cancel: None,
            handle,
            client: None,
            fx: Fx::new(),
            mode: Mode::Verify,
            enroll_finger: ENROLL_FINGERS[6].to_string(),
            enroll_events: None,
            enroll_cancel: None,
            enroll_status: String::new(),
            enroll_tone: EnrollTone::Idle,
            enroll_done: 0,
            enroll_total: 8,
            delete_armed: None,
            delete_pending: false,
            list_pending: None,
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
                let client = FingerprintClient::connect().await.map_err(|e| e.friendly())?;
                let mut fingers = client.list(&user).await.map_err(|e| e.friendly())?;
                if !fingers.contains(&"any".to_string()) {
                    fingers.push("any".to_string());
                }
                Ok::<_, String>((Arc::new(client), fingers))
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
        self.status = "touch the sensor".to_string();
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
        self.enroll_status = format!("stage 0 of {}", self.enroll_total);
    }

    fn cancel_enroll(&mut self) {
        if let Some(cancel) = self.enroll_cancel.take() {
            cancel.cancel();
        }
        self.enroll_events = None;
        self.enroll_status = "cancelled".to_string();
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
            let outcome = handle.block_on(async {
                client.list(&user).await.map_err(|e| e.friendly())
            });
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
                    Ok((client, fingers)) => {
                        self.client = Some(client);
                        self.fingers = fingers;
                        if !self.fingers.contains(&self.finger) {
                            self.finger = "any".to_string();
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
                        self.fx.flash(egui::Color32::from_rgb(255, 191, 0));
                        self.fx.ripple();
                    }
                    AuthEvent::Finger(finger) => {
                        self.status = format!("verifying {finger}");
                        self.fx.ripple();
                        self.fx.set_pulse(Pulse::Scan);
                    }
                    AuthEvent::Granted => {
                        self.screen = Screen::Granted;
                        self.status = format!("access granted for {}", self.user);
                        self.fx.flash(egui::Color32::GREEN);
                        self.fx.set_pulse(Pulse::Off);
                        done = true;
                    }
                    AuthEvent::Denied(text) => {
                        self.screen = Screen::Denied;
                        self.status = text;
                        self.fx.flash(egui::Color32::RED);
                        self.fx.shake();
                        self.fx.set_pulse(Pulse::Off);
                        done = true;
                    }
                    AuthEvent::Locked => {
                        self.screen = Screen::Locked;
                        self.status = copy::PASSWORD_FALLBACK.to_string();
                        self.fx.set_pulse(Pulse::Off);
                        done = true;
                    }
                    AuthEvent::Failed(text) => {
                        self.screen = Screen::Failed;
                        self.status = text;
                        self.fx.set_pulse(Pulse::Off);
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
                        self.enroll_status =
                            format!("stage {} of {}", self.enroll_done, self.enroll_total);
                        self.enroll_tone = EnrollTone::Idle;
                    }
                    EnrollEvent::Retry(hint) => {
                        self.enroll_status = hint;
                        self.enroll_tone = EnrollTone::Hint;
                        self.fx.flash(egui::Color32::from_rgb(255, 191, 0));
                    }
                    EnrollEvent::Completed => {
                        self.enroll_status = "enrolled".to_string();
                        self.enroll_tone = EnrollTone::Done;
                        self.fx.flash(egui::Color32::GREEN);
                        done = true;
                        need_refresh = true;
                    }
                    EnrollEvent::Failed(text) => {
                        self.enroll_status = text;
                        self.enroll_tone = EnrollTone::Error;
                        self.fx.flash(egui::Color32::RED);
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
        polish(ctx);
        if self.screen != self.last_screen {
            match self.screen {
                Screen::Waiting => self.fx.set_pulse(Pulse::Breathe),
                Screen::Disconnected | Screen::Ready => self.fx.set_pulse(Pulse::Breathe),
                Screen::Granted | Screen::Denied | Screen::Locked | Screen::Failed => {
                    self.fx.set_pulse(Pulse::Off);
                }
            }
            self.last_screen = self.screen.clone();
        }
        let now = Instant::now();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.set_max_width(480.0);
                ui.add_space(18.0);
                header(ui, self, now);
                ui.add_space(12.0);
                reader_block(ui, self, ctx);
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);
                mode_toggle(ui, self);
                ui.add_space(10.0);
                if self.mode == Mode::Verify {
                    form_block(ui, self);
                    ui.add_space(10.0);
                    verdict(ui, self, now);
                } else {
                    enroll_block(ui, self);
                }
                ui.add_space(12.0);
                ui.label(egui::RichText::new(copy::DISCLAIMER).small().weak());
            });
        });
        if self.fx.animating(now) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else if self.events.is_some() || self.pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// Larger type, roomier controls, rounded widgets.
fn polish(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(28.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(17.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(17.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(13.5, egui::FontFamily::Proportional),
    );
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(16.0, 10.0);
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);
    ctx.set_style(style);
}

/// Centered mark with pulse, flash, ripple, and dim states.
fn header(ui: &mut egui::Ui, app: &LoginApp, now: Instant) {
    ui.vertical_centered(|ui| {
        let dimmed = matches!(app.screen, Screen::Locked | Screen::Disconnected);
        let base = if dimmed {
            ui.visuals().weak_text_color()
        } else {
            ui.visuals().strong_text_color()
        };
        let mut mark = if dimmed {
            base
        } else {
            base.gamma_multiply(app.fx.pulse_alpha(now))
        };
        if let Some(flash) = app.fx.flash_now(now) {
            mark = flash;
        }
        let response = icon::fingerprint(ui, 64.0, mark);
        if let Some((radius, alpha)) = app.fx.ripple_now(now) {
            if radius > 0.0 && alpha > 0.0 {
                ui.painter().add(egui::Shape::circle_stroke(
                    response.rect.center(),
                    radius,
                    egui::Stroke::new(2.0, mark.gamma_multiply(alpha)),
                ));
            }
        }
        ui.heading(copy::TITLE);
        ui.label(egui::RichText::new(copy::SUBTITLE).weak());
    });
}

/// Connect, pick, scan as one sequential flow.
///
/// Connection state only. The verdict below owns all status text.
fn reader_block(ui: &mut egui::Ui, app: &mut LoginApp, ctx: &egui::Context) {
    match app.screen {
        Screen::Disconnected => {
            ui.label(copy::NO_READER);
            ui.add_space(6.0);
            let label = if app.pending.is_some() {
                "connecting"
            } else {
                copy::CONNECT
            };
            let clicked = ui
                .add_sized(
                    egui::vec2(ui.available_width(), 44.0),
                    egui::Button::new(egui::RichText::new(label).strong()),
                )
                .clicked();
            if clicked {
                app.connect(ctx);
            }
        }
        Screen::Ready | Screen::Waiting => {
            ui.label("reader ready");
            ui.add_space(6.0);
            ui.label(egui::RichText::new("pick a finger, then start the scan below").weak());
        }
        Screen::Granted | Screen::Denied | Screen::Locked | Screen::Failed => {
            ui.label("reader ready");
        }
    }
}

/// Verify or enroll side switch.
fn mode_toggle(ui: &mut egui::Ui, app: &mut LoginApp) {
    let locked = app.events.is_some() || app.enroll_events.is_some();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!locked, |ui| {
            ui.selectable_value(&mut app.mode, Mode::Verify, copy::VERIFY_TAB);
            ui.selectable_value(&mut app.mode, Mode::Enroll, copy::ENROLL_TAB);
        });
    });
}

/// Enroll side: ten-name picker, stage progress, delete with confirm.
fn enroll_block(ui: &mut egui::Ui, app: &mut LoginApp) {
    let busy = app.enroll_events.is_some() || app.delete_pending;
    ui.label(copy::ENROLL_HINT);
    ui.horizontal(|ui| {
        ui.label("finger");
        ui.add_enabled_ui(!busy, |ui| {
            egui::ComboBox::from_id_salt("enroll_finger")
                .selected_text(app.enroll_finger.clone())
                .width(240.0)
                .show_ui(ui, |ui| {
                    for name in ENROLL_FINGERS {
                        ui.selectable_value(&mut app.enroll_finger, name.to_string(), name);
                    }
                });
        });
    });
    ui.add_space(6.0);
    if app.enroll_events.is_some() {
        if full_button(ui, copy::CANCEL_ENROLL).clicked() {
            app.cancel_enroll();
        }
    } else {
        ui.add_enabled_ui(app.client.is_some() && !app.delete_pending, |ui| {
            if full_button(ui, copy::START_ENROLL).clicked() {
                app.start_enroll();
            }
        });
    }
    if !app.enroll_status.is_empty() {
        let color = match app.enroll_tone {
            EnrollTone::Hint => egui::Color32::from_rgb(255, 191, 0),
            EnrollTone::Done => egui::Color32::GREEN,
            EnrollTone::Error => egui::Color32::RED,
            EnrollTone::Idle => ui.visuals().strong_text_color(),
        };
        ui.label(egui::RichText::new(&app.enroll_status).color(color));
    }
    ui.add_space(6.0);
    ui.label(copy::ENROLLED_LIST);
    let listed: Vec<String> = app
        .fingers
        .iter()
        .filter(|f| f.as_str() != "any")
        .cloned()
        .collect();
    for name in listed {
        ui.horizontal(|ui| {
            ui.label(&name);
            let armed = app.delete_armed.as_ref() == Some(&name);
            let label = if armed {
                copy::CONFIRM_DELETE
            } else {
                copy::DELETE
            };
            ui.add_enabled_ui(!busy, |ui| {
                if ui.button(label).clicked() {
                    app.request_delete(name.clone());
                }
            });
        });
    }
}

/// User and finger pickers plus the action button.
fn form_block(ui: &mut egui::Ui, app: &mut LoginApp) {
    let editing = matches!(app.screen, Screen::Disconnected | Screen::Ready);
    let armed = matches!(app.screen, Screen::Ready);
    ui.horizontal(|ui| {
        ui.label("user");
        ui.add_enabled(
            editing,
            egui::TextEdit::singleline(&mut app.user).desired_width(240.0),
        );
    });
    ui.horizontal(|ui| {
        ui.label("finger");
        ui.add_enabled_ui(editing, |ui| {
            egui::ComboBox::from_id_salt("finger")
                .selected_text(app.finger.clone())
                .width(240.0)
                .show_ui(ui, |ui| {
                    for name in app.fingers.clone() {
                        ui.selectable_value(&mut app.finger, name.clone(), name);
                    }
                });
        });
    });
    if app.fingers.len() == 1 && app.fingers.first().is_some_and(|f| f == "any") {
        ui.label(egui::RichText::new(copy::NOTHING_ENROLLED).small().weak());
    }
    ui.add_space(6.0);
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
            let retry = matches!(
                app.screen,
                Screen::Denied | Screen::Failed | Screen::Locked
            );
            ui.add_enabled_ui(armed || retry, |ui| {
                let label = if retry { copy::TRY_AGAIN } else { copy::LOGIN };
                if full_button(ui, label).clicked() {
                    app.start_login();
                }
            });
            if matches!(app.screen, Screen::Locked | Screen::Failed) {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(copy::PASSWORD_FALLBACK)
                        .small()
                        .weak(),
                );
            }
        }
    }
}

/// Centered verdict with dot, shake, and granted check.
fn verdict(ui: &mut egui::Ui, app: &LoginApp, now: Instant) {
    let color = match app.screen {
        Screen::Granted => egui::Color32::GREEN,
        Screen::Denied | Screen::Locked | Screen::Failed => egui::Color32::RED,
        Screen::Waiting => egui::Color32::YELLOW,
        Screen::Disconnected | Screen::Ready => egui::Color32::GRAY,
    };
    let dx = app.fx.shake_dx(now);
    ui.vertical_centered(|ui| {
        ui.add_space(6.0);
        if matches!(app.screen, Screen::Granted) {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            icon::check(ui, rect, color);
        } else {
            ui.horizontal(|ui| {
                ui.add_space(dx);
                dot_mark(ui, color, 11.0);
            });
        }
        ui.label(egui::RichText::new(&app.status).size(19.0));
        if matches!(app.screen, Screen::Locked) {
            ui.label(
                egui::RichText::new(copy::PASSWORD_FALLBACK)
                    .small()
                    .weak(),
            );
        }
    });
}

/// Filled status dot. Drawn, not a glyph: the default font lacks it.
fn dot_mark(ui: &mut egui::Ui, color: egui::Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(radius * 2.0, radius * 2.0),
        egui::Sense::hover(),
    );
    ui.painter().circle_filled(rect.center(), radius, color);
}

/// Full-width action button.
fn full_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized(
        egui::vec2(ui.available_width(), 44.0),
        egui::Button::new(egui::RichText::new(text).strong()),
    )
}
