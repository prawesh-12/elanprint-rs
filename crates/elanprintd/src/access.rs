//! Who may act on whose fingerprints.
//!
//! The shipped `net.reactivated.Fprint.conf` lets any local user reach the
//! Device interface, which is safe for stock fprintd because it checks
//! polkit. This daemon does not, so the check lives here.
//!
//! Root is allowed: `gdm-session-worker` runs as root during `pam_fprintd`,
//! which is the greeter login path.

use std::time::Duration;

/// Reads `/etc/nsswitch.conf`, so LDAP, SSSD and systemd-homed resolve.
const GETENT: &str = "/usr/bin/getent";

/// A stuck NSS backend must not wedge the auth path.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Fields are `name:password:uid:gid:gecos:home:shell`.
pub fn uid_for_user(passwd: &str, user: &str) -> Option<u32> {
    if user.is_empty() {
        return None;
    }
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.next()? != user {
            return None;
        }
        let _password = fields.next()?;
        fields.next()?.trim().parse().ok()
    })
}

/// Whether `caller` may read or change `target`'s fingerprints.
///
/// An unknown target is refused for everyone but root, so a typo or a
/// deleted account cannot widen access.
pub fn may_act(caller: u32, target: Option<u32>) -> bool {
    if caller == 0 {
        return true;
    }
    target == Some(caller)
}

/// uid of `user` through NSS.
///
/// Reading `/etc/passwd` directly refuses anyone whose account is not in it.
pub async fn lookup_uid(user: &str) -> Option<u32> {
    if user.is_empty() || user.contains(':') || user.contains('\n') {
        return None;
    }
    let run = tokio::process::Command::new(GETENT)
        .arg("passwd")
        .arg(user)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let out = match tokio::time::timeout(LOOKUP_TIMEOUT, run).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            tracing::warn!("cannot run {GETENT}: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("NSS lookup for {user} timed out, a backend may be down");
            return None;
        }
    };
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8(out.stdout).ok()?;
    uid_for_user(&line, user)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD_SAMPLE: &str = "\
root:x:0:0:root:/root:/bin/bash
daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin
gdm:x:128:135:Gnome Display Manager:/var/lib/gdm3:/bin/false
alice:x:1000:1000:Alice:/home/alice:/bin/bash
mallory:x:1001:1001:Mallory:/home/mallory:/bin/bash
";

    #[test]
    fn uid_comes_from_the_third_field() {
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "root"), Some(0));
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "alice"), Some(1000));
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "mallory"), Some(1001));
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "gdm"), Some(128));
    }

    #[test]
    fn an_unknown_or_empty_name_has_no_uid() {
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "nobody-here"), None);
        assert_eq!(uid_for_user(PASSWD_SAMPLE, ""), None);
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "praw"), None);
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "alice2"), None);
    }

    #[test]
    fn a_name_only_matches_the_first_field() {
        // "1000" appears as uid and gid, never as an account name.
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "1000"), None);
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "/home/alice"), None);
    }

    #[test]
    fn root_may_act_on_anyone() {
        assert!(may_act(0, Some(1000)));
        assert!(may_act(0, Some(0)));
        assert!(may_act(0, None));
    }

    #[test]
    fn a_user_may_act_on_themselves_only() {
        assert!(may_act(1000, Some(1000)));
        assert!(!may_act(1000, Some(1001)));
        assert!(!may_act(1001, Some(1000)));
        assert!(!may_act(1000, Some(0)));
    }

    #[test]
    fn an_unknown_target_is_refused_for_everyone_but_root() {
        assert!(!may_act(1000, None));
        assert!(!may_act(1001, None));
        assert!(may_act(0, None));
    }

    /// `DeleteEnrolledFingers` erased the slot 0 template once.
    #[test]
    fn nobody_but_root_and_the_owner_can_reach_a_delete() {
        let uid = |name: &str| uid_for_user(PASSWD_SAMPLE, name);
        let root = 0;
        let alice = 1000;
        let mallory = 1001;
        let gdm = 128;

        assert!(
            may_act(root, uid("alice")),
            "the greeter must still be able to act"
        );
        assert!(
            may_act(alice, uid("alice")),
            "a user deletes their own prints"
        );
        assert!(
            !may_act(mallory, uid("alice")),
            "another local user must not delete alice's prints"
        );
        assert!(
            !may_act(gdm, uid("alice")),
            "the gdm account itself is not root and must not delete"
        );
        assert!(
            !may_act(mallory, uid("ghost")),
            "an unknown target is never a way in"
        );
        assert!(!may_act(alice, uid("root")));
        assert!(may_act(root, uid("root")));
    }

    #[test]
    fn every_local_uid_is_refused_against_every_other() {
        let users = [("alice", 1000u32), ("mallory", 1001), ("gdm", 128)];
        for (_, caller) in users {
            for (target_name, target) in users {
                let allowed = may_act(caller, uid_for_user(PASSWD_SAMPLE, target_name));
                assert_eq!(
                    allowed,
                    caller == target,
                    "uid {caller} acting on {target_name} ({target})"
                );
            }
        }
    }
}

#[cfg(test)]
mod nss_tests {
    use super::*;

    /// Root exists in every NSS configuration.
    #[tokio::test]
    async fn nss_resolves_root_to_zero() {
        assert_eq!(lookup_uid("root").await, Some(0));
    }

    #[tokio::test]
    async fn nss_refuses_a_missing_account() {
        assert_eq!(lookup_uid("elanprint-no-such-account").await, None);
    }

    #[tokio::test]
    async fn nss_refuses_a_malformed_name() {
        assert_eq!(lookup_uid("").await, None);
        assert_eq!(lookup_uid("a:b").await, None);
        assert_eq!(lookup_uid("a\nroot").await, None);
    }
}

#[cfg(test)]
mod unit_tests {
    use std::path::PathBuf;

    fn unit() -> String {
        let p: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "systemd", "elanprintd.service"]
            .iter()
            .collect();
        match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => panic!("cannot read {}: {e}", p.display()),
        }
    }

    /// The shipped unit must not sandbox the NSS lookup out of existence.
    ///
    /// `lookup_uid` spawns `/usr/bin/getent`, which dlopens NSS modules. Each
    /// directive below breaks that, and the symptom looks like an
    /// access-control bug rather than sandboxing.
    #[test]
    fn the_unit_can_still_spawn_getent() {
        let text = unit();
        let blocking = [
            ("DynamicUser", "the daemon would not resolve real accounts"),
            ("PrivateUsers", "NSS uid mapping breaks inside a user namespace"),
            ("MemoryDenyWriteExecute", "NSS modules are dlopened"),
            ("NoExecPaths", "getent could not be executed"),
            ("SystemCallFilter", "exec and clone must stay allowed"),
            ("RestrictNamespaces", "some NSS backends need namespaces"),
            ("ProtectHome", "systemd-homed accounts resolve through it"),
        ];
        for (directive, why) in blocking {
            assert!(
                !text.lines().any(|l| l.trim_start().starts_with(directive)),
                "{directive} was added to elanprintd.service: {why}. \
                 Prove `getent passwd <user>` still works from the unit, \
                 then allow it here"
            );
        }
    }

    #[test]
    fn the_unit_sets_the_store_path() {
        assert!(unit().contains("ELANPRINT_STORE=/var/lib/elanprint/prints.json"));
    }
}
