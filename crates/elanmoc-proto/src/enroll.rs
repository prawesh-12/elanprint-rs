use std::time::Duration;

use crate::{Command, ProtoError, Response, Retry, Status, TOTAL_ENROLL_ATTEMPTS};

/// What the enroll loop can fail with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnrollError {
    #[error("slot limit reached (0xdd)")]
    MaxEnrolled,
    #[error("collides with finger id {0}")]
    Collision(u8),
    #[error("{command}: device returned status 0x{code:02x}")]
    Device {
        command: &'static str,
        code: u8,
    },
    #[error("unexpected reply shape")]
    UnexpectedReply,
    #[error(transparent)]
    Parse(#[from] ProtoError),
}

/// Visible enroll progress, per plan.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollState {
    Idle,
    Started {
        slot: u8,
    },
    AwaitingTouch {
        collected: u8,
        needed: u8,
    },
    Committing,
    Done {
        template_id: u8,
    },
    Failed,
}

/// Next step for the caller. The caller sends `Send` bytes and feeds the
/// reply back into [`Enroll::step`]. After `EmitProgress` or `EmitRetry` the
/// caller collects the pending resend with [`Enroll::take_send`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollAction {
    Send(Vec<u8>),
    EmitProgress {
        done: u8,
        total: u8,
    },
    EmitRetry(Retry),
    Complete(u8),
    Fail(EnrollError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Sample,
    Collision,
    Commit,
}

/// Pure enroll driver. No I/O, no timeouts, no endpoint knowledge.
pub struct Enroll {
    slot: u8,
    needed: u8,
    collected: u8,
    pending: Option<Pending>,
    send_buf: Option<Vec<u8>>,
    state: EnrollState,
}

impl Enroll {
    /// Begin enrolling `slot`, expecting the shared stage count.
    pub fn new(slot: u8) -> Self {
        Self::with_total(slot, TOTAL_ENROLL_ATTEMPTS)
    }

    /// Same, with an explicit stage count for replay tests.
    pub fn with_total(slot: u8, total: u8) -> Self {
        Self {
            slot,
            needed: total,
            collected: 0,
            pending: None,
            send_buf: None,
            state: EnrollState::Idle,
        }
    }

    /// Current state, for asserts and for D-Bus progress signals.
    pub fn state(&self) -> EnrollState {
        self.state
    }

    /// Samples collected so far.
    pub fn collected(&self) -> u8 {
        self.collected
    }

    /// Timeout the caller should use for the pending read.
    pub fn pending_timeout(&self) -> Option<Duration> {
        self.pending_command().map(|c| c.timeout())
    }

    /// Endpoint, reply length and timeout for the queued command.
    ///
    /// The machine knows which command it just queued, so the caller never
    /// has to guess from the outgoing bytes. Reading a sample's endpoint for
    /// the collision check costs a full touch timeout and no commit.
    pub fn pending_read(&self) -> Option<(crate::ReplyEndpoint, usize, Duration)> {
        self.pending_command()
            .map(|c| (c.reply_endpoint(), c.expected_len(), c.timeout()))
    }

    fn pending_command(&self) -> Option<Command> {
        match self.pending {
            Some(Pending::Sample) => Some(Command::Enroll {
                finger_id: self.slot,
                total_attempts: self.needed,
                attempts_done: self.collected,
            }),
            Some(Pending::Collision) => Some(Command::CheckCollision),
            Some(Pending::Commit) => Some(Command::commit(crate::sub_id(self.slot))),
            None => None,
        }
    }

    fn queue(&mut self, cmd: Command) -> EnrollAction {
        let out = cmd.encode();
        self.send_buf = Some(out.clone());
        EnrollAction::Send(out)
    }

    /// Move out of `Idle`. Returns the first sample command.
    pub fn start(&mut self) -> EnrollAction {
        let cmd = Command::Enroll {
            finger_id: self.slot,
            total_attempts: self.needed,
            attempts_done: 0,
        };
        self.pending = Some(Pending::Sample);
        self.state = EnrollState::Started { slot: self.slot };
        self.queue(cmd)
    }

    /// Collect the resend after `EmitProgress` or `EmitRetry`.
    pub fn take_send(&mut self) -> Option<EnrollAction> {
        self.send_buf.take().map(EnrollAction::Send)
    }

    /// Feed raw reply bytes for the pending command.
    pub fn step(&mut self, input: &[u8]) -> EnrollAction {
        match self.pending {
            Some(Pending::Sample) => self.step_sample(input),
            Some(Pending::Collision) => self.step_collision(input),
            Some(Pending::Commit) => self.step_commit(input),
            None => self.fail(EnrollError::UnexpectedReply),
        }
    }

    fn step_sample(&mut self, input: &[u8]) -> EnrollAction {
        let cmd = Command::Enroll {
            finger_id: self.slot,
            total_attempts: self.needed,
            attempts_done: self.collected,
        };
        let parsed = match Response::parse(&cmd, input) {
            Ok(Response::Enroll { status, .. }) => status,
            Ok(_) => return self.fail(EnrollError::UnexpectedReply),
            Err(e) => return self.fail(EnrollError::Parse(e)),
        };
        match parsed {
            Status::Ok(0) => {
                self.collected += 1;
                self.state = EnrollState::AwaitingTouch {
                    collected: self.collected,
                    needed: self.needed,
                };
                if self.collected >= self.needed {
                    self.pending = Some(Pending::Collision);
                    let out = Command::CheckCollision.encode();
                    self.send_buf = Some(out);
                    EnrollAction::EmitProgress {
                        done: self.collected,
                        total: self.needed,
                    }
                } else {
                    let next = Command::Enroll {
                        finger_id: self.slot,
                        total_attempts: self.needed,
                        attempts_done: self.collected,
                    };
                    self.pending = Some(Pending::Sample);
                    let out = next.encode();
                    self.send_buf = Some(out);
                    EnrollAction::EmitProgress {
                        done: self.collected,
                        total: self.needed,
                    }
                }
            }
            Status::Ok(n) => self.fail(EnrollError::Device {
                command: "enroll",
                code: n,
            }),
            Status::Retry(r) => {
                let next = Command::Enroll {
                    finger_id: self.slot,
                    total_attempts: self.needed,
                    attempts_done: self.collected,
                };
                self.pending = Some(Pending::Sample);
                self.send_buf = Some(next.encode());
                EnrollAction::EmitRetry(r)
            }
            Status::MaxEnrolled => self.fail(EnrollError::MaxEnrolled),
            Status::NotEnrolled => self.fail(EnrollError::Device {
                command: "enroll",
                code: 0xfd,
            }),
            Status::Unknown(b) => self.fail(EnrollError::Device {
                command: "enroll",
                code: b,
            }),
        }
    }

    fn step_collision(&mut self, input: &[u8]) -> EnrollAction {
        match Response::parse(&Command::CheckCollision, input) {
            Ok(Response::CheckCollision { colliding, .. }) => match colliding {
                Some(id) => self.fail(EnrollError::Collision(id)),
                None => {
                    self.pending = Some(Pending::Commit);
                    self.state = EnrollState::Committing;
                    self.queue(Command::commit(crate::sub_id(self.slot)))
                }
            },
            Ok(_) => self.fail(EnrollError::UnexpectedReply),
            Err(e) => self.fail(EnrollError::Parse(e)),
        }
    }

    fn step_commit(&mut self, input: &[u8]) -> EnrollAction {
        let cmd = Command::commit(crate::sub_id(self.slot));
        match Response::parse(&cmd, input) {
            Ok(Response::Commit { status, .. }) => match status {
                Status::Ok(0) => {
                    self.pending = None;
                    self.send_buf = None;
                    self.state = EnrollState::Done {
                        template_id: self.slot,
                    };
                    EnrollAction::Complete(self.slot)
                }
                Status::Ok(n) => self.fail(EnrollError::Device {
                    command: "commit",
                    code: n,
                }),
                Status::Retry(r) => self.fail(EnrollError::Device {
                    command: "commit",
                    code: retry_code(r),
                }),
                Status::MaxEnrolled => self.fail(EnrollError::MaxEnrolled),
                Status::NotEnrolled => self.fail(EnrollError::Device {
                    command: "commit",
                    code: 0xfd,
                }),
                Status::Unknown(b) => self.fail(EnrollError::Device {
                    command: "commit",
                    code: b,
                }),
            },
            Ok(_) => self.fail(EnrollError::UnexpectedReply),
            Err(e) => self.fail(EnrollError::Parse(e)),
        }
    }

    fn fail(&mut self, e: EnrollError) -> EnrollAction {
        self.pending = None;
        self.send_buf = None;
        self.state = EnrollState::Failed;
        EnrollAction::Fail(e)
    }
}

fn retry_code(r: Retry) -> u8 {
    match r {
        Retry::MoveDown => 0x41,
        Retry::MoveRight => 0x42,
        Retry::MoveUp => 0x43,
        Retry::MoveLeft => 0x44,
        Retry::Dirty => 0xfb,
        Retry::AreaTooSmall => 0xfe,
    }
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    use crate::ReplyEndpoint;

    /// The machine names the endpoint, so the caller never guesses.
    #[test]
    fn the_collision_check_is_read_on_status_not_touch_wait() {
        let mut sm = Enroll::with_total(0, 2);
        assert_eq!(
            sm.start(),
            EnrollAction::Send(vec![0x40, 0xff, 0x01, 0x00, 0x02, 0x00, 0x00])
        );
        assert_eq!(
            sm.pending_read().map(|(ep, len, _)| (ep, len)),
            Some((ReplyEndpoint::TouchWait, 2))
        );
        for _ in 0..2 {
            let _ = sm.step(&[0x40, 0x00]);
            let _ = sm.take_send();
        }
        assert_eq!(
            sm.pending_read().map(|(ep, len, _)| (ep, len)),
            Some((ReplyEndpoint::Status, 3)),
            "the collision check replies on 0x83 with 3 bytes"
        );
    }

    #[test]
    fn the_commit_is_read_on_status() {
        let mut sm = Enroll::with_total(0, 1);
        let _ = sm.start();
        let _ = sm.step(&[0x40, 0x00]);
        let _ = sm.take_send();
        let _ = sm.step(&[0x40, 0x00, 0xff]);
        assert_eq!(
            sm.pending_read().map(|(ep, len, _)| (ep, len)),
            Some((ReplyEndpoint::Status, 2))
        );
    }

    #[test]
    fn a_finished_machine_has_nothing_pending() {
        let mut sm = Enroll::with_total(0, 1);
        let _ = sm.start();
        let _ = sm.step(&[0x40, 0x00]);
        let _ = sm.take_send();
        let _ = sm.step(&[0x40, 0x00, 0xff]);
        assert_eq!(sm.step(&[0x40, 0x00]), EnrollAction::Complete(0));
        assert_eq!(sm.pending_read(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ok() -> Vec<u8> {
        vec![0x40, 0x00]
    }

    fn drive_ok(total: u8) -> Enroll {
        let mut sm = Enroll::with_total(0, total);
        let EnrollAction::Send(first) = sm.start() else {
            panic!("start must send");
        };
        assert_eq!(first, vec![0x40, 0xff, 0x01, 0x00, total, 0x00, 0x00]);
        for done in 1..=total {
            let action = sm.step(&sample_ok());
            assert_eq!(
                action,
                EnrollAction::EmitProgress { done, total },
                "sample {done}"
            );
            let send = match sm.take_send() {
                Some(s) => s,
                None => panic!("progress is followed by a send"),
            };
            if done < total {
                let EnrollAction::Send(bytes) = send else {
                    panic!("expected resend");
                };
                assert_eq!(
                    bytes,
                    vec![0x40, 0xff, 0x01, 0x00, total, done, 0x00],
                    "attempt counter advances without gaps"
                );
            } else {
                assert_eq!(
                    send,
                    EnrollAction::Send(vec![0x40, 0xff, 0x10]),
                    "last sample is followed by the collision check"
                );
            }
        }
        sm
    }

    #[test]
    fn full_sequence_reaches_the_collision_check() {
        let sm = drive_ok(3);
        assert_eq!(
            sm.state(),
            EnrollState::AwaitingTouch {
                collected: 3,
                needed: 3
            }
        );
    }

    #[test]
    fn no_collision_sends_commit_then_completes() {
        let mut sm = drive_ok(2);
        let action = sm.step(&[0x40, 0x00, 0x00]);
        let EnrollAction::Send(bytes) = action else {
            panic!("no collision must send commit");
        };
        assert_eq!(bytes.len(), 72);
        assert_eq!(&bytes[..4], &[0x40, 0xff, 0x11, 0xf5]);
        assert_eq!(sm.state(), EnrollState::Committing);
        assert_eq!(sm.step(&[0x40, 0x00]), EnrollAction::Complete(0));
        assert_eq!(sm.state(), EnrollState::Done { template_id: 0 });
    }

    #[test]
    fn collision_aborts_before_commit() {
        let mut sm = drive_ok(1);
        assert_eq!(
            sm.step(&[0x40, 0x01, 0x02]),
            EnrollAction::Fail(EnrollError::Collision(2))
        );
        assert_eq!(sm.state(), EnrollState::Failed);
        assert_eq!(sm.take_send(), None);
    }

    #[test]
    fn retry_does_not_advance_the_counter() {
        let mut sm = Enroll::with_total(0, 8);
        let _ = sm.start();
        assert_eq!(sm.step(&[0x40, 0xfe]), EnrollAction::EmitRetry(Retry::AreaTooSmall));
        let Some(EnrollAction::Send(bytes)) = sm.take_send() else {
            panic!("retry must resend the same attempt");
        };
        assert_eq!(bytes, vec![0x40, 0xff, 0x01, 0x00, 0x08, 0x00, 0x00]);
        assert_eq!(sm.collected(), 0);
        assert_eq!(sm.step(&sample_ok()), EnrollAction::EmitProgress { done: 1, total: 8 });
    }

    #[test]
    fn slot_limit_stops_the_loop() {
        let mut sm = Enroll::with_total(0, 8);
        let _ = sm.start();
        assert_eq!(
            sm.step(&[0x40, 0xdd]),
            EnrollAction::Fail(EnrollError::MaxEnrolled)
        );
        assert_eq!(sm.state(), EnrollState::Failed);
    }

    #[test]
    fn commit_error_is_a_failure_not_a_save() {
        let mut sm = drive_ok(1);
        let EnrollAction::Send(_) = sm.step(&[0x40, 0x00, 0x00]) else {
            panic!("expected commit send");
        };
        assert!(matches!(
            sm.step(&[0x40, 0xfb]),
            EnrollAction::Fail(EnrollError::Device { .. })
        ));
        assert_eq!(sm.state(), EnrollState::Failed);
    }

    #[test]
    fn short_reply_is_a_parse_failure() {
        let mut sm = Enroll::with_total(0, 2);
        let _ = sm.start();
        assert!(matches!(
            sm.step(&[0x40]),
            EnrollAction::Fail(EnrollError::Parse(_))
        ));
    }
}
