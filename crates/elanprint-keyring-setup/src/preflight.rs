use std::path::Path;

use anyhow::{bail, Result};

pub const CRED_DIR: &str = "/var/lib/elanprint-rs";
pub const RECOVERY_FILE: &str = "/root/keyring-key.txt";
/// Kept when the key is rotated, so a failed re-key can be undone.
pub const CRED_PREVIOUS: &str = "/var/lib/elanprint-rs/keyring.cred.prev";
pub const CRED: &str = "/var/lib/elanprint-rs/keyring.cred";
pub const MODULE: &str = "pam_elanprint_keyring.so";
pub const FINGERPRINT_STACK: &str = "/etc/pam.d/gdm-fingerprint";
pub const PASSWORD_STACK: &str = "/etc/pam.d/gdm-password";

/// A normal user cannot stat inside 0700 root, so absent and unreadable differ.
#[derive(PartialEq)]
pub enum Cred {
    Present,
    Absent,
    Unreadable,
}

pub struct Report {
    pub tpm: bool,
    pub module: Option<String>,
    pub cred: Cred,
    pub fingerprint_wired: bool,
    pub password_wired: bool,
    pub keyring_present: bool,
}

pub fn report() -> Report {
    Report {
        tpm: tpm_usable(),
        module: module_path(),
        cred: cred_state(),
        fingerprint_wired: wired(FINGERPRINT_STACK),
        password_wired: wired(PASSWORD_STACK),
        keyring_present: login_keyring().is_some_and(|p| p.exists()),
    }
}

pub fn require_ready(report: &Report) -> Result<()> {
    if !report.tpm {
        bail!("no usable TPM 2.0: systemd-creds has-tpm2 says no");
    }
    if report.module.is_none() {
        bail!("{MODULE} is not installed. Install the elanprint-rs package first");
    }
    if !Path::new(FINGERPRINT_STACK).exists() {
        bail!("{FINGERPRINT_STACK} does not exist, so there is no fingerprint login to unlock");
    }
    if !keyring_line(FINGERPRINT_STACK)? {
        bail!("{FINGERPRINT_STACK} has no pam_gnome_keyring auth line to sit above");
    }
    if !report.keyring_present {
        bail!("no login keyring at {}", display_keyring());
    }
    Ok(())
}

pub fn cred_state() -> Cred {
    match std::fs::metadata(CRED) {
        Ok(_) => Cred::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Cred::Absent,
        Err(_) => Cred::Unreadable,
    }
}

/// Only meaningful as root: the directory is 0700.
pub fn cred_exists() -> bool {
    Path::new(CRED).exists()
}

fn tpm_usable() -> bool {
    std::process::Command::new("/usr/bin/systemd-creds")
        .arg("has-tpm2")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

pub fn module_path() -> Option<String> {
    let arch = std::process::Command::new("/usr/bin/dpkg-architecture")
        .arg("-qDEB_HOST_MULTIARCH")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|| "x86_64-linux-gnu".to_string());
    let path = format!("/usr/lib/{arch}/security/{MODULE}");
    Path::new(&path).exists().then_some(path)
}

pub fn login_keyring() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join(".local/share/keyrings/login.keyring"))
}

pub fn display_keyring() -> String {
    login_keyring()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.local/share/keyrings/login.keyring".to_string())
}

fn wired(stack: &str) -> bool {
    std::fs::read_to_string(stack)
        .map(|text| text.lines().any(is_our_line))
        .unwrap_or(false)
}

pub fn is_our_line(line: &str) -> bool {
    let line = line.trim_start();
    !line.starts_with('#') && line.contains(MODULE)
}

pub fn keyring_line(stack: &str) -> Result<bool> {
    let text = std::fs::read_to_string(stack)?;
    Ok(text.lines().any(is_keyring_auth))
}

pub fn is_keyring_auth(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }
    let mut words = trimmed.split_whitespace();
    words.next() == Some("auth") && trimmed.contains("pam_gnome_keyring.so")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keyring_auth_line_is_found_but_not_the_session_one() {
        assert!(is_keyring_auth("auth    optional        pam_gnome_keyring.so"));
        assert!(is_keyring_auth("auth\toptional\tpam_gnome_keyring.so"));
        assert!(!is_keyring_auth(
            "session optional        pam_gnome_keyring.so auto_start"
        ));
        assert!(!is_keyring_auth("# auth optional pam_gnome_keyring.so"));
        assert!(!is_keyring_auth("auth required pam_fprintd.so"));
    }

    #[test]
    fn a_commented_out_module_line_does_not_count_as_wired() {
        assert!(is_our_line("auth optional pam_elanprint_keyring.so"));
        assert!(!is_our_line("# auth optional pam_elanprint_keyring.so"));
        assert!(!is_our_line("auth optional pam_gnome_keyring.so"));
    }
}
