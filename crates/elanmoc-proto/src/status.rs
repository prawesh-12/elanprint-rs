/// A status byte, classified per the `docs/protocol.md` error table.
///
/// The high-nibble-zero rule is described in the source as eyeballed rather
/// than derived, so anything not in the table is [`Status::Unknown`] and gets
/// logged rather than assumed to be success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// High nibble is zero.
    Ok(u8),
    /// The user should be asked to touch again.
    Retry(Retry),
    /// `0xdd`, no slots left.
    MaxEnrolled,
    /// `0xfd`, this finger is not enrolled.
    NotEnrolled,
    /// In the table's range but not a documented value.
    Unknown(u8),
}

/// Why a sample was rejected. These are retries, not failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// `0x41`.
    MoveDown,
    /// `0x42`.
    MoveRight,
    /// `0x43`.
    MoveUp,
    /// `0x44`.
    MoveLeft,
    /// `0xfb`, sensor dirty or wet.
    Dirty,
    /// `0xfe`, finger area not enough.
    AreaTooSmall,
}

impl Status {
    /// Classify a status byte.
    pub fn classify(byte: u8) -> Self {
        match byte {
            0x41 => Self::Retry(Retry::MoveDown),
            0x42 => Self::Retry(Retry::MoveRight),
            0x43 => Self::Retry(Retry::MoveUp),
            0x44 => Self::Retry(Retry::MoveLeft),
            0xfb => Self::Retry(Retry::Dirty),
            0xfe => Self::Retry(Retry::AreaTooSmall),
            0xdd => Self::MaxEnrolled,
            0xfd => Self::NotEnrolled,
            b if b >> 4 == 0 => Self::Ok(b),
            b => Self::Unknown(b),
        }
    }

    /// Whether the operation may be retried with another touch.
    pub fn is_retry(self) -> bool {
        matches!(self, Self::Retry(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_nibble_zero_is_not_an_error() {
        assert_eq!(Status::classify(0x00), Status::Ok(0x00));
        assert_eq!(Status::classify(0x0f), Status::Ok(0x0f));
    }

    #[test]
    fn documented_retries_classify_as_retries() {
        for b in [0x41, 0x42, 0x43, 0x44, 0xfb, 0xfe] {
            assert!(Status::classify(b).is_retry(), "0x{b:02x} should retry");
        }
    }

    #[test]
    fn max_enrolled_is_not_a_retry() {
        assert_eq!(Status::classify(0xdd), Status::MaxEnrolled);
        assert!(!Status::classify(0xdd).is_retry());
    }

    #[test]
    fn finger_not_enrolled_is_its_own_case() {
        assert_eq!(Status::classify(0xfd), Status::NotEnrolled);
    }

    #[test]
    fn undocumented_high_nibble_is_unknown_not_ok() {
        assert_eq!(Status::classify(0x99), Status::Unknown(0x99));
    }
}
