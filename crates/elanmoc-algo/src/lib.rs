//! Login decision engine for fingerprint authentication.
//!
//! Pure. No I/O, no D-Bus, no USB. It consumes status strings from the daemon
//! and answers what the login screen must do. Every branch is unit tested, so
//! the grant path can be reviewed without hardware.
//!
//! The string tables are canonical here. `elanmocd` reuses them, so the exact
//! matches exist exactly once. Source for every value is
//! `docs/dbus-device.xml`.

pub use policy::AttemptPolicy;
pub use session::{Action, SessionState, Signal, VerifySession};
pub use status::{enroll_failure, enroll_retry, verify_retry, verify_status};

pub mod status;

mod policy;
mod session;
