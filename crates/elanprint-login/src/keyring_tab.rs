//! Fingerprint unlock of the login keyring, set up from the app.
//!
//! Privileged steps run through pkexec, so this is still no kind of
//! authentication surface. The password it asks for is the keyring's own.

use std::sync::mpsc::{channel, Receiver, TryRecvError};

use elanprint_keyring_setup::preflight::{self, Cred, Report};
use elanprint_keyring_setup::{elevate, keyring};

pub enum Stage {
    Idle,
    Confirm,
    Working(&'static str),
    Saved { secret: String, typed: String },
    Rekey { secret: String, password: String },
    ShowKey(String),
    ConfirmRotate,
    Rotated(String),
    Done,
    Failed(String),
}

enum Job {
    Provisioned(String),
    Revealed(String),
    Rotated(String),
    Wired,
    Disabled,
    Forgot,
}

pub struct KeyringTab {
    pub stage: Stage,
    pub report: Report,
    pub fingerprint_only: bool,
    pub recovery_file: bool,
    job: Option<Receiver<Result<Job, String>>>,
    rekey: Option<Receiver<Result<(), String>>>,
}

impl KeyringTab {
    pub fn new() -> Self {
        Self {
            stage: Stage::Idle,
            report: preflight::report(),
            fingerprint_only: false,
            recovery_file: recovery_file_present(),
            job: None,
            rekey: None,
        }
    }

    pub fn refresh(&mut self) {
        self.report = preflight::report();
        self.recovery_file = recovery_file_present();
    }

    pub fn busy(&self) -> bool {
        self.job.is_some() || self.rekey.is_some()
    }

    pub fn poll(&mut self) {
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(Ok(job)) => {
                    self.job = None;
                    self.refresh();
                    self.stage = match job {
                        Job::Provisioned(secret) => Stage::Saved {
                            secret,
                            typed: String::new(),
                        },
                        Job::Revealed(secret) => Stage::ShowKey(secret),
                        Job::Rotated(secret) => Stage::Rotated(secret),
                        Job::Wired | Job::Disabled | Job::Forgot => Stage::Idle,
                    };
                }
                Ok(Err(e)) => {
                    self.job = None;
                    self.refresh();
                    self.stage = Stage::Failed(e);
                }
                Err(TryRecvError::Disconnected) => {
                    self.job = None;
                    self.stage = Stage::Failed("the setup helper stopped".to_string());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.rekey {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    self.rekey = None;
                    self.refresh();
                    self.stage = Stage::Done;
                }
                Ok(Err(e)) => {
                    self.rekey = None;
                    self.stage = Stage::Failed(e);
                }
                Err(TryRecvError::Disconnected) => {
                    self.rekey = None;
                    self.stage = Stage::Failed("the re-key stopped".to_string());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    fn spawn<F>(&mut self, label: &'static str, work: F)
    where
        F: FnOnce() -> Result<Job, String> + Send + 'static,
    {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        self.job = Some(rx);
        self.stage = Stage::Working(label);
    }

    pub fn start_provision(&mut self) {
        let fingerprint_only = self.fingerprint_only;
        self.spawn("Approve the request to continue.", move || {
            elevate::provision(fingerprint_only)
                .map(Job::Provisioned)
                .map_err(|e| e.to_string())
        });
    }

    pub fn start_reveal(&mut self) {
        self.spawn("Approve the request to read the key.", || {
            elevate::reveal().map(Job::Revealed).map_err(|e| e.to_string())
        });
    }

    /// The keyring's current password is the old key, so nothing is asked for.
    pub fn start_rotate(&mut self) {
        self.spawn("Replacing the key.", || {
            let (old, new) = elevate::rotate_seal().map_err(|e| e.to_string())?;
            match keyring::change_password(&old, &new) {
                Ok(()) => Ok(Job::Rotated(new)),
                Err(e) => {
                    let restored = elevate::rollback().is_ok();
                    Err(if restored {
                        format!("the keyring refused the new key, the old one is back: {e}")
                    } else {
                        format!("the keyring refused the new key and the old one could not be restored: {e}")
                    })
                }
            }
        });
    }

    pub fn start_wire_password(&mut self) {
        self.spawn("Approve the request to change PAM.", || {
            elevate::wire_password()
                .map(|()| Job::Wired)
                .map_err(|e| e.to_string())
        });
    }

    pub fn start_disable(&mut self) {
        self.spawn("Approve the request to change PAM.", || {
            elevate::deprovision()
                .map(|()| Job::Disabled)
                .map_err(|e| e.to_string())
        });
    }

    pub fn start_forget_file(&mut self) {
        self.spawn("Approve the request to delete the file.", || {
            elevate::forget_recovery_file()
                .map(|()| Job::Forgot)
                .map_err(|e| e.to_string())
        });
    }

    pub fn start_rekey(&mut self, secret: String, password: String) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(keyring::change_password(&password, &secret).map_err(|e| e.to_string()));
        });
        self.rekey = Some(rx);
        self.stage = Stage::Working("One moment.");
    }

    /// As far as an unprivileged process can tell.
    pub fn looks_enabled(&self) -> bool {
        self.report.fingerprint_wired && self.report.cred != Cred::Absent
    }
}

/// Only root can read it, so this reports the directory entry alone.
fn recovery_file_present() -> bool {
    match std::fs::metadata(preflight::RECOVERY_FILE) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

pub fn ready_error(report: &Report) -> Option<String> {
    preflight::require_ready(report).err().map(|e| e.to_string())
}
