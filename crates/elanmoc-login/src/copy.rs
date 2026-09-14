//! Every user-facing string in one place.

pub const TITLE: &str = "elanmoc login";
pub const CONNECTED: &str = "connected";
pub const NOT_CONNECTED: &str = "not connected";
pub const READER_NOT_FOUND: &str = "reader not found";
pub const PICK_ENROLL: &str = "pick a finger to enrol";
pub const PICK_VERIFY: &str = "pick a finger to verify";
pub const TOUCH: &str = "touch the sensor";
pub const TOUCH_FIRST: &str = "touch the sensor to begin";
pub const CONNECT: &str = "connect";
pub const LOGIN: &str = "login with fingerprint";
pub const TRY_AGAIN: &str = "try again";
pub const CANCEL: &str = "cancel";
pub const SIGNOUT: &str = "sign out";
pub const VERIFIED: &str = "verified";
pub const NOT_RECOGNISED: &str = "not recognised";
pub const PASSWORD_FALLBACK: &str = "too many attempts, use your password";
pub const SELF_TEST: &str = "self-test only, real login is handled by PAM";
pub const START_ENROLL: &str = "start enroll";
pub const CANCEL_ENROLL: &str = "cancel enroll";
pub const DELETE: &str = "delete";
pub const CONFIRM_DELETE: &str = "confirm";
pub const ENROLLED_LIST: &str = "enrolled fingers";
pub const NO_FINGERS: &str = "no fingers enrolled yet";

/// A terminal enroll status as something a person can act on.
pub fn enroll_failure(result: &str) -> String {
    match result {
        "enroll-duplicate" => "that finger is already enrolled, delete it first".to_string(),
        "enroll-data-full" => "the sensor has no free slots left".to_string(),
        "enroll-disconnected" => "the reader disconnected".to_string(),
        "enroll-failed" | "enroll-unknown-error" => "enrol failed, see log".to_string(),
        other => format!("enrol ended: {other}"),
    }
}

/// What to ask for after `done` of `total` samples landed.
pub fn touch_again(done: u8, total: u8) -> String {
    if done >= total {
        "saving".to_string()
    } else {
        format!("good, touch again ({} to go)", total.saturating_sub(done))
    }
}
