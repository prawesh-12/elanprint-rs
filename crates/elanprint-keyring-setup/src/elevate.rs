use anyhow::{bail, Context, Result};

pub fn provision(fingerprint_only: bool) -> Result<String> {
    let mut args: Vec<&str> = vec![HELPER, "provision"];
    if fingerprint_only {
        args.push("--fingerprint-only");
    }
    let launcher = launcher()?;
    let out = std::process::Command::new(launcher)
        .args(&args)
        .output()
        .with_context(|| format!("running {launcher}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.lines().last().unwrap_or("").trim();
        if why.is_empty() {
            bail!("setup was cancelled or failed");
        }
        bail!("{why}");
    }
    let secret = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if secret.is_empty() {
        bail!("the helper returned no secret");
    }
    Ok(secret)
}

pub fn reveal() -> Result<String> {
    one_line(&["reveal"])
}

pub fn rotate_seal() -> Result<(String, String)> {
    let out = capture(&["rotate-seal"])?;
    let mut lines = out.lines();
    match (lines.next(), lines.next()) {
        (Some(old), Some(new)) if !old.is_empty() && !new.is_empty() => {
            Ok((old.to_string(), new.to_string()))
        }
        _ => bail!("the helper did not return both secrets"),
    }
}

pub fn rollback() -> Result<()> {
    capture(&["rollback"]).map(|_| ())
}

pub fn wire_password() -> Result<()> {
    capture(&["wire-password"]).map(|_| ())
}

pub fn forget_recovery_file() -> Result<()> {
    capture(&["remove-recovery-file"]).map(|_| ())
}

fn one_line(args: &[&str]) -> Result<String> {
    let out = capture(args)?;
    if out.is_empty() {
        bail!("the helper returned nothing");
    }
    Ok(out)
}

fn capture(args: &[&str]) -> Result<String> {
    let launcher = launcher()?;
    let mut argv: Vec<&str> = vec![HELPER];
    argv.extend_from_slice(args);
    let out = std::process::Command::new(launcher)
        .args(&argv)
        .output()
        .with_context(|| format!("running {launcher}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.lines().last().unwrap_or("").trim().to_string();
        if why.is_empty() {
            bail!("cancelled, or the helper failed");
        }
        bail!("{why}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn deprovision() -> Result<()> {
    let launcher = launcher()?;
    let status = std::process::Command::new(launcher)
        .args([HELPER, "deprovision"])
        .status()
        .with_context(|| format!("running {launcher}"))?;
    if !status.success() {
        bail!("removing the PAM lines was cancelled or failed");
    }
    Ok(())
}

pub const HELPER: &str = "/usr/bin/elanprint-keyring";

/// pkexec first: a desktop gets the polkit dialog, not a dead terminal prompt.
fn launcher() -> Result<&'static str> {
    if !std::path::Path::new(HELPER).exists() {
        bail!("{HELPER} is not installed. Install the elanprint-rs package");
    }
    for candidate in ["/usr/bin/pkexec", "/usr/bin/sudo"] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate);
        }
    }
    bail!("neither pkexec nor sudo is available to raise privileges")
}
