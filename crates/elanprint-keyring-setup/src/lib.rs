//! Fingerprint unlock of the GNOME login keyring.
//!
//! Split by privilege: [`provision`] and friends need root and run through
//! `pkexec`, [`keyring`] needs the user's session bus.

pub mod elevate;
pub mod keyring;
pub mod pam;
pub mod preflight;
pub mod seal;

use anyhow::{bail, Context, Result};

use preflight::{CRED, CRED_PREVIOUS, FINGERPRINT_STACK, PASSWORD_STACK, RECOVERY_FILE};

pub struct Wired {
    pub stack: String,
    pub backup: Option<String>,
}

/// Nothing is wired until the seal has round tripped.
pub fn provision(fingerprint_only: bool) -> Result<(String, Vec<Wired>)> {
    if preflight::cred_exists() {
        bail!(
            "{} already exists. Sealing over it would leave the keyring locked \
             to a secret nothing holds any more",
            preflight::CRED
        );
    }

    let secret = seal::generate()?;
    seal::seal_as_root(&secret).context("sealing the secret to the TPM")?;
    seal::verify_as_root(&secret).context("the sealed secret did not round trip")?;
    seal::write_recovery_as_root(&secret).context("writing the recovery file")?;

    let stacks: &[&str] = if fingerprint_only {
        &[FINGERPRINT_STACK]
    } else {
        &[FINGERPRINT_STACK, PASSWORD_STACK]
    };

    let mut wired = Vec::new();
    for stack in stacks {
        let Ok(text) = std::fs::read_to_string(stack) else {
            continue;
        };
        match pam::insert(&text)? {
            pam::Change::AlreadyPresent => wired.push(Wired {
                stack: (*stack).to_string(),
                backup: None,
            }),
            pam::Change::Added(updated) => {
                let backup = pam::backup_as_root(stack)?;
                pam::write_as_root(stack, &updated)?;
                wired.push(Wired {
                    stack: (*stack).to_string(),
                    backup: Some(backup),
                });
            }
        }
    }
    Ok((secret, wired))
}

/// The keyring keeps its current password.
pub fn deprovision() -> Result<Vec<Wired>> {
    let mut undone = Vec::new();
    for stack in [FINGERPRINT_STACK, PASSWORD_STACK] {
        let Ok(text) = std::fs::read_to_string(stack) else {
            continue;
        };
        let Some(updated) = pam::remove(&text) else {
            continue;
        };
        let backup = pam::backup_as_root(stack)?;
        pam::write_as_root(stack, &updated)?;
        undone.push(Wired {
            stack: stack.to_string(),
            backup: Some(backup),
        });
    }
    Ok(undone)
}

pub fn reveal() -> Result<String> {
    seal::unseal_as_root()
}

/// Keeps the old blob, so a failed re-key can be undone.
pub fn rotate_seal() -> Result<(String, String)> {
    let old = seal::unseal_as_root().context("reading the current secret")?;
    let new = seal::generate()?;
    std::fs::copy(CRED, CRED_PREVIOUS)
        .with_context(|| format!("keeping the old secret at {CRED_PREVIOUS}"))?;
    seal::seal_as_root(&new).context("sealing the new secret")?;
    seal::verify_as_root(&new).context("the new secret did not round trip")?;
    seal::write_recovery_as_root(&new).context("writing the recovery file")?;
    Ok((old, new))
}

pub fn rollback() -> Result<()> {
    if !std::path::Path::new(CRED_PREVIOUS).exists() {
        bail!("no previous secret to restore");
    }
    std::fs::copy(CRED_PREVIOUS, CRED).context("restoring the previous secret")?;
    let secret = seal::unseal_as_root()?;
    seal::write_recovery_as_root(&secret)?;
    Ok(())
}

pub fn wire_password() -> Result<Option<Wired>> {
    let text = std::fs::read_to_string(PASSWORD_STACK)
        .with_context(|| format!("reading {PASSWORD_STACK}"))?;
    match pam::insert(&text)? {
        pam::Change::AlreadyPresent => Ok(None),
        pam::Change::Added(updated) => {
            let backup = pam::backup_as_root(PASSWORD_STACK)?;
            pam::write_as_root(PASSWORD_STACK, &updated)?;
            Ok(Some(Wired {
                stack: PASSWORD_STACK.to_string(),
                backup: Some(backup),
            }))
        }
    }
}

/// The sealed secret is untouched.
pub fn forget_recovery_file() -> Result<()> {
    match std::fs::remove_file(RECOVERY_FILE) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {RECOVERY_FILE}")),
    }
}
