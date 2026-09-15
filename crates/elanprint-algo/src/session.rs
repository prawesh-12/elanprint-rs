//! Grant only on terminal verify-match; unknowns fail closed.

use crate::policy::AttemptPolicy;
use crate::status::verify;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Waiting {
        failures: u32,
    },
    Succeeded,
    Denied,
    LockedOut,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Status {
        result: String,
        done: bool,
    },
    FingerSelected {
        finger: String,
    },
    Disconnected,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Prompt(String),
    Granted(String),
    Deny(String),
    Locked,
    Failed(String),
    Idle,
}

/// Never a grant path.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("unknown status string '{0}', failing closed")]
    UnknownString(String),
    #[error("signal arrived with no session running")]
    NoSession,
}

pub struct VerifySession {
    user: String,
    policy: AttemptPolicy,
    state: SessionState,
    selected: Option<String>,
}

impl VerifySession {
    /// Uses the PAM attempt budget.
    pub fn new(user: &str) -> Self {
        Self::with_policy(user, AttemptPolicy::pam_default())
    }

    pub fn with_policy(user: &str, policy: AttemptPolicy) -> Self {
        Self {
            user: user.to_string(),
            policy,
            state: SessionState::Idle,
            selected: None,
        }
    }

    pub fn state(&self) -> SessionState {
        self.state.clone()
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Prompts for the first touch.
    pub fn begin(&mut self) -> Action {
        self.state = SessionState::Waiting { failures: 0 };
        Action::Prompt("touch the sensor".to_string())
    }

    pub fn step(&mut self, signal: &Signal) -> Result<Action, SessionError> {
        match signal {
            Signal::Cancelled => {
                self.state = SessionState::Idle;
                Ok(Action::Idle)
            }
            Signal::Disconnected => {
                self.state = SessionState::Failed;
                Ok(Action::Failed("reader disconnected".to_string()))
            }
            Signal::FingerSelected { finger } => {
                self.selected = Some(finger.clone());
                Ok(Action::Prompt("touch the sensor".to_string()))
            }
            Signal::Status { result, done } => self.step_status(result, *done),
        }
    }

    fn step_status(&mut self, result: &str, done: bool) -> Result<Action, SessionError> {
        let SessionState::Waiting { failures } = self.state else {
            return Err(SessionError::NoSession);
        };
        if result == verify::MATCH {
            if !done {
                return Err(SessionError::UnknownString(result.to_string()));
            }
            self.state = SessionState::Succeeded;
            return Ok(Action::Granted(self.user.clone()));
        }
        if result == verify::NO_MATCH {
            let used = failures + 1;
            if self.policy.allows(used) {
                self.state = SessionState::Waiting { failures: used };
                return Ok(Action::Deny(format!(
                    "no match, {} tries left",
                    self.policy.left(used)
                )));
            }
            self.state = SessionState::LockedOut;
            return Ok(Action::Locked);
        }
        if is_retry(result) {
            return Ok(Action::Prompt(hint(result)));
        }
        if result == verify::DISCONNECTED {
            self.state = SessionState::Failed;
            return Ok(Action::Failed("reader disconnected".to_string()));
        }
        if result == verify::UNKNOWN_ERROR {
            self.state = SessionState::Failed;
            return Ok(Action::Failed("reader error".to_string()));
        }
        Err(SessionError::UnknownString(result.to_string()))
    }
}

fn is_retry(result: &str) -> bool {
    matches!(
        result,
        verify::SWIPE_TOO_SHORT | verify::NOT_CENTERED | verify::REMOVE_AND_RETRY
    )
}

fn hint(result: &str) -> String {
    if result == verify::NOT_CENTERED {
        return "center the finger and retry".to_string();
    }
    if result == verify::REMOVE_AND_RETRY {
        return "lift the finger and retry".to_string();
    }
    "touch the sensor again".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(result: &str, done: bool) -> Signal {
        Signal::Status {
            result: result.to_string(),
            done,
        }
    }

    #[test]
    fn match_grants_the_claimed_user() {
        let mut session = VerifySession::new("u");
        assert_eq!(session.begin(), Action::Prompt("touch the sensor".to_string()));
        let action = session.step(&status(verify::MATCH, true));
        assert_eq!(action.unwrap(), Action::Granted("u".to_string()));
        assert_eq!(session.state(), SessionState::Succeeded);
    }

    #[test]
    fn match_without_done_is_not_a_grant() {
        let mut session = VerifySession::new("u");
        session.begin();
        assert!(session.step(&status(verify::MATCH, false)).is_err());
        assert_ne!(session.state(), SessionState::Succeeded);
    }

    #[test]
    fn three_misses_lock_out() {
        let mut session = VerifySession::new("u");
        session.begin();
        assert!(matches!(
            session.step(&status(verify::NO_MATCH, true)).unwrap(),
            Action::Deny(_)
        ));
        assert!(matches!(
            session.step(&status(verify::NO_MATCH, true)).unwrap(),
            Action::Deny(_)
        ));
        assert_eq!(
            session.step(&status(verify::NO_MATCH, true)).unwrap(),
            Action::Locked
        );
        assert_eq!(session.state(), SessionState::LockedOut);
    }

    #[test]
    fn retries_prompt_without_spending_tries() {
        let mut session = VerifySession::new("u");
        session.begin();
        let action = session.step(&status(verify::NOT_CENTERED, false)).unwrap();
        assert_eq!(action, Action::Prompt("center the finger and retry".to_string()));
        let action = session.step(&status(verify::NO_MATCH, true)).unwrap();
        assert!(matches!(action, Action::Deny(_)));
        assert_eq!(session.state(), SessionState::Waiting { failures: 1 });
    }

    #[test]
    fn unknown_string_fails_closed() {
        let mut session = VerifySession::new("u");
        session.begin();
        assert!(session.step(&status("verify-something-new", true)).is_err());
        assert_ne!(session.state(), SessionState::Succeeded);
    }

    #[test]
    fn disconnect_and_cancel_are_distinct() {
        let mut session = VerifySession::new("u");
        session.begin();
        assert_eq!(
            session.step(&Signal::Disconnected).unwrap(),
            Action::Failed("reader disconnected".to_string())
        );
        let mut session = VerifySession::new("u");
        session.begin();
        assert_eq!(session.step(&Signal::Cancelled).unwrap(), Action::Idle);
    }
}
