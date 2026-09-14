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
                let client = FingerprintClient::connect()
                    .await
                    .map_err(|e| e.to_string())?;
                let mut fingers = client.list(&user).await.map_err(|e| e.to_string())?;
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
            let _ = client.verify(&user, &finger, tx, cancel_task).await;
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
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("elanmoc login");
            ui.label("fingerprint sign-in, demo front end");
            ui.separator();

            let editing = matches!(self.screen, Screen::Disconnected | Screen::Ready);
            ui.horizontal(|ui| {
                ui.label("user");
                ui.add_enabled(editing, egui::TextEdit::singleline(&mut self.user));
            });
            ui.horizontal(|ui| {
                ui.label("finger");
                egui::ComboBox::from_id_salt("finger")
                    .selected_text(&self.finger)
                    .show_ui(ui, |ui| {
                        for name in self.fingers.clone() {
                            ui.selectable_value(&mut self.finger, name.clone(), name);
                        }
                    });
            });

            ui.separator();
            match self.screen {
                Screen::Waiting => {
                    if ui.button("cancel").clicked() {
                        self.cancel_login();
                    }
                }
                Screen::Granted => {
                    if ui.button("sign out").clicked() {
                        self.logout();
                    }
                }
                _ => {
                    if ui.button("connect").clicked() {
                        self.connect(ctx);
                    }
                    if ui.button("login with fingerprint").clicked() {
                        self.start_login();
                    }
                }
            }

            ui.separator();
            let color = match self.screen {
                Screen::Granted => egui::Color32::GREEN,
                Screen::Denied | Screen::Locked | Screen::Failed => egui::Color32::RED,
                Screen::Waiting => egui::Color32::YELLOW,
                Screen::Disconnected | Screen::Ready => egui::Color32::GRAY,
            };
            ui.colored_label(color, &self.status);
        });
        if self.events.is_some() || self.pending.is_some() {
            ctx.request_repaint();
        }
    }
}
