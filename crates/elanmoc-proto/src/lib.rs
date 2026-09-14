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

/// Borrowed stage count, unconfirmed on 0c90.
pub const TOTAL_ENROLL_ATTEMPTS: u8 = 8;

/// Sub id byte for `commit`, per protocol.md.
pub fn sub_id(finger_id: u8) -> u8 {
    0xf0 | finger_id.wrapping_add(5)
}

/// A command from the `docs/protocol.md` table.
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
    /// `40 ff 01` plus slot, totals and progress. Replies on `0x84`.
    Enroll {
        finger_id: u8,
        total_attempts: u8,
        attempts_done: u8,
    },
    /// `40 ff 10`, run after the last sample. 3 byte reply.
    CheckCollision,
    /// `40 ff 11` plus 69 byte payload. Writes flash.
    Commit {
        sub_id: u8,
        user: [u8; 68],
    },
    /// `40 ff 05` plus id and zero. Erases one slot.
    Delete(u8),
    /// `40 ff 13` plus 69 byte payload. Erases one sub id.
    DeleteSubsid {
        sub_id: u8,
        tail: [u8; 68],
    },
    /// `40 ff 99`, no reply. Erases every template.
    WipeAll,
    /// `40 27 57 44 54 52 53 54`, watchdog reset, no reply.  // reset_device, per protocol.md
    ResetDevice,
    /// `00 09`, debug image read on `0x82`.
    CaptureStart {
        width: u16,
        height: u16,
    },
    /// `40` plus register selector, one value byte back.
    ReadRegister(u8),
}

impl Command {
    /// Empty user data commit for the given slot.
    pub fn commit(sub_id: u8) -> Self {
        Self::Commit { sub_id, user: [0; 68] }
    }

    /// Build `delete_subsid` from a 70 byte record.
    pub fn delete_subsid(sub_id: u8, record: &[u8]) -> Result<Self, ProtoError> {
        if record.len() != 70 {
            return Err(ProtoError::UnexpectedLength {
                command: "delete_subsid",
                wanted: 70,
                got: record.len(),
            });
        }
        let mut tail = [0u8; 68];
        tail.copy_from_slice(&record[2..70]);
        Ok(Self::DeleteSubsid { sub_id, tail })
    }

    /// The exact bytes to write.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::FwVersion => vec![0x40, 0x19],
            Self::SensorSize => vec![0x00, 0x0c],
            Self::EnrolledNum => vec![0x40, 0xff, 0x04],
            Self::FingerInfo(id) => vec![0x40, 0xff, 0x12, *id],
            Self::Verify => vec![0x40, 0xff, 0x03],
            Self::Abort => vec![0x40, 0xff, 0x02],  // abort, per protocol.md
            Self::Enroll {
                finger_id,
                total_attempts,
                attempts_done,
            } => vec![
                0x40,
                0xff,
                0x01,  // enroll, per protocol.md
                *finger_id,
                *total_attempts,
                *attempts_done,
                0x00,
            ],
            Self::CheckCollision => vec![0x40, 0xff, 0x10],
            Self::Commit { sub_id, user } => {
                let mut out = Vec::with_capacity(72);
                out.extend_from_slice(&[0x40, 0xff, 0x11]);  // commit, per protocol.md
                out.push(*sub_id);
                out.extend_from_slice(user);
                out
            }
            Self::Delete(id) => vec![0x40, 0xff, 0x05, *id, 0x00],
            Self::DeleteSubsid { sub_id, tail } => {
                let mut out = Vec::with_capacity(72);
                out.extend_from_slice(&[0x40, 0xff, 0x13]);  // delete_subsid, per protocol.md
                out.push(*sub_id);
                out.extend_from_slice(tail);
                out
            }
            Self::WipeAll => vec![0x40, 0xff, 0x99],
            Self::ResetDevice => vec![0x40, 0x27, 0x57, 0x44, 0x54, 0x52, 0x53, 0x54],
            Self::CaptureStart { .. } => vec![0x00, 0x09],
            Self::ReadRegister(reg) => vec![0x40, 0x40 + (*reg & 0x3f)],
        }
    }

    /// Bytes to read back. There is no length field on the wire.
    pub fn expected_len(&self) -> usize {
        match self {
            Self::FwVersion | Self::EnrolledNum | Self::Verify => 2,
            Self::Abort | Self::WipeAll | Self::ResetDevice => 0,
            Self::SensorSize => 4,
            Self::ReadRegister(_) => 2,
            Self::FingerInfo(_) => 70,
            Self::Enroll { .. }
            | Self::Commit { .. }
            | Self::Delete(_)
            | Self::DeleteSubsid { .. } => 2,
            Self::CheckCollision => 3,
            Self::CaptureStart { width, height } => 2 * (*width as usize) * (*height as usize),
        }
    }

    /// Endpoint the reply arrives on.
    pub fn reply_endpoint(&self) -> ReplyEndpoint {
        match self {
            Self::FwVersion
            | Self::SensorSize
            | Self::EnrolledNum
            | Self::FingerInfo(_)
            | Self::Abort
            | Self::CheckCollision
            | Self::Commit { .. }
            | Self::Delete(_)
            | Self::DeleteSubsid { .. }
            | Self::WipeAll
            | Self::ResetDevice
            | Self::ReadRegister(_) => ReplyEndpoint::Status,
            Self::Verify | Self::Enroll { .. } => ReplyEndpoint::TouchWait,
            Self::CaptureStart { .. } => ReplyEndpoint::Image,
        }
    }

    /// How long to wait for the reply. Chosen, not measured.
    pub fn timeout(&self) -> Duration {
        match self {
            Self::FwVersion | Self::SensorSize | Self::EnrolledNum | Self::Abort => {
                Duration::from_secs(1)
            }
            Self::FingerInfo(_) | Self::ReadRegister(_) => Duration::from_secs(2),
            Self::Verify => Duration::from_secs(20),
            Self::Enroll { .. } => Duration::from_secs(30),
            Self::CheckCollision => Duration::from_secs(2),
            Self::Commit { .. } | Self::Delete(_) | Self::DeleteSubsid { .. } => {
                Duration::from_secs(5)
            }
            Self::WipeAll => Duration::from_secs(10),
            Self::ResetDevice => Duration::from_secs(1),
            Self::CaptureStart { .. } => Duration::from_secs(5),
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
            Self::Enroll { .. } => "enroll",
            Self::CheckCollision => "check_enrolled_collision",
            Self::Commit { .. } => "commit",
            Self::Delete(_) => "delete",
            Self::DeleteSubsid { .. } => "delete_subsid",
            Self::WipeAll => "wipe_all",
            Self::ResetDevice => "reset_device",
            Self::CaptureStart { .. } => "capture_start",
            Self::ReadRegister(_) => "read_register",
        }
    }

    /// Whether this command changes flash. GATE required before first run.
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            Self::Enroll { .. }
                | Self::Commit { .. }
                | Self::Delete(_)
                | Self::DeleteSubsid { .. }
                | Self::WipeAll
        )
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
    /// One enroll sample. Byte 1 is 0 on a good sample.
    Enroll {
        byte0: u8,
        status: Status,
    },
    /// Post-sample collision check. Byte 2 is the clashing id.
    CheckCollision {
        byte0: u8,
        colliding: Option<u8>,
    },
    /// Flash write result. Byte 1 is 0 on success.
    Commit {
        byte0: u8,
        status: Status,
    },
    /// Single slot erase result.
    Delete {
        byte0: u8,
        status: Status,
    },
    /// Sub id erase result.
    DeleteSubsid {
        byte0: u8,
        status: Status,
    },
    /// No reply. Chip sends nothing.
    WipeAll,
    /// No reply. Device re-enumerates.
    ResetDevice,
    /// Raw image bytes, `2 * w * h` long.
    CaptureStart {
        raw: Vec<u8>,
    },
    /// Register value in byte 0, status in byte 1.
    ReadRegister {
        value: u8,
        status: Status,
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
            Command::Enroll { .. } => {
                let b = exact(cmd, raw, 2)?;
                Ok(Self::Enroll {
                    byte0: b[0],
                    status: Status::classify(b[1]),
                })
            }
            Command::CheckCollision => {
                let b = exact(cmd, raw, 3)?;
                let colliding = if b[1] != 0 { Some(b[2]) } else { None };
                Ok(Self::CheckCollision {
                    byte0: b[0],
                    colliding,
                })
            }
            Command::Commit { .. } => {
                let b = exact(cmd, raw, 2)?;
                Ok(Self::Commit {
                    byte0: b[0],
                    status: Status::classify(b[1]),
                })
            }
            Command::Delete(_) => {
                let b = exact(cmd, raw, 2)?;
                Ok(Self::Delete {
                    byte0: b[0],
                    status: Status::classify(b[1]),
                })
            }
            Command::DeleteSubsid { .. } => {
                let b = exact(cmd, raw, 2)?;
                Ok(Self::DeleteSubsid {
                    byte0: b[0],
                    status: Status::classify(b[1]),
                })
            }
            Command::WipeAll => {
                exact(cmd, raw, 0)?;
                Ok(Self::WipeAll)
            }
            Command::ResetDevice => {
                exact(cmd, raw, 0)?;
                Ok(Self::ResetDevice)
            }
            Command::CaptureStart { .. } => Ok(Self::CaptureStart { raw: raw.to_vec() }),
            Command::ReadRegister(_) => {
                let b = exact(cmd, raw, 2)?;
                Ok(Self::ReadRegister {
                    value: b[0],
                    status: Status::classify(b[1]),
                })
            }
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

    #[test]
    fn enroll_encodes_slot_and_counters() {
        let cmd = Command::Enroll {
            finger_id: 0,
            total_attempts: TOTAL_ENROLL_ATTEMPTS,
            attempts_done: 3,
        };
        assert_eq!(
            cmd.encode(),
            vec![0x40, 0xff, 0x01, 0x00, 0x08, 0x03, 0x00]
        );
        assert_eq!(cmd.encode().len(), 7);
        assert_eq!(cmd.reply_endpoint(), ReplyEndpoint::TouchWait);
    }

    #[test]
    fn enroll_zero_status_is_a_good_sample() {
        let cmd = Command::Enroll {
            finger_id: 0,
            total_attempts: 8,
            attempts_done: 0,
        };
        assert_eq!(
            Response::parse(&cmd, &[0x40, 0x00]),
            Ok(Response::Enroll {
                byte0: 0x40,
                status: Status::Ok(0)
            })
        );
    }

    #[test]
    fn enroll_dd_stops_the_loop() {
        let cmd = Command::Enroll {
            finger_id: 0,
            total_attempts: 8,
            attempts_done: 0,
        };
        assert_eq!(
            Response::parse(&cmd, &[0x40, 0xdd]),
            Ok(Response::Enroll {
                byte0: 0x40,
                status: Status::MaxEnrolled
            })
        );
    }

    #[test]
    fn collision_no_clash_gives_none() {
        assert_eq!(
            Response::parse(&Command::CheckCollision, &[0x40, 0x00, 0x00]),
            Ok(Response::CheckCollision {
                byte0: 0x40,
                colliding: None
            })
        );
    }

    #[test]
    fn collision_reports_the_clashing_id() {
        assert_eq!(
            Response::parse(&Command::CheckCollision, &[0x40, 0x01, 0x02]),
            Ok(Response::CheckCollision {
                byte0: 0x40,
                colliding: Some(0x02)
            })
        );
    }

    #[test]
    fn commit_is_72_bytes_with_sub_id_first() {
        let cmd = Command::commit(sub_id(0));
        let out = cmd.encode();
        assert_eq!(out.len(), 72);
        assert_eq!(&out[..3], &[0x40, 0xff, 0x11]);
        assert_eq!(out[3], 0xf5);
        assert!(out[4..].iter().all(|b| *b == 0));
    }

    #[test]
    fn sub_id_matches_the_documented_formula() {
        assert_eq!(sub_id(0), 0xf5);
        assert_eq!(sub_id(1), 0xf6);
    }

    #[test]
    fn commit_zero_status_is_success() {
        let cmd = Command::commit(0xf5);
        assert_eq!(
            Response::parse(&cmd, &[0x40, 0x00]),
            Ok(Response::Commit {
                byte0: 0x40,
                status: Status::Ok(0)
            })
        );
    }

    #[test]
    fn delete_encodes_id_and_zero() {
        assert_eq!(
            Command::Delete(2).encode(),
            vec![0x40, 0xff, 0x05, 0x02, 0x00]
        );
    }

    #[test]
    fn delete_subsid_takes_bytes_two_onward() {
        let mut record = vec![0u8; 70];
        record[2] = 0xab;
        let cmd = Command::delete_subsid(0xf5, &record).unwrap();
        let out = cmd.encode();
        assert_eq!(out.len(), 72);
        assert_eq!(out[3], 0xf5);
        assert_eq!(out[4], 0xab);
    }

    #[test]
    fn wipe_all_sends_three_bytes_expects_none() {
        assert_eq!(Command::WipeAll.encode(), vec![0x40, 0xff, 0x99]);
        assert_eq!(Command::WipeAll.expected_len(), 0);
        assert!(Command::WipeAll.is_destructive());
    }

    #[test]
    fn enroll_and_commit_are_destructive() {
        let enroll = Command::Enroll {
            finger_id: 0,
            total_attempts: 8,
            attempts_done: 0,
        };
        assert!(enroll.is_destructive());
        assert!(Command::commit(0xf5).is_destructive());
        assert!(!Command::Verify.is_destructive());
    }

    #[test]
    fn read_register_masks_to_sixty_four() {
        assert_eq!(Command::ReadRegister(0).encode(), vec![0x40, 0x40]);
        assert_eq!(Command::ReadRegister(63).encode(), vec![0x40, 0x7f]);
        assert_eq!(Command::ReadRegister(70).encode(), vec![0x40, 0x46]);
    }
}
