//! Chip status bytes to fprintd D-Bus strings, via the shared engine.
//!
//! The tables live in `elanmoc-algo` so the exact matches exist exactly once.

pub use elanmoc_algo::status::{enroll, enroll_failure, enroll_retry, verify, verify_status};
