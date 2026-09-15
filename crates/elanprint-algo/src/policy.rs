//! How many failed attempts a login survives.

/// Mirrors the PAM `max_tries` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptPolicy {
    max_tries: u32,
}

impl AttemptPolicy {
    pub fn new(max_tries: u8) -> Self {
        Self {
            max_tries: u32::from(max_tries.max(1)),
        }
    }

    /// PAM default: three tries, then password fallback.
    pub fn pam_default() -> Self {
        Self::new(3)
    }

    pub fn allows(&self, used: u32) -> bool {
        used < self.max_tries
    }

    /// Saturates at zero.
    pub fn left(&self, used: u32) -> u32 {
        self.max_tries.saturating_sub(used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_failures_lock_out() {
        let policy = AttemptPolicy::pam_default();
        assert!(policy.allows(0));
        assert!(policy.allows(2));
        assert!(!policy.allows(3));
        assert_eq!(policy.left(1), 2);
        assert_eq!(policy.left(9), 0);
    }

    #[test]
    fn zero_max_clamps_to_one() {
        assert!(!AttemptPolicy::new(0).allows(1));
    }
}
