//! Fingerprint enrolment window over the daemon.
//!
//! Three fixed regions: header, stage, controls. The stage owns the one
//! status line. Verify is a self-test; real login is handled by PAM.

use std::sync::Arc;
use std::time::Instant;

use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{AuthEvent, EnrollEvent, FingerprintClient};
use crate::copy;
use crate::fx::{Fx, Pulse};
use crate::icon;

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
                        self.fx.ripple();
                    }
                    AuthEvent::Finger(finger) => {
                        self.status = format!("verifying {finger}");
                    }
                    AuthEvent::Granted => {
                        self.screen = Screen::Granted;
                        self.fx.ripple();
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
                        self.enroll_status = copy::touch_again(
                            self.enroll_done,
                            self.enroll_total,
                        );
                        self.enroll_tone = EnrollTone::Idle;
                        self.fx.ripple();
                    }
                    EnrollEvent::Retry(hint) => {
                        self.enroll_status = hint;
                        self.enroll_tone = EnrollTone::Hint;
                        self.fx.ripple();
                    }
                    EnrollEvent::Completed => {
                        self.enroll_done = self.enroll_total;
                        self.enroll_status = format!("{} enrolled", self.enroll_finger);
                        self.enroll_tone = EnrollTone::Done;
                        self.fx.ripple();
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
        let active = self.events.is_some() || self.enroll_events.is_some();
        let want = if active { Pulse::Breathe } else { Pulse::Off };
        if want != self.pulse {
            self.fx.set_pulse(want);
            self.pulse = want;
        }
        self.fx.set_sweep(active);
        let now = Instant::now();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.set_max_width(380.0);
                ui.add_space(14.0);
                header(ui, self);
                ui.add_space(10.0);
                stage(ui, self, now);
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);
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

fn header(ui: &mut egui::Ui, app: &LoginApp) {
    ui.vertical_centered(|ui| {
        ui.heading(copy::TITLE);
        let connected = app.client.is_some();
        let (dot, text) = if connected {
            (egui::Color32::GREEN, copy::CONNECTED)
        } else {
            (egui::Color32::GRAY, copy::NOT_CONNECTED)
        };
        ui.horizontal(|ui| {
            dot_mark(ui, dot, 5.0);
            ui.label(egui::RichText::new(text).small().weak());
        });
    });
}

/// The fingerprint mark, the progress ring and the one status line.
///
/// A ridge lights per accepted sample, a ripple fires on every chip reply,
/// a scan band runs while a touch is due.
fn stage(ui: &mut egui::Ui, app: &LoginApp, now: Instant) {
    let dimmed = match app.mode {
        Mode::Verify => matches!(app.screen, Screen::Disconnected | Screen::Locked),
        Mode::Enroll => app.client.is_none(),
    };
    let waiting = match app.mode {
        Mode::Verify => app.events.is_some(),
        Mode::Enroll => app.enroll_events.is_some(),
    };
    let good = egui::Color32::from_rgb(64, 200, 122);
    let warn = egui::Color32::from_rgb(255, 191, 0);
    let bad = egui::Color32::from_rgb(232, 86, 86);

    let base = if dimmed {
        ui.visuals().weak_text_color()
    } else {
        ui.visuals().strong_text_color()
    };
    let accent = match app.mode {
        Mode::Verify => match app.screen {
            Screen::Granted => good,
            Screen::Denied | Screen::Locked => bad,
            _ => ui.visuals().hyperlink_color,
        },
        Mode::Enroll => match app.enroll_tone {
            EnrollTone::Done => good,
            EnrollTone::Error => bad,
            EnrollTone::Hint => warn,
            EnrollTone::Idle => ui.visuals().hyperlink_color,
        },
    };

    let outcome = match app.mode {
        Mode::Verify => match app.screen {
            Screen::Granted => Some(true),
            Screen::Denied => Some(false),
            _ => None,
        },
        Mode::Enroll if app.enroll_events.is_none() => match app.enroll_tone {
            EnrollTone::Done => Some(true),
            EnrollTone::Error => Some(false),
            _ => None,
        },
        Mode::Enroll => None,
    };

    let (done, total) = match app.mode {
        Mode::Enroll => (app.enroll_done, app.enroll_total.max(1)),
        Mode::Verify => (0, 1),
    };

    let dx = app.fx.shake_dx(now);
    let size = 148.0;
    ui.vertical_centered(|ui| {
        ui.horizontal(|ui| {
            ui.add_space(dx);
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
            let inner = rect.shrink(size * 0.13);
            if app.mode == Mode::Enroll && app.client.is_some() {
                icon::ring(
                    ui,
                    rect.shrink(size * 0.03),
                    done,
                    total,
                    ui.visuals().weak_text_color().gamma_multiply(0.35),
                    accent,
                );
            }
            match outcome {
                Some(true) => icon::check(ui, inner, good),
                Some(false) => icon::cross(ui, inner, bad),
                None => {
                    let mut mark = icon::Mark::new(inner.width(), base, accent);
                    mark.lit = done.min(total);
                    mark.total = total;
                    mark.alpha = if dimmed {
                        0.5
                    } else {
                        app.fx.pulse_alpha(now)
                    };
                    mark.ripple = app.fx.ripple_now(now);
                    mark.sweep = if waiting { app.fx.sweep_now(now) } else { None };
                    draw_mark_at(ui, inner, &mark);
                }
            }
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(stage_line(app))
                .size(19.0)
                .color(match app.enroll_tone {
                    EnrollTone::Error if app.mode == Mode::Enroll => bad,
                    EnrollTone::Hint if app.mode == Mode::Enroll => warn,
                    _ => ui.visuals().text_color(),
                }),
        );
        if app.mode == Mode::Enroll && app.client.is_some() && total > 1 {
            ui.label(
                egui::RichText::new(format!("{done} of {total} touches"))
                    .small()
                    .weak(),
            );
            dots(ui, done, total);
        }
    });
}

fn draw_mark_at(ui: &mut egui::Ui, rect: egui::Rect, mark: &icon::Mark) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let _ = icon::fingerprint(&mut child, mark);
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
    }
}

fn dots(ui: &mut egui::Ui, done: u8, total: u8) {
    ui.horizontal(|ui| {
        let mut i = 0;
        while i < total {
            let color = if i < done {
                egui::Color32::GREEN
            } else {
                ui.visuals().weak_text_color()
            };
            dot_mark(ui, color, 4.0);
            i += 1;
        }
    });
}

fn controls(ui: &mut egui::Ui, app: &mut LoginApp, ctx: &egui::Context) {
    let locked = app.events.is_some() || app.enroll_events.is_some();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!locked, |ui| {
            ui.selectable_value(&mut app.mode, Mode::Verify, "Verify");
            ui.selectable_value(&mut app.mode, Mode::Enroll, "Enroll");
        });
    });
    ui.add_space(4.0);
    if app.mode == Mode::Verify {
        verify_controls(ui, app);
    } else {
        enroll_controls(ui, app, ctx);
    }
}

fn verify_controls(ui: &mut egui::Ui, app: &mut LoginApp) {
    let editing = matches!(app.screen, Screen::Disconnected | Screen::Ready);
    let armed = matches!(app.screen, Screen::Ready);
    ui.horizontal(|ui| {
        ui.label("user");
        ui.add_enabled(
            editing,
            egui::TextEdit::singleline(&mut app.user).desired_width(220.0),
        );
    });
    ui.horizontal(|ui| {
        ui.label("finger");
        ui.add_enabled_ui(editing, |ui| {
            egui::ComboBox::from_id_salt("finger")
                .selected_text(app.finger.clone())
                .width(220.0)
                .show_ui(ui, |ui| {
                    for name in app.fingers.clone() {
                        ui.selectable_value(&mut app.finger, name.clone(), name);
                    }
                });
        });
    });
    if app.fingers.len() == 1 && app.fingers.first().is_some_and(|f| f == "any") {
        ui.label(egui::RichText::new(copy::NO_FINGERS).small().weak());
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
    ui.add_space(6.0);
    ui.label(egui::RichText::new(copy::SELF_TEST).small().weak());
}

fn enroll_controls(ui: &mut egui::Ui, app: &mut LoginApp, ctx: &egui::Context) {
    if app.client.is_none() {
        if full_button(ui, copy::CONNECT).clicked() {
            app.connect(ctx);
        }
        if app.pending.is_some() {
            ui.label(egui::RichText::new("connecting").small().weak());
        }
        return;
    }
    let busy = app.enroll_events.is_some() || app.delete_pending;
    ui.horizontal(|ui| {
        ui.label("finger");
        ui.add_enabled_ui(!busy, |ui| {
            egui::ComboBox::from_id_salt("enroll_finger")
                .selected_text(app.enroll_finger.clone())
                .width(220.0)
                .show_ui(ui, |ui| {
                    for name in ENROLL_FINGERS {
                        if app.fingers.iter().any(|f| f == name) {
                            ui.add_enabled_ui(false, |ui| {
                                ui.label(format!("{name} (enrolled)"));
                            });
                        } else {
                            ui.selectable_value(&mut app.enroll_finger, name.to_string(), name);
                        }
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
        ui.add_enabled_ui(!app.delete_pending, |ui| {
            if full_button(ui, copy::START_ENROLL).clicked() {
                app.start_enroll();
            }
        });
    }
    ui.add_space(6.0);
    ui.label(copy::ENROLLED_LIST);
    let listed: Vec<String> = app
        .fingers
        .iter()
        .filter(|f| f.as_str() != "any")
        .cloned()
        .collect();
    if listed.is_empty() {
        ui.label(egui::RichText::new(copy::NO_FINGERS).small().weak());
    }
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

/// Filled status dot. Drawn, not a glyph: the default font lacks it.
fn dot_mark(ui: &mut egui::Ui, color: egui::Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(radius * 2.0, radius * 2.0),
        egui::Sense::hover(),
    );
    ui.painter().circle_filled(rect.center(), radius, color);
}

fn full_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized(
        egui::vec2(ui.available_width(), 44.0),
        egui::Button::new(egui::RichText::new(text).strong()),
    )
}
