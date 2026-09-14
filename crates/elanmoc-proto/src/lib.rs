//! Command encoding and response parsing for the ELAN 04f3:0c90.
//!
//! Pure. Bytes in, bytes out, no I/O and no dependency on the transport crate,
//! so every function here is testable against recorded fixtures with no
//! hardware attached.
//!
//! Every byte sequence produced here corresponds to a row in `docs/protocol.md`.

use std::time::Duration;

pub use error::ProtoError;
pub use status::Status;

mod error;
mod status;

/// Which IN endpoint a command's reply arrives on.
///
/// Mirrors `elanmoc-usb::EndpointIn`. Duplicated rather than shared because
/// this crate must not depend on the transport crate. The caller maps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyEndpoint {
    /// `0x82`.
    Image,
    /// `0x83`.
    Status,
    /// `0x84`, commands that wait for a finger.
    TouchWait,
}

impl ReplyEndpoint {
    /// Wire address.
    pub fn address(self) -> u8 {
        match self {
            Self::Image => 0x82,
            Self::Status => 0x83,
            Self::TouchWait => 0x84,
        }
    }
}

/// A command from the `docs/protocol.md` table.
///
/// Read-only commands only, so far. Nothing here changes device state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `40 19`, firmware version.
    FwVersion,
    /// `00 0c`, sensor dimensions.
    SensorSize,
    /// `40 ff 04`, count of enrolled fingers.
    EnrolledNum,
    /// `40 ff 12` plus a finger id, 70 byte slot record.
    FingerInfo(u8),
    /// `40 ff 03`, wait for a touch and match it. Replies on `0x84`.
    Verify,
    /// `40 ff 02`, end the current session. Sent before releasing after an error.
    ///
    /// `docs/protocol.md` gives `in_len` 2. 0c90 sends nothing, confirmed from
    /// idle with a 5 second wait, so nothing is read back. See `findings.md`.
    Abort,
}

impl Command {
    /// The exact bytes to write.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::FwVersion => vec![0x40, 0x19],
            Self::SensorSize => vec![0x00, 0x0c],
            Self::EnrolledNum => vec![0x40, 0xff, 0x04],
            Self::FingerInfo(id) => vec![0x40, 0xff, 0x12, *id],
            Self::Verify => vec![0x40, 0xff, 0x03],
            Self::Abort => vec![0x40, 0xff, 0x02],
        }
    }

    /// Bytes to read back. There is no length field on the wire.
    pub fn expected_len(&self) -> usize {
        match self {
            Self::FwVersion | Self::EnrolledNum | Self::Verify => 2,
            Self::Abort => 0,
            Self::SensorSize => 4,
            Self::FingerInfo(_) => 70,
        }
    }

    /// Endpoint the reply arrives on.
    pub fn reply_endpoint(&self) -> ReplyEndpoint {
        match self {
            Self::FwVersion
            | Self::SensorSize
            | Self::EnrolledNum
            | Self::FingerInfo(_)
            | Self::Abort => ReplyEndpoint::Status,
            Self::Verify => ReplyEndpoint::TouchWait,
        }
    }

    /// How long to wait for the reply. Chosen, not measured.
    ///
    /// None of these wait for a finger, so a slow answer means something is
    /// wrong rather than that the user is being slow.
    pub fn timeout(&self) -> Duration {
        match self {
            Self::FwVersion | Self::SensorSize | Self::EnrolledNum | Self::Abort => {
                Duration::from_secs(1)
            }
            Self::FingerInfo(_) => Duration::from_secs(2),
            Self::Verify => Duration::from_secs(20),
        }
    }

    /// Name as it appears in `docs/protocol.md`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::FwVersion => "fw_ver",
            Self::SensorSize => "sensor_size",
            Self::EnrolledNum => "enrolled_num",
            Self::FingerInfo(_) => "finger_info",
            Self::Verify => "verify",
            Self::Abort => "abort",
        }
    }
}

/// What one slot's `finger_info` record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    /// Last byte is `0xff`: nothing enrolled here.
    Empty,
    /// Byte 1 is `0xff`: the sensor is in the stuck state the source describes.
    Stuck,
    /// A populated record.
    Enrolled {
        /// The full 70 bytes as received. The layout beyond byte 1 is not
        /// documented for 0c90, so nothing is decoded out of it yet.
        raw: Vec<u8>,
    },
    /// The two byte form, where byte 1 is an error code.
    Error(u8),
}

/// A parsed reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// Firmware version.
    FwVersion {
        /// Byte 0.
        major: u8,
        /// Byte 1.
        minor: u8,
    },
    /// Sensor dimensions, with the documented off by one already applied.
    SensorSize {
        /// `byte0 + 1`.
        width: u16,
        /// `byte2 + 1`.
        height: u16,
    },
    /// Count of enrolled fingers, from byte 1.
    EnrolledNum {
        /// Byte 1.
        count: u8,
    },
    /// Result of a touch-wait match.
    Verify {
        /// Byte 0, as received.
        byte0: u8,
        /// Byte 1 classified. `Ok(id)` is a match on that finger id,
        /// `NotEnrolled` is `0xfd`, which is a normal answer, not a failure.
        status: Status,
    },
    /// Session ended.
    Abort {
        /// Byte 1, when there is one. 0c90 replies with nothing.
        status: Option<Status>,
    },
    /// One slot record.
    FingerInfo {
        /// The id that was asked about.
        id: u8,
        /// What the record says.
        state: SlotState,
    },
}

impl Response {
    /// Parse a reply against the command that produced it.
    ///
    /// `raw` may be shorter than [`Command::expected_len`]: `finger_info` has a
    /// documented two byte error form. Anything else short is an error.
    pub fn parse(cmd: &Command, raw: &[u8]) -> Result<Self, ProtoError> {
        match cmd {
            Command::FwVersion => {
                let b = exact(cmd, raw, 2)?;
                Ok(Self::FwVersion {
                    major: b[0],
                    minor: b[1],
                })
            }
            Command::SensorSize => {
                let b = exact(cmd, raw, 4)?;
                Ok(Self::SensorSize {
                    width: u16::from(b[0]) + 1,
                    height: u16::from(b[2]) + 1,
                })
            }
            Command::EnrolledNum => {
                let b = exact(cmd, raw, 2)?;
                Ok(Self::EnrolledNum { count: b[1] })
            }
            Command::Verify => {
                let b = exact(cmd, raw, 2)?;
                Ok(Self::Verify {
                    byte0: b[0],
                    status: Status::classify(b[1]),
                })
            }
            Command::Abort => match raw.len() {
                0 => Ok(Self::Abort { status: None }),
                2 => Ok(Self::Abort {
                    status: Some(Status::classify(raw[1])),
                }),
                got => Err(ProtoError::UnexpectedLength {
                    command: cmd.name(),
                    wanted: 0,
                    got,
                }),
            },
            Command::FingerInfo(id) => Ok(Self::FingerInfo {
                id: *id,
                state: parse_slot(cmd, raw)?,
            }),
        }
    }
}

fn parse_slot(cmd: &Command, raw: &[u8]) -> Result<SlotState, ProtoError> {
    match raw.len() {
        2 => Ok(SlotState::Error(raw[1])),
        70 => {
            if raw[1] == 0xff {
                Ok(SlotState::Stuck)
            } else if raw[69] == 0xff {
                Ok(SlotState::Empty)
            } else {
                Ok(SlotState::Enrolled { raw: raw.to_vec() })
            }
        }
        got => Err(ProtoError::UnexpectedLength {
            command: cmd.name(),
            wanted: 70,
            got,
        }),
    }
}

fn exact<'a>(cmd: &Command, raw: &'a [u8], want: usize) -> Result<&'a [u8], ProtoError> {
    if raw.len() == want {
        Ok(raw)
    } else {
        Err(ProtoError::UnexpectedLength {
            command: cmd.name(),
            wanted: want,
            got: raw.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fw_version_encodes_to_the_documented_bytes() {
        assert_eq!(Command::FwVersion.encode(), vec![0x40, 0x19]);
    }

    #[test]
    fn sensor_size_encodes_to_the_documented_bytes() {
        assert_eq!(Command::SensorSize.encode(), vec![0x00, 0x0c]);
    }

    #[test]
    fn enrolled_num_encodes_to_the_documented_bytes() {
        assert_eq!(Command::EnrolledNum.encode(), vec![0x40, 0xff, 0x04]);
    }

    #[test]
    fn finger_info_appends_the_finger_id() {
        assert_eq!(
            Command::FingerInfo(3).encode(),
            vec![0x40, 0xff, 0x12, 0x03]
        );
    }

    #[test]
    fn encoded_length_matches_the_protocol_table() {
        assert_eq!(Command::FwVersion.encode().len(), 2);
        assert_eq!(Command::SensorSize.encode().len(), 2);
        assert_eq!(Command::EnrolledNum.encode().len(), 3);
        assert_eq!(Command::FingerInfo(0).encode().len(), 4);
    }

    #[test]
    fn abort_encodes_to_the_documented_bytes() {
        assert_eq!(Command::Abort.encode(), vec![0x40, 0xff, 0x02]);
    }

    #[test]
    fn abort_expects_no_reply_on_0c90() {
        assert_eq!(Command::Abort.expected_len(), 0);
        assert_eq!(
            Response::parse(&Command::Abort, &[]),
            Ok(Response::Abort { status: None })
        );
    }

    #[test]
    fn abort_still_parses_a_reply_if_one_ever_arrives() {
        assert_eq!(
            Response::parse(&Command::Abort, &[0x40, 0x00]),
            Ok(Response::Abort {
                status: Some(Status::Ok(0))
            })
        );
    }

    #[test]
    fn verify_encodes_to_the_documented_bytes() {
        assert_eq!(Command::Verify.encode(), vec![0x40, 0xff, 0x03]);
    }

    #[test]
    fn verify_replies_on_the_touch_wait_endpoint() {
        assert_eq!(Command::Verify.reply_endpoint(), ReplyEndpoint::TouchWait);
        assert_eq!(ReplyEndpoint::TouchWait.address(), 0x84);
    }

    #[test]
    fn verify_waits_far_longer_than_a_status_command() {
        assert!(Command::Verify.timeout() > Command::FwVersion.timeout());
    }

    #[test]
    fn verify_fd_is_not_enrolled_not_an_ok_status() {
        let got = Response::parse(&Command::Verify, &[0x40, 0xfd]);
        assert_eq!(
            got,
            Ok(Response::Verify {
                byte0: 0x40,
                status: Status::NotEnrolled
            })
        );
    }

    #[test]
    fn verify_low_byte_is_a_matched_finger_id() {
        let got = Response::parse(&Command::Verify, &[0x40, 0x02]);
        assert_eq!(
            got,
            Ok(Response::Verify {
                byte0: 0x40,
                status: Status::Ok(2)
            })
        );
    }

    #[test]
    fn read_only_commands_all_reply_on_status() {
        for cmd in [
            Command::FwVersion,
            Command::SensorSize,
            Command::EnrolledNum,
            Command::FingerInfo(0),
        ] {
            assert_eq!(cmd.reply_endpoint(), ReplyEndpoint::Status);
        }
    }

    #[test]
    fn status_endpoint_is_0x83() {
        assert_eq!(ReplyEndpoint::Status.address(), 0x83);
    }

    #[test]
    fn fw_version_parses_both_bytes() {
        let got = Response::parse(&Command::FwVersion, &[0x01, 0x08]);
        assert_eq!(got, Ok(Response::FwVersion { major: 1, minor: 8 }));
    }

    #[test]
    fn sensor_size_applies_the_off_by_one() {
        let got = Response::parse(&Command::SensorSize, &[0x37, 0x00, 0x77, 0x00]);
        assert_eq!(
            got,
            Ok(Response::SensorSize {
                width: 0x38,
                height: 0x78
            })
        );
    }

    #[test]
    fn enrolled_num_reads_byte_one() {
        let got = Response::parse(&Command::EnrolledNum, &[0x40, 0x02]);
        assert_eq!(got, Ok(Response::EnrolledNum { count: 2 }));
    }

    #[test]
    fn short_reply_to_fw_version_is_an_error() {
        let got = Response::parse(&Command::FwVersion, &[0x01]);
        assert!(matches!(got, Err(ProtoError::UnexpectedLength { .. })));
    }

    #[test]
    fn finger_info_last_byte_ff_means_the_slot_is_empty() {
        let mut raw = vec![0u8; 70];
        raw[69] = 0xff;
        let got = Response::parse(&Command::FingerInfo(4), &raw);
        assert_eq!(
            got,
            Ok(Response::FingerInfo {
                id: 4,
                state: SlotState::Empty
            })
        );
    }

    #[test]
    fn finger_info_byte_one_ff_means_stuck() {
        let mut raw = vec![0u8; 70];
        raw[1] = 0xff;
        let got = Response::parse(&Command::FingerInfo(0), &raw);
        assert_eq!(
            got,
            Ok(Response::FingerInfo {
                id: 0,
                state: SlotState::Stuck
            })
        );
    }

    #[test]
    fn finger_info_two_byte_form_carries_an_error_code() {
        let got = Response::parse(&Command::FingerInfo(1), &[0x40, 0xfd]);
        assert_eq!(
            got,
            Ok(Response::FingerInfo {
                id: 1,
                state: SlotState::Error(0xfd)
            })
        );
    }

    #[test]
    fn finger_info_populated_slot_keeps_all_seventy_bytes() {
        let mut raw = vec![0u8; 70];
        raw[1] = 0x00;
        raw[2] = 0xab;
        raw[69] = 0x01;
        let Ok(Response::FingerInfo {
            state: SlotState::Enrolled { raw: kept },
            ..
        }) = Response::parse(&Command::FingerInfo(2), &raw)
        else {
            panic!("expected an enrolled slot");
        };
        assert_eq!(kept, raw);
    }
}
