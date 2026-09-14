//! Chip status bytes to fprintd D-Bus strings.
//!
//! Every string here comes from `docs/dbus-device.xml`, captured from the
//! installed fprintd 1.94 interface files on this machine. The names in
//! `plan.md` 6.3 do not exist there (`enroll-retry-center-finger` and friends
//! are absent), so the live XML wins and the plan list is not used.

use elanmoc_proto::{EnrollError, Retry, Status, VerifyOutcome};

/// D-Bus enroll status strings, live from fprintd 1.94.
///
/// Only strings this daemon emits are defined here. The full live list is
/// `docs/dbus-device.xml`, which stays the source for anything added later.
pub mod enroll {
    pub const COMPLETED: &str = "enroll-completed";
    pub const FAILED: &str = "enroll-failed";
    pub const STAGE_PASSED: &str = "enroll-stage-passed";
    pub const SWIPE_TOO_SHORT: &str = "enroll-swipe-too-short";
    pub const NOT_CENTERED: &str = "enroll-finger-not-centered";
    pub const REMOVE_AND_RETRY: &str = "enroll-remove-and-retry";
    pub const DATA_FULL: &str = "enroll-data-full";
    pub const DUPLICATE: &str = "enroll-duplicate";
    pub const DISCONNECTED: &str = "enroll-disconnected";
    pub const UNKNOWN_ERROR: &str = "enroll-unknown-error";
}

/// D-Bus verify status strings, live from fprintd 1.94.
///
/// Only strings this daemon emits are defined here. The full live list is
/// `docs/dbus-device.xml`, which stays the source for anything added later.
pub mod verify {
    pub const MATCH: &str = "verify-match";
    pub const NO_MATCH: &str = "verify-no-match";
    pub const SWIPE_TOO_SHORT: &str = "verify-swipe-too-short";
    pub const NOT_CENTERED: &str = "verify-finger-not-centered";
    pub const REMOVE_AND_RETRY: &str = "verify-remove-and-retry";
    pub const DISCONNECTED: &str = "verify-disconnected";
    pub const UNKNOWN_ERROR: &str = "verify-unknown-error";
}

/// A retry reason as an enroll status string.
pub fn enroll_retry(r: Retry) -> &'static str {
    match r {
        Retry::MoveDown | Retry::MoveRight | Retry::MoveUp | Retry::MoveLeft => {
            enroll::NOT_CENTERED
        }
        Retry::Dirty => enroll::REMOVE_AND_RETRY,
        Retry::AreaTooSmall => enroll::SWIPE_TOO_SHORT,
    }
}

/// A retry reason as a verify status string.
pub fn verify_retry(r: Retry) -> &'static str {
    match r {
        Retry::MoveDown | Retry::MoveRight | Retry::MoveUp | Retry::MoveLeft => {
            verify::NOT_CENTERED
        }
        Retry::Dirty => verify::REMOVE_AND_RETRY,
        Retry::AreaTooSmall => verify::SWIPE_TOO_SHORT,
    }
}

/// A verify answer as a D-Bus string. Retries are not terminal.
pub fn verify_status(status: Status) -> Option<&'static str> {
    match VerifyOutcome::classify(status) {
        Ok(VerifyOutcome::Match(_)) => Some(verify::MATCH),
        Ok(VerifyOutcome::NoMatch) => Some(verify::NO_MATCH),
        Ok(VerifyOutcome::Retry(r)) => Some(verify_retry(r)),
        Err(_) => Some(verify::UNKNOWN_ERROR),
    }
}

/// An enroll failure as a terminal D-Bus string.
pub fn enroll_failure(e: &EnrollError) -> &'static str {
    match e {
        EnrollError::MaxEnrolled => enroll::DATA_FULL,
        EnrollError::Collision(_) => enroll::DUPLICATE,
        EnrollError::Parse(_) => enroll::UNKNOWN_ERROR,
        EnrollError::Device { .. } | EnrollError::UnexpectedReply => enroll::FAILED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_hints_mean_not_centered() {
        assert_eq!(enroll_retry(Retry::MoveDown), enroll::NOT_CENTERED);
        assert_eq!(verify_retry(Retry::MoveLeft), verify::NOT_CENTERED);
    }

    #[test]
    fn dirty_means_remove_and_retry() {
        assert_eq!(enroll_retry(Retry::Dirty), enroll::REMOVE_AND_RETRY);
        assert_eq!(verify_retry(Retry::Dirty), verify::REMOVE_AND_RETRY);
    }

    #[test]
    fn small_area_is_a_short_press() {
        assert_eq!(enroll_retry(Retry::AreaTooSmall), enroll::SWIPE_TOO_SHORT);
        assert_eq!(verify_retry(Retry::AreaTooSmall), verify::SWIPE_TOO_SHORT);
    }

    #[test]
    fn fd_is_no_match() {
        assert_eq!(verify_status(Status::NotEnrolled), Some(verify::NO_MATCH));
    }

    #[test]
    fn slot_limit_is_data_full() {
        assert_eq!(enroll_failure(&EnrollError::MaxEnrolled), enroll::DATA_FULL);
    }

    #[test]
    fn collision_is_duplicate() {
        assert_eq!(
            enroll_failure(&EnrollError::Collision(2)),
            enroll::DUPLICATE
        );
    }

    #[test]
    fn strings_come_from_the_live_xml() {
        assert_eq!(enroll::STAGE_PASSED, "enroll-stage-passed");
        assert_eq!(enroll::COMPLETED, "enroll-completed");
        assert_eq!(enroll::DISCONNECTED, "enroll-disconnected");
        assert_eq!(enroll::UNKNOWN_ERROR, "enroll-unknown-error");
        assert_eq!(verify::MATCH, "verify-match");
        assert_eq!(verify::DISCONNECTED, "verify-disconnected");
        assert_eq!(verify::UNKNOWN_ERROR, "verify-unknown-error");
    }
}
