//! Every user-facing string in one place.

pub const TITLE: &str = "elanprint-rs";
pub const CONNECTED: &str = "Connected";
pub const NOT_CONNECTED: &str = "Not connected";
pub const READER_NOT_FOUND: &str = "Reader not found";
pub const PICK_ENROLL: &str = "Pick a finger to enrol";
pub const PICK_VERIFY: &str = "Pick a finger to verify";
pub const TOUCH: &str = "Touch the sensor";
pub const TOUCH_FIRST: &str = "Touch the sensor";
pub const CONNECT: &str = "Connect";
pub const VERIFY_ACTION: &str = "Test my finger";
pub const TRY_AGAIN: &str = "Try again";
pub const CANCEL: &str = "Cancel";
pub const SIGNOUT: &str = "Sign out";
pub const VERIFIED: &str = "Verified";
pub const NOT_RECOGNISED: &str = "Not recognised";
pub const PASSWORD_FALLBACK: &str = "Too many attempts, use your password";
pub const SELF_TEST: &str = "Self-test only, real login is handled by PAM";
pub const START_ENROLL: &str = "Start enrol";
pub const CANCEL_ENROLL: &str = "Cancel enrol";
pub const DELETE: &str = "Delete";
pub const CONFIRM_DELETE: &str = "Confirm";
pub const ENROLLED_LIST: &str = "Enrolled fingers";
pub const NO_FINGERS: &str = "No fingers enrolled yet";

pub fn enroll_failure(result: &str) -> String {
    match result {
        "enroll-duplicate" => "That finger is already enrolled, delete it first".to_string(),
        "enroll-data-full" => "The sensor has no free slots left".to_string(),
        "enroll-disconnected" => "The reader disconnected".to_string(),
        "enroll-failed" | "enroll-unknown-error" => "Enrol failed, see log".to_string(),
        other => format!("Enrol ended: {other}"),
    }
}

pub fn touch_again(done: u8, total: u8) -> String {
    if done >= total {
        "Saving".to_string()
    } else {
        "Touch again".to_string()
    }
}

/// `any` is the protocol value. It needs saying in words.
pub fn finger_label(name: &str) -> String {
    if name == "any" {
        "any enrolled finger".to_string()
    } else {
        name.to_string()
    }
}

pub const KEYRING_ON: &str = "Keyring unlocks with your finger";
pub const KEYRING_OFF: &str = "Keyring asks for a password";
pub const KEYRING_CONFIRM: &str = "Read this before you continue";
pub const KEYRING_WORKING: &str = "Waiting for authorisation";
pub const KEYRING_SAVE_CODE: &str = "Save your recovery code";
pub const KEYRING_REKEY: &str = "Last step";
pub const KEYRING_KEY: &str = "Your recovery key";
pub const KEYRING_REPLACE: &str = "Replace the key";
pub const KEYRING_REPLACED: &str = "New key in place";
pub const KEYRING_DONE: &str = "Done, log out and back in";
