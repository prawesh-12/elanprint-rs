//! Unlocks the GNOME login keyring after a fingerprint login.
//!
//! `pam_fprintd` sets no authtok, so this unseals a secret from the TPM and
//! puts it in `PAM_AUTHTOK` for `pam_gnome_keyring`, which already runs after
//! it. Every path returns `PAM_IGNORE`, so it can never fail a login.

use std::ffi::{c_char, c_int, c_void, CString};
use std::io::Read;
#[cfg(not(test))]
use std::os::unix::net::UnixDatagram;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zeroize::Zeroize;

/// From `_pam_types.h`, libpam 1.5.3.
const PAM_SUCCESS: c_int = 0;
const PAM_AUTHTOK: c_int = 6;
const PAM_IGNORE: c_int = 25;

const SYSTEMD_CREDS: &str = "/usr/bin/systemd-creds";
const CRED: &str = "/var/lib/elanprint-rs/keyring.cred";
const TIMEOUT: Duration = Duration::from_secs(2);
/// Bounded wait for a child whose stdout already closed.
const GRACE: Duration = Duration::from_millis(200);
const STDERR_CAP: u64 = 4096;
const LINE_CAP: usize = 200;

#[cfg(not(test))]
const SYSLOG: &str = "/dev/log";
/// authpriv.err
#[cfg(not(test))]
const PRIORITY: &str = "<83>";

extern "C" {
    fn pam_set_item(pamh: *mut c_void, item_type: c_int, item: *const c_void) -> c_int;
}

/// # Safety
/// The handle is passed straight back to libpam, never dereferenced here.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut c_void,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    match catch_unwind(AssertUnwindSafe(|| supply_authtok(pamh))) {
        Ok(rc) => rc,
        Err(_) => {
            log("panicked, keyring left locked");
            PAM_IGNORE
        }
    }
}

/// # Safety
/// Required of any auth module. Ignores every argument.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut c_void,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_SUCCESS
}

fn supply_authtok(pamh: *mut c_void) -> c_int {
    let Some(secret) = decrypt(SYSTEMD_CREDS, CRED, TIMEOUT) else {
        return PAM_IGNORE;
    };
    let Some(token) = to_token(secret) else {
        return PAM_IGNORE;
    };

    // Deliberate: in gdm-password this replaces the password pam_unix set,
    // because the account password no longer opens the re-keyed keyring.
    let rc = unsafe { pam_set_item(pamh, PAM_AUTHTOK, token.as_ptr().cast()) };

    let mut spent = token.into_bytes_with_nul();
    spent.zeroize();

    if rc != PAM_SUCCESS {
        log(&format!("pam_set_item returned {rc}, keyring left locked"));
    }
    PAM_IGNORE
}

/// A keyring password is a C string, so an interior NUL is unusable.
fn to_token(secret: Vec<u8>) -> Option<CString> {
    match CString::new(secret) {
        Ok(token) => Some(token),
        Err(e) => {
            let mut spent = e.into_vec();
            spent.zeroize();
            log("secret holds a NUL byte, cannot be a keyring password");
            None
        }
    }
}

fn decrypt(program: &str, cred: &str, timeout: Duration) -> Option<Vec<u8>> {
    let out = run(program, &["decrypt", cred, "-"], timeout)?;
    let out = trim_eol(out);
    if out.is_empty() {
        log("unsealed secret is empty");
        return None;
    }
    Some(out)
}

fn run(program: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            log(&format!("{program} did not start: {e}"));
            return None;
        }
    };

    let (Some(mut stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };

    let (tx_out, rx_out) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = stdout.read_to_end(&mut buf).map(|_| buf);
        let _ = tx_out.send(read);
    });

    // Drained on its own thread, or a chatty child fills the pipe and blocks.
    let (tx_err, rx_err) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.take(STDERR_CAP).read_to_end(&mut buf);
        let _ = tx_err.send(buf);
    });

    let captured = rx_out.recv_timeout(timeout);
    let status = reap(&mut child, GRACE);
    let clean = matches!(status, Some(s) if s.success());

    if !clean {
        let why = match rx_err.recv_timeout(GRACE) {
            Ok(bytes) => first_line(&bytes),
            Err(_) => String::new(),
        };
        let outcome = match status {
            Some(s) => format!("{s}"),
            None => "killed".to_string(),
        };
        if why.is_empty() {
            log(&format!("{program} {outcome}, no stderr"));
        } else {
            log(&format!("{program} {outcome}: {why}"));
        }
    }

    let mut bytes = match captured {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            log(&format!("reading {program} failed: {e}"));
            return None;
        }
        Err(_) => {
            log(&format!("{program} timed out after {timeout:?}"));
            return None;
        }
    };

    if !clean {
        bytes.zeroize();
        return None;
    }
    Some(bytes)
}

fn reap(child: &mut std::process::Child, grace: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            // stdout is already at EOF here, so exit is imminent
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn first_line(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == b'\n').unwrap_or(bytes.len());
    let head = bytes.get(..end).unwrap_or_default();
    let mut line = String::from_utf8_lossy(head).trim().to_string();
    if line.chars().count() > LINE_CAP {
        line = line.chars().take(LINE_CAP).collect();
    }
    line
}

fn trim_eol(mut bytes: Vec<u8>) -> Vec<u8> {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    bytes
}

/// Never called with anything derived from the secret.
#[cfg(not(test))]
fn log(message: &str) {
    let line = format!("{PRIORITY}pam_elanprint_keyring: {message}");
    if let Ok(socket) = UnixDatagram::unbound() {
        let _ = socket.send_to(line.as_bytes(), SYSLOG);
    }
}

/// Test failures would otherwise read like real ones in the journal.
#[cfg(test)]
fn log(_message: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_the_shipped_pam_header() {
        assert_eq!(PAM_SUCCESS, 0);
        assert_eq!(PAM_AUTHTOK, 6);
        assert_eq!(PAM_IGNORE, 25);
    }

    #[test]
    fn setcred_succeeds_or_the_auth_stack_breaks() {
        let rc = unsafe {
            pam_sm_setcred(std::ptr::null_mut(), 0, 0, std::ptr::null())
        };
        assert_eq!(rc, PAM_SUCCESS);
    }

    #[test]
    fn a_missing_helper_is_not_fatal() {
        assert_eq!(decrypt("/nonexistent/systemd-creds", CRED, TIMEOUT), None);
    }

    #[test]
    fn a_hanging_helper_is_killed_and_reported_as_nothing() {
        let start = std::time::Instant::now();
        let out = run("/usr/bin/sleep", &["30"], Duration::from_millis(150));
        assert_eq!(out, None);
        assert!(start.elapsed() < Duration::from_secs(2), "did not time out");
    }

    #[test]
    fn a_failing_helper_yields_nothing() {
        assert_eq!(run("/usr/bin/false", &[], TIMEOUT), None);
    }

    #[test]
    fn a_failure_with_stderr_still_yields_nothing() {
        let out = run("/bin/sh", &["-c", "echo sealed key unavailable >&2; exit 3"], TIMEOUT);
        assert_eq!(out, None);
    }

    #[test]
    fn stderr_reduces_to_one_bounded_line() {
        assert_eq!(first_line(b"boom\nsecond line\n"), "boom");
        assert_eq!(first_line(b"  padded  "), "padded");
        assert_eq!(first_line(b""), "");
        assert!(!first_line(&[0xff, 0xfe]).is_empty());
        assert_eq!(first_line(&vec![b'x'; 500]).chars().count(), LINE_CAP);
    }

    #[test]
    fn stdout_is_captured() {
        let out = run("/usr/bin/printf", &["opensesame"], TIMEOUT);
        assert_eq!(out.as_deref(), Some(&b"opensesame"[..]));
    }

    #[test]
    fn trailing_newlines_go_but_inner_bytes_stay() {
        assert_eq!(trim_eol(b"secret\n".to_vec()), b"secret".to_vec());
        assert_eq!(trim_eol(b"secret\r\n\n".to_vec()), b"secret".to_vec());
        assert_eq!(trim_eol(b"se\ncret".to_vec()), b"se\ncret".to_vec());
        assert_eq!(trim_eol(Vec::new()), Vec::<u8>::new());
    }

    #[test]
    fn an_interior_nul_is_refused() {
        assert!(to_token(b"one\0two".to_vec()).is_none());
    }

    #[test]
    fn a_plain_secret_becomes_a_c_string() {
        let token = to_token(b"opensesame".to_vec());
        assert_eq!(token.as_ref().map(|t| t.as_bytes()), Some(&b"opensesame"[..]));
    }
}
