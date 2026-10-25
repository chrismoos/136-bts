use std::{error::Error, fmt, sync::Arc};

const INACTIVITY_BLOCKS: u64 = 32;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InactivityAction {
    None,
    RequestUnsolicitedStatus,
    Terminate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusResult {
    Sent,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InactivityError {
    ClockBackwards,
}
impl fmt::Display for InactivityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ARQ TDMA block clock moved backwards")
    }
}
impl Error for InactivityError {}
#[derive(Debug, Clone, Copy)]
enum State {
    Idle,
    Waiting { since_block: u64 },
    SendingUnsolicited,
    FinalWait { since_block: u64 },
}
#[derive(Debug, Clone)]
pub struct TransactionToken(Arc<()>);

#[derive(Debug)]
pub struct ArqInactivity {
    state: State,
    last_block: u64,
    unsolicited_pending: bool,
    transaction: Arc<()>,
}
impl Default for ArqInactivity {
    fn default() -> Self {
        Self {
            state: State::Idle,
            last_block: 0,
            unsolicited_pending: false,
            transaction: Arc::new(()),
        }
    }
}
impl ArqInactivity {
    pub(crate) fn owns(&self, token: &TransactionToken) -> bool {
        Arc::ptr_eq(&token.0, &self.transaction)
    }

    pub fn token(&self) -> TransactionToken {
        TransactionToken(Arc::clone(&self.transaction))
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.unsolicited_pending = false;
        self.transaction = Arc::new(());
    }

    /// Call for a matching accepted BEGIN/CONTINUE only while message frames remain pending.
    pub fn received_frame(&mut self, tdma_block: u64) -> Result<(), InactivityError> {
        self.observe(tdma_block)?;
        self.state = State::Waiting {
            since_block: tdma_block,
        };
        Ok(())
    }
    // IS-136.1-A §5.4.2.2. Both inactivity intervals are 32 TDMA blocks.
    pub fn advance(&mut self, tdma_block: u64) -> Result<InactivityAction, InactivityError> {
        self.observe(tdma_block)?;
        match self.state {
            State::Waiting { since_block }
                if !self.unsolicited_pending && tdma_block - since_block >= INACTIVITY_BLOCKS =>
            {
                self.state = State::SendingUnsolicited;
                self.unsolicited_pending = true;
                Ok(InactivityAction::RequestUnsolicitedStatus)
            }
            State::FinalWait { since_block } if tdma_block - since_block >= INACTIVITY_BLOCKS => {
                self.reset();
                Ok(InactivityAction::Terminate)
            }
            _ => Ok(InactivityAction::None),
        }
    }
    /// Capture token when requesting status. Reset invalidates outstanding transaction callbacks.
    pub fn unsolicited_status_result(
        &mut self,
        token: &TransactionToken,
        tdma_block: u64,
        result: StatusResult,
    ) -> Result<InactivityAction, InactivityError> {
        if !Arc::ptr_eq(&token.0, &self.transaction) {
            return Ok(InactivityAction::None);
        }
        self.observe(tdma_block)?;
        if !self.unsolicited_pending {
            return Ok(InactivityAction::None);
        }
        self.unsolicited_pending = false;
        match result {
            StatusResult::Sent => {
                self.state = if matches!(self.state, State::Waiting { .. }) {
                    State::Waiting {
                        since_block: tdma_block,
                    }
                } else {
                    State::FinalWait {
                        since_block: tdma_block,
                    }
                };
                Ok(InactivityAction::None)
            }
            StatusResult::Failed => {
                self.reset();
                Ok(InactivityAction::Terminate)
            }
        }
    }
    pub fn solicited_status_result(
        &mut self,
        token: &TransactionToken,
        tdma_block: u64,
        result: StatusResult,
    ) -> Result<InactivityAction, InactivityError> {
        if !Arc::ptr_eq(&token.0, &self.transaction) {
            return Ok(InactivityAction::None);
        }
        self.observe(tdma_block)?;
        if matches!(self.state, State::Idle) {
            return Ok(InactivityAction::None);
        }
        match result {
            StatusResult::Sent => {
                self.state = State::Waiting {
                    since_block: tdma_block,
                };
                Ok(InactivityAction::None)
            }
            StatusResult::Failed => {
                self.reset();
                Ok(InactivityAction::Terminate)
            }
        }
    }
    pub(crate) fn observe(&mut self, tdma_block: u64) -> Result<(), InactivityError> {
        if tdma_block < self.last_block {
            return Err(InactivityError::ClockBackwards);
        }
        self.last_block = tdma_block;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadlines_are_exact_and_final_wait_starts_after_successful_status_send() {
        let mut timer = ArqInactivity::default();
        timer.received_frame(100).unwrap();
        assert_eq!(timer.advance(131).unwrap(), InactivityAction::None);
        assert_eq!(
            timer.advance(132).unwrap(),
            InactivityAction::RequestUnsolicitedStatus
        );
        assert_eq!(timer.advance(200).unwrap(), InactivityAction::None);
        timer
            .unsolicited_status_result(&timer.token(), 201, StatusResult::Sent)
            .unwrap();
        assert_eq!(timer.advance(232).unwrap(), InactivityAction::None);
        assert_eq!(timer.advance(233).unwrap(), InactivityAction::Terminate);
        assert_eq!(timer.advance(234).unwrap(), InactivityAction::None);
    }
    #[test]
    fn accepted_frame_and_solicited_status_restart_wait_and_failures_terminate() {
        let mut timer = ArqInactivity::default();
        timer.received_frame(0).unwrap();
        timer.received_frame(31).unwrap();
        assert_eq!(timer.advance(32).unwrap(), InactivityAction::None);
        timer
            .solicited_status_result(&timer.token(), 60, StatusResult::Sent)
            .unwrap();
        assert_eq!(timer.advance(91).unwrap(), InactivityAction::None);
        assert_eq!(
            timer.advance(92).unwrap(),
            InactivityAction::RequestUnsolicitedStatus
        );
        assert_eq!(
            timer
                .unsolicited_status_result(&timer.token(), 93, StatusResult::Failed)
                .unwrap(),
            InactivityAction::Terminate
        );
        timer.received_frame(100).unwrap();
        assert_eq!(
            timer
                .solicited_status_result(&timer.token(), 101, StatusResult::Failed)
                .unwrap(),
            InactivityAction::Terminate
        );
        timer.received_frame(200).unwrap();
        timer.advance(232).unwrap();
        timer
            .unsolicited_status_result(&timer.token(), 233, StatusResult::Sent)
            .unwrap();
        timer.received_frame(250).unwrap();
        assert_eq!(timer.advance(265).unwrap(), InactivityAction::None);
        assert_eq!(
            timer.advance(282).unwrap(),
            InactivityAction::RequestUnsolicitedStatus
        );
    }
    #[test]
    fn received_frame_during_status_send_keeps_new_transaction_activity() {
        let mut timer = ArqInactivity::default();
        timer.received_frame(0).unwrap();
        timer.advance(32).unwrap();
        timer.received_frame(33).unwrap();
        timer
            .unsolicited_status_result(&timer.token(), 34, StatusResult::Sent)
            .unwrap();
        assert_eq!(timer.advance(65).unwrap(), InactivityAction::None);
        assert_eq!(
            timer.advance(66).unwrap(),
            InactivityAction::RequestUnsolicitedStatus
        );
        timer.received_frame(67).unwrap();
        assert_eq!(
            timer
                .unsolicited_status_result(&timer.token(), 68, StatusResult::Failed)
                .unwrap(),
            InactivityAction::Terminate
        );
    }

    #[test]
    fn stale_and_foreign_tokens_cannot_mutate_new_transaction_or_clock() {
        let mut timer = ArqInactivity::default();
        timer.received_frame(0).unwrap();
        let stale = timer.token();
        timer.reset();
        timer.received_frame(100).unwrap();
        assert_eq!(
            timer
                .solicited_status_result(&stale, 1000, StatusResult::Failed)
                .unwrap(),
            InactivityAction::None
        );
        assert_eq!(timer.advance(131).unwrap(), InactivityAction::None);
        assert_eq!(
            timer.advance(132).unwrap(),
            InactivityAction::RequestUnsolicitedStatus
        );
        let foreign = ArqInactivity::default().token();
        assert_eq!(
            timer
                .unsolicited_status_result(&foreign, 1000, StatusResult::Sent)
                .unwrap(),
            InactivityAction::None
        );
        let current = timer.token();
        assert_eq!(
            timer
                .unsolicited_status_result(&current, 133, StatusResult::Sent)
                .unwrap(),
            InactivityAction::None
        );
        assert_eq!(timer.advance(165).unwrap(), InactivityAction::Terminate);
    }

    #[test]
    fn backward_time_is_rejected_without_changing_deadline_and_reset_stops_timeout() {
        let mut timer = ArqInactivity::default();
        timer.received_frame(50).unwrap();
        assert_eq!(timer.advance(49), Err(InactivityError::ClockBackwards));
        assert_eq!(
            timer.advance(82).unwrap(),
            InactivityAction::RequestUnsolicitedStatus
        );
        timer.reset();
        assert_eq!(
            timer
                .unsolicited_status_result(&timer.token(), 83, StatusResult::Failed)
                .unwrap(),
            InactivityAction::None
        );
        assert_eq!(timer.advance(u64::MAX).unwrap(), InactivityAction::None);
    }
}
