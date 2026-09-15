use anyhow::{bail, Context, Result};

use crate::preflight::{is_keyring_auth, is_our_line, MODULE};

const LINE: &str = "auth\toptional\tpam_elanprint_keyring.so";

pub enum Change {
    Added(String),
    AlreadyPresent,
}

/// Inserts the module directly above the keyring module it feeds.
pub fn insert(text: &str) -> Result<Change> {
    if text.lines().any(is_our_line) {
        return Ok(Change::AlreadyPresent);
    }
    let Some(at) = text.lines().position(is_keyring_auth) else {
        bail!("no pam_gnome_keyring auth line to insert above");
    };
    let mut lines: Vec<&str> = text.lines().collect();
    if at > lines.len() {
        bail!("refusing to write outside the file");
    }
    lines.insert(at, LINE);
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(Change::Added(out))
}

pub fn remove(text: &str) -> Option<String> {
    if !text.lines().any(is_our_line) {
        return None;
    }
    let kept: Vec<&str> = text.lines().filter(|l| !is_our_line(l)).collect();
    let mut out = kept.join("\n");
    out.push('\n');
    Some(out)
}

pub fn backup_as_root(stack: &str) -> Result<String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = stack.rsplit('/').next().unwrap_or("pam-stack");
    let to = format!("/root/{name}.bak.{stamp}");
    std::fs::copy(stack, &to).with_context(|| format!("backing up {stack} to {to}"))?;
    Ok(to)
}

pub fn write_as_root(stack: &str, text: &str) -> Result<()> {
    std::fs::write(stack, text).with_context(|| format!("writing {stack}"))
}

pub fn module_name() -> &'static str {
    MODULE
}

#[cfg(test)]
mod tests {
    use super::*;

    const STACK: &str = "\
#%PAM-1.0
auth    requisite       pam_nologin.so
auth	required	pam_fprintd.so
auth    optional        pam_gnome_keyring.so
@include common-account
session optional        pam_gnome_keyring.so auto_start
";

    #[test]
    fn the_line_lands_directly_above_the_keyring_module() {
        let out = match insert(STACK) {
            Ok(Change::Added(text)) => text,
            _ => String::new(),
        };
        let lines: Vec<&str> = out.lines().collect();
        let ours = lines.iter().position(|l| l.contains(MODULE));
        let keyring = lines.iter().position(|l| is_keyring_auth(l));
        assert_eq!(ours.zip(keyring).map(|(a, b)| a + 1 == b), Some(true));
    }

    #[test]
    fn the_session_line_is_left_alone() {
        let out = match insert(STACK) {
            Ok(Change::Added(text)) => text,
            _ => String::new(),
        };
        assert!(out.contains("session optional        pam_gnome_keyring.so auto_start"));
        assert_eq!(out.matches(MODULE).count(), 1);
    }

    #[test]
    fn inserting_twice_changes_nothing() {
        let once = match insert(STACK) {
            Ok(Change::Added(text)) => text,
            _ => String::new(),
        };
        assert!(matches!(insert(&once), Ok(Change::AlreadyPresent)));
    }

    #[test]
    fn remove_undoes_insert() {
        let once = match insert(STACK) {
            Ok(Change::Added(text)) => text,
            _ => String::new(),
        };
        assert_eq!(remove(&once).as_deref(), Some(STACK));
    }

    #[test]
    fn remove_reports_nothing_to_do() {
        assert_eq!(remove(STACK), None);
    }

    #[test]
    fn a_stack_without_the_keyring_module_is_refused() {
        let stack = "auth required pam_fprintd.so\n";
        assert!(insert(stack).is_err());
    }
}
