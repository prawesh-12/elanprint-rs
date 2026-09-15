//! Command line front end. The app does the same thing with the same library.

use std::io::{BufRead, Write};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use elanprint_keyring_setup::{
    deprovision, elevate, forget_recovery_file, keyring, preflight, provision, reveal, rollback,
    rotate_seal, wire_password,
};
use preflight::{Cred, CRED, FINGERPRINT_STACK, PASSWORD_STACK};

#[derive(Parser)]
#[command(about = "Unlock the GNOME login keyring with a fingerprint")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// What is in place and what is not. Changes nothing
    Status,
    /// Seal a secret, wire PAM, and re-key the login keyring
    Enable {
        /// Leave password logins alone, so only fingerprint unlocks the keyring
        #[arg(long)]
        fingerprint_only: bool,
    },
    /// Take the module back out of the PAM stacks
    Disable,
    /// Root half of enable, run through pkexec. Prints the secret on stdout
    Provision {
        #[arg(long)]
        fingerprint_only: bool,
    },
    /// Root half of disable, run through pkexec
    Deprovision,
    /// Show the recovery key
    ShowKey,
    /// Replace the key with a fresh one and re-key the keyring
    Rotate,
    /// Also unlock the keyring on password logins
    AddPasswordLogin,
    /// Delete the plaintext copy of the key
    ForgetRecoveryFile,

    /// Root half of show-key
    Reveal,
    /// Root half of rotate
    RotateSeal,
    /// Root half of rotate, undoing a half finished one
    Rollback,
    /// Root half of add-password-login
    WirePassword,
    /// Root half of forget-recovery-file
    RemoveRecoveryFile,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Status => status(),
        Command::Enable { fingerprint_only } => enable(fingerprint_only),
        Command::Disable => disable(),
        Command::Provision { fingerprint_only } => {
            require_root()?;
            let (secret, wired) = provision(fingerprint_only)?;
            for w in wired {
                match w.backup {
                    Some(path) => eprintln!("{}: wired, backup at {path}", w.stack),
                    None => eprintln!("{}: already wired", w.stack),
                }
            }
            println!("{secret}");
            Ok(())
        }
        Command::ShowKey => {
            println!("{}", elevate::reveal()?);
            Ok(())
        }
        Command::Rotate => rotate(),
        Command::AddPasswordLogin => {
            elevate::wire_password()?;
            println!("password logins now unlock the keyring too.");
            Ok(())
        }
        Command::ForgetRecoveryFile => {
            elevate::forget_recovery_file()?;
            println!("removed {}", preflight::RECOVERY_FILE);
            Ok(())
        }
        Command::Reveal => {
            require_root()?;
            println!("{}", reveal()?);
            Ok(())
        }
        Command::RotateSeal => {
            require_root()?;
            let (old, new) = rotate_seal()?;
            println!("{old}");
            println!("{new}");
            Ok(())
        }
        Command::Rollback => {
            require_root()?;
            rollback()?;
            eprintln!("previous key restored");
            Ok(())
        }
        Command::WirePassword => {
            require_root()?;
            match wire_password()? {
                Some(w) => eprintln!("{}: wired, backup at {}", w.stack, w.backup.unwrap_or_default()),
                None => eprintln!("{}: already wired", PASSWORD_STACK),
            }
            Ok(())
        }
        Command::RemoveRecoveryFile => {
            require_root()?;
            forget_recovery_file()?;
            eprintln!("removed {}", preflight::RECOVERY_FILE);
            Ok(())
        }
        Command::Deprovision => {
            require_root()?;
            for w in deprovision()? {
                if let Some(path) = w.backup {
                    eprintln!("{}: unwired, backup at {path}", w.stack);
                }
            }
            Ok(())
        }
    }
}

fn status() -> Result<()> {
    let report = preflight::report();
    let yes_no = |ok: bool| if ok { "yes" } else { "no" };

    println!("TPM 2.0 usable          {}", yes_no(report.tpm));
    println!(
        "PAM module installed    {}",
        report.module.as_deref().unwrap_or("no")
    );
    println!(
        "sealed secret           {}",
        match report.cred {
            Cred::Present => "yes",
            Cred::Absent => "no",
            Cred::Unreadable => "cannot tell without root",
        }
    );
    println!("gdm-fingerprint wired   {}", yes_no(report.fingerprint_wired));
    println!("gdm-password wired      {}", yes_no(report.password_wired));
    println!("login keyring           {}", yes_no(report.keyring_present));

    if report.fingerprint_wired {
        println!();
        println!("Whether the keyring actually opens is only visible at login:");
        println!("  journalctl -b | grep gkr-pam");
    }
    Ok(())
}

fn enable(fingerprint_only: bool) -> Result<()> {
    if is_root() {
        bail!("run this as your normal user. The keyring re-key needs your session bus");
    }
    preflight::require_ready(&preflight::report())?;

    let stacks = if fingerprint_only {
        FINGERPRINT_STACK.to_string()
    } else {
        format!("{FINGERPRINT_STACK} and {PASSWORD_STACK}")
    };
    println!("This will seal 32 random bytes to the TPM at {CRED}, add the PAM");
    println!("module to {stacks}, and re-key your login keyring to that secret.");
    println!();
    println!("Afterwards the keyring opens with the TPM secret alone. Your account");
    println!("password will not open it, and nor will your fingerprint if the TPM");
    println!("stops unsealing. The recovery code is then the only way in.");
    println!();
    if !confirm("Type yes to continue: ", "yes")? {
        println!("nothing was changed.");
        return Ok(());
    }

    let secret = elevate::provision(fingerprint_only)?;
    println!();
    println!("RECOVERY CODE, save it now:");
    println!();
    println!("    {secret}");
    println!();
    if !confirm("Type the recovery code back to confirm you saved it: ", &secret)? {
        println!("That did not match. PAM is wired and the secret is sealed, but the");
        println!("keyring was not re-keyed, so nothing about your login has changed.");
        return Ok(());
    }

    let current = ask("Current login keyring password")?;
    keyring::change_password(&current, &secret).context(
        "re-keying the keyring. Your login is unaffected: the keyring simply \
         stays locked as it does today",
    )?;
    println!("done. Log out and back in with your finger, then check:");
    println!("  journalctl -b | grep gkr-pam        expect: unlocked login keyring");
    Ok(())
}

fn disable() -> Result<()> {
    let report = preflight::report();
    if !report.fingerprint_wired && !report.password_wired {
        println!("the module is in neither PAM stack, nothing to do.");
        return Ok(());
    }
    elevate::deprovision()?;
    println!("removed.");
    println!();
    println!("The keyring is still keyed to the TPM secret, so logins will prompt");
    println!("for it. To go back to your account password, change it in Passwords");
    println!("and Keys: old is the recovery code, new is your account password.");
    Ok(())
}

/// The keyring's current password is the old key, so nothing is asked for.
fn rotate() -> Result<()> {
    if is_root() {
        bail!("run this as your normal user. The keyring re-key needs your session bus");
    }
    let (old, new) = elevate::rotate_seal()?;
    match keyring::change_password(&old, &new) {
        Ok(()) => {
            println!("rotated. New recovery key, also written to {}:", preflight::RECOVERY_FILE);
            println!();
            println!("    {new}");
            Ok(())
        }
        Err(e) => {
            elevate::rollback().ok();
            bail!("the keyring refused the new key, the old one was restored: {e}")
        }
    }
}

fn require_root() -> Result<()> {
    if is_root() {
        return Ok(());
    }
    bail!("this subcommand must run as root, through pkexec or sudo")
}

fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .map(|s| s.lines().any(|l| l.starts_with("Uid:\t0\t")))
        .unwrap_or(false)
}

fn confirm(prompt: &str, expected: &str) -> Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush().context("flushing stdout")?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading stdin")?;
    Ok(line.trim() == expected)
}

/// Reads without echo through systemd-ask-password, no terminal handling here.
fn ask(prompt: &str) -> Result<String> {
    let out = std::process::Command::new("/usr/bin/systemd-ask-password")
        .args(["--no-tty", prompt])
        .output()
        .context("running systemd-ask-password")?;
    if !out.status.success() {
        bail!("could not read the password");
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\n', '\r'])
        .to_string())
}
