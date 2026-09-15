use std::io::{Read, Write};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::preflight::{CRED, CRED_DIR, RECOVERY_FILE};

const SECRET_BYTES: usize = 32;
const SYSTEMD_CREDS: &str = "/usr/bin/systemd-creds";

/// Hex, so the secret can never hold a NUL or a newline the module would trim.
pub fn generate() -> Result<String> {
    let mut raw = [0u8; SECRET_BYTES];
    std::fs::File::open("/dev/urandom")
        .context("opening /dev/urandom")?
        .read_exact(&mut raw)
        .context("reading /dev/urandom")?;
    let mut hex = String::with_capacity(SECRET_BYTES * 2);
    for byte in raw {
        use std::fmt::Write as _;
        write!(hex, "{byte:02x}").context("encoding the secret")?;
    }
    Ok(hex)
}

/// PCR binding off on purpose: the PCR 7 default breaks on every dbx update.
pub fn seal_as_root(secret: &str) -> Result<()> {
    std::fs::create_dir_all(CRED_DIR).with_context(|| format!("creating {CRED_DIR}"))?;
    set_mode(CRED_DIR, 0o700)?;

    let mut child = Command::new(SYSTEMD_CREDS)
        .args(["encrypt", "--with-key=tpm2", "--tpm2-pcrs=", "-", CRED])
        .stdin(Stdio::piped())
        .spawn()
        .context("running systemd-creds encrypt")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(secret.as_bytes())
            .context("writing the secret to systemd-creds")?;
    }
    let status = child.wait().context("waiting for systemd-creds")?;
    if !status.success() {
        bail!("sealing failed: {status}");
    }
    set_mode(CRED, 0o600)
}

pub fn verify_as_root(secret: &str) -> Result<()> {
    let out = Command::new(SYSTEMD_CREDS)
        .args(["decrypt", CRED, "-"])
        .output()
        .context("running systemd-creds decrypt")?;
    if !out.status.success() {
        bail!("unsealing failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let got = String::from_utf8_lossy(&out.stdout);
    if got.trim_end_matches(['\n', '\r']) != secret {
        bail!("the unsealed secret does not match what was sealed");
    }
    Ok(())
}

pub fn unseal_as_root() -> Result<String> {
    let out = Command::new(SYSTEMD_CREDS)
        .args(["decrypt", CRED, "-"])
        .output()
        .context("running systemd-creds decrypt")?;
    if !out.status.success() {
        bail!("unsealing failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let secret = String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\n', '\r'])
        .to_string();
    if secret.is_empty() {
        bail!("the unsealed secret is empty");
    }
    Ok(secret)
}

/// Plaintext on disk, which the docs tell people to delete.
pub fn write_recovery_as_root(secret: &str) -> Result<()> {
    std::fs::write(RECOVERY_FILE, format!("{secret}\n"))
        .with_context(|| format!("writing {RECOVERY_FILE}"))?;
    set_mode(RECOVERY_FILE, 0o600)
}

fn set_mode(path: &str, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, perms).with_context(|| format!("chmod {mode:o} {path}"))
}
