/// Eyeballed: high nibble zero is Ok, else Unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok(u8),
    Retry(Retry),
    MaxEnrolled,
    NotEnrolled,
    Unknown(u8),
}

/// Rejected sample; retry touch, not failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    MoveDown,
    MoveRight,
    MoveUp,
    MoveLeft,
    Dirty,
    AreaTooSmall,
}

impl Status {
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

    pub fn is_retry(self) -> bool {
        matches!(self, Self::Retry(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    Match(u8),
    /// Unknown finger, not capture failure.
    NoMatch,
    Retry(Retry),
}

impl VerifyOutcome {
    pub fn classify(status: Status) -> Result<Self, crate::ProtoError> {
        match status {
            Status::Ok(id) => Ok(Self::Match(id)),
            Status::NotEnrolled => Ok(Self::NoMatch),
            Status::Retry(r) => Ok(Self::Retry(r)),
            Status::MaxEnrolled => Err(crate::ProtoError::Device {
                command: "verify",
                code: 0xdd,
            }),
            Status::Unknown(b) => Err(crate::ProtoError::Device {
                command: "verify",
                code: b,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_range() {
        assert_eq!(Status::classify(0x00), Status::Ok(0x00));
        assert_eq!(Status::classify(0x0f), Status::Ok(0x0f));
    }

    #[test]
    fn retries_retry() {
        for b in [0x41, 0x42, 0x43, 0x44, 0xfb, 0xfe] {
            assert!(Status::classify(b).is_retry(), "not retry 0x{b:02x}");
        }
    }

    #[test]
    fn max_enrolled() {
        assert_eq!(Status::classify(0xdd), Status::MaxEnrolled);
        assert!(!Status::classify(0xdd).is_retry());
    }

    #[test]
    fn not_enrolled() {
        assert_eq!(Status::classify(0xfd), Status::NotEnrolled);
    }

    #[test]
    fn unknown_byte() {
        assert_eq!(Status::classify(0x99), Status::Unknown(0x99));
    }

    #[test]
    fn verify_match() {
        assert_eq!(
            VerifyOutcome::classify(Status::Ok(2)),
            Ok(VerifyOutcome::Match(2))
        );
    }

    #[test]
    fn verify_no_match() {
        assert_eq!(
            VerifyOutcome::classify(Status::NotEnrolled),
            Ok(VerifyOutcome::NoMatch)
        );
    }

    #[test]
    fn verify_retry() {
        assert_eq!(
            VerifyOutcome::classify(Status::Retry(Retry::Dirty)),
            Ok(VerifyOutcome::Retry(Retry::Dirty))
        );
    }

    #[test]
    fn verify_max_enrolled_err() {
        assert!(VerifyOutcome::classify(Status::MaxEnrolled).is_err());
    }
}
