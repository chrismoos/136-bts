use crate::core::call_session::{CallSession, CallState, SessionError};

const ANSWER_DELAY_MS: u64 = 5_000;

pub(super) enum Answer {
    Disabled,
    WaitingBearer,
    Ringing { started_ms: u64 },
    Playing,
    Stopped,
}

impl Answer {
    pub(super) fn advance(
        &mut self,
        session: &mut CallSession,
        now_ms: u64,
    ) -> Result<(), SessionError> {
        if matches!(
            session.state(),
            CallState::Releasing(_) | CallState::Stopped(_)
        ) {
            if !matches!(self, Self::Disabled) {
                *self = Self::Stopped;
            }
            return Ok(());
        }
        match self {
            Self::WaitingBearer if session.state() == CallState::WaitingAnswer => {
                *self = Self::Ringing { started_ms: now_ms };
            }
            Self::Ringing { started_ms }
                if session.state() == CallState::WaitingAnswer
                    && now_ms.saturating_sub(*started_ms) >= ANSWER_DELAY_MS =>
            {
                session.remote_answered(now_ms)?;
                *self = Self::Playing;
            }
            _ => {}
        }
        Ok(())
    }
}
