//! Login window: username, finger picker, touch prompt, verdict.
//!
//! A demo front end over the daemon. Granting here unlocks this window only.
//! Real session auth stays on the PAM path in Phase 7.

use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{AuthEvent, FingerprintClient};

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

/// Outcome of the background connect task.
type ConnectOutcome = Result<(Arc<FingerprintClient>, Vec<String>), String>;

/// Fingerprint login window.
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
        self.events = None;
        self.cancel = None;
        self.screen = Screen::Ready;
        self.status = "signed out".to_string();
    }

    fn poll(&mut self) {
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
                    AuthEvent::Finger(finger) => {
                        self.status = format!("verifying {finger}");
                    }
                    AuthEvent::Granted => {
                        self.screen = Screen::Granted;
                        self.status = format!("access granted for {}", self.user);
                        done = true;
                    }
                    AuthEvent::Denied(text) => {
                        self.screen = Screen::Denied;
                        self.status = text;
                        done = true;
                    }
                    AuthEvent::Locked => {
                        self.screen = Screen::Locked;
                        self.status = "no tries left, use password".to_string();
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
}

impl eframe::App for LoginApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        polish(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(18.0);
            header(ui);
            ui.add_space(10.0);
            reader_card(ui, self, ctx);
            ui.add_space(8.0);
            form_card(ui, self, ctx);
            ui.add_space(8.0);
            verdict(ui, self);
            ui.add_space(6.0);
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("demo front end, granting unlocks this window only")
                        .small()
                        .weak(),
                );
            });
        });
        if self.events.is_some() || self.pending.is_some() {
            ctx.request_repaint();
        }
    }
}

/// Larger type, roomier controls, rounded cards.
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

/// Centered mark, title and subtitle.
fn header(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        fingerprint_mark(ui, 54.0, ui.visuals().strong_text_color());
        ui.heading("elanmoc login");
        ui.label(egui::RichText::new("fingerprint sign-in").weak());
    });
}

/// Reader state with a status dot and a connect button.
fn reader_card(ui: &mut egui::Ui, app: &mut LoginApp, ctx: &egui::Context) {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            let (dot, text) = match app.screen {
                Screen::Disconnected => (egui::Color32::RED, "no reader"),
                _ => (egui::Color32::GREEN, "reader ready"),
            };
            dot_mark(ui, dot, 9.0);
            ui.label(text);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("connect").clicked() {
                    app.connect(ctx);
                }
            });
        });
        if app.pending.is_some() {
            ui.label(egui::RichText::new("connecting on the system bus").weak());
        }
    });
}

/// User and finger pickers.
fn form_card(ui: &mut egui::Ui, app: &mut LoginApp, ctx: &egui::Context) {
    card(ui, |ui| {
        let editing = matches!(app.screen, Screen::Disconnected | Screen::Ready);
        ui.horizontal(|ui| {
            ui.label("user");
            ui.add_enabled(
                editing,
                egui::TextEdit::singleline(&mut app.user).desired_width(220.0),
            );
        });
        ui.horizontal(|ui| {
            ui.label("finger");
            egui::ComboBox::from_id_salt("finger")
                .selected_text(&app.finger)
                .width(220.0)
                .show_ui(ui, |ui| {
                    for name in app.fingers.clone() {
                        ui.selectable_value(&mut app.finger, name.clone(), name);
                    }
                });
        });
        ui.add_space(4.0);
        match app.screen {
            Screen::Waiting => {
                if full_button(ui, "cancel").clicked() {
                    app.cancel_login();
                }
            }
            Screen::Granted => {
                if full_button(ui, "sign out").clicked() {
                    app.logout();
                }
            }
            _ => {
                if full_button(ui, "login with fingerprint").clicked() {
                    app.start_login();
                }
            }
        }
        let _ = ctx;
    });
}

/// Big centered verdict with a status dot.
fn verdict(ui: &mut egui::Ui, app: &LoginApp) {
    let color = match app.screen {
        Screen::Granted => egui::Color32::GREEN,
        Screen::Denied | Screen::Locked | Screen::Failed => egui::Color32::RED,
        Screen::Waiting => egui::Color32::YELLOW,
        Screen::Disconnected | Screen::Ready => egui::Color32::GRAY,
    };
    ui.vertical_centered(|ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            dot_mark(ui, color, 11.0);
            ui.label(egui::RichText::new(&app.status).size(19.0));
        });
    });
}

/// Rounded container for a screen section.
fn card(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(14.0)
        .show(ui, body);
}

/// Full-width action button.
fn full_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized(
        egui::vec2(ui.available_width(), 44.0),
        egui::Button::new(egui::RichText::new(text).strong()),
    )
}

/// Filled status dot. Drawn, not a glyph: the default font lacks ●.
fn dot_mark(ui: &mut egui::Ui, color: egui::Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(radius * 2.0, radius * 2.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

/// Fingerprint mark drawn from concentric arcs.
fn fingerprint_mark(ui: &mut egui::Ui, size: f32, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let center = rect.center() + egui::vec2(0.0, size * 0.12);
    let stroke = egui::Stroke::new(2.5, color);
    for (i, radius) in [0.16, 0.30, 0.44].iter().map(|f| f * size).enumerate() {
        let _ = i;
        ui.painter().add(egui::Shape::line(
            arc_points(center, radius, -2.4, -0.7),
            stroke,
        ));
    }
    ui.painter().circle_filled(center + egui::vec2(0.0, -size * 0.30), 3.0, color);
}

/// Points along an arc, for the mark above.
fn arc_points(center: egui::Pos2, radius: f32, from: f32, to: f32) -> Vec<egui::Pos2> {
    let mut points = Vec::with_capacity(24);
    let mut angle = from;
    while angle <= to {
        points.push(center + egui::vec2(angle.cos(), angle.sin()) * radius);
        angle += (to - from) / 24.0;
    }
    points
}
