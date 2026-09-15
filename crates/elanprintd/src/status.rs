//! Chip status bytes to fprintd D-Bus strings.
//!
//! Tables live in `elanprint-algo` so the exact matches exist once.

pub use elanprint_algo::status::{enroll, enroll_failure, enroll_retry, verify, verify_status};
