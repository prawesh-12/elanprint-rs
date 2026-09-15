//! Who may act on whose fingerprints.
//!
//! The shipped `net.reactivated.Fprint.conf` lets any local user send to
//! the Device interface, which is safe for stock fprintd because it checks
//! polkit. This daemon does not, so the check lives here: root, or the
//! owner of the prints, nobody else.
//!
//! Root is allowed because `gdm-session-worker` runs as root while
//! `pam_fprintd` authenticates. That is the greeter login path.

use std::path::Path;

pub const PASSWD: &str = "/etc/passwd";

/// uid of `user`, from passwd file contents.
///
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

pub fn lookup_uid(user: &str) -> Option<u32> {
    let passwd = std::fs::read_to_string(Path::new(PASSWD)).ok()?;
    uid_for_user(&passwd, user)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD_SAMPLE: &str = "\
root:x:0:0:root:/root:/bin/bash
daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin
gdm:x:128:135:Gnome Display Manager:/var/lib/gdm3:/bin/false
prawesh:x:1000:1000:Prawesh:/home/prawesh:/usr/bin/zsh
mallory:x:1001:1001:Mallory:/home/mallory:/bin/bash
";

    #[test]
    fn uid_comes_from_the_third_field() {
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "root"), Some(0));
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "prawesh"), Some(1000));
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "mallory"), Some(1001));
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "gdm"), Some(128));
    }

    #[test]
    fn an_unknown_or_empty_name_has_no_uid() {
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "nobody-here"), None);
        assert_eq!(uid_for_user(PASSWD_SAMPLE, ""), None);
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "praw"), None);
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "prawesh2"), None);
    }

    #[test]
    fn a_name_only_matches_the_first_field() {
        // "1000" appears as uid and gid, never as an account name.
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "1000"), None);
        assert_eq!(uid_for_user(PASSWD_SAMPLE, "/home/prawesh"), None);
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
        let prawesh = 1000;
        let mallory = 1001;
        let gdm = 128;

        assert!(
            may_act(root, uid("prawesh")),
            "the greeter must still be able to act"
        );
        assert!(
            may_act(prawesh, uid("prawesh")),
            "a user deletes their own prints"
        );
        assert!(
            !may_act(mallory, uid("prawesh")),
            "another local user must not delete prawesh's prints"
        );
        assert!(
            !may_act(gdm, uid("prawesh")),
            "the gdm account itself is not root and must not delete"
        );
        assert!(
            !may_act(mallory, uid("ghost")),
            "an unknown target is never a way in"
        );
        assert!(!may_act(prawesh, uid("root")));
        assert!(may_act(root, uid("root")));
    }

    #[test]
    fn every_local_uid_is_refused_against_every_other() {
        let users = [("prawesh", 1000u32), ("mallory", 1001), ("gdm", 128)];
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
