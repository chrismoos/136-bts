use super::{
    inactivity::{
        ArqInactivity, InactivityAction, InactivityError, StatusResult, TransactionToken,
    },
    *,
};
use std::{error::Error, fmt};

#[derive(Debug)]
enum StatusKind {
    Solicited,
    Unsolicited,
}
#[derive(Debug)]
pub struct StatusTransmission {
    pub request: StatusRequest,
    token: TransactionToken,
    kind: StatusKind,
    complete_at_poll: bool,
}
#[derive(Debug)]
pub enum ReceiveEvent {
    Ignored,
    Accepted {
        message: Option<SingleMessage>,
        status: Option<StatusTransmission>,
    },
}
#[derive(Debug)]
pub enum TimerEvent {
    None,
    SendStatus(StatusTransmission),
    Terminated,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendEvent {
    Ignored,
    Pending,
    Complete,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureError {
    Wire(SpachError),
    Clock(InactivityError),
}
impl fmt::Display for ProcedureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ARQ procedure failed: {self:?}")
    }
}
impl Error for ProcedureError {}
impl From<SpachError> for ProcedureError {
    fn from(error: SpachError) -> Self {
        Self::Wire(error)
    }
}
impl From<InactivityError> for ProcedureError {
    fn from(error: InactivityError) -> Self {
        Self::Clock(error)
    }
}
#[derive(Debug)]
pub struct ArqProcedure {
    receiver: ArqReassembler,
    inactivity: ArqInactivity,
}
impl ArqProcedure {
    pub fn try_new(identity: MobileIdentity) -> Result<Self, SpachError> {
        Ok(Self {
            receiver: ArqReassembler::try_new(identity)?,
            inactivity: ArqInactivity::default(),
        })
    }
    pub fn reset(&mut self) {
        self.receiver.reset();
        self.inactivity.reset();
    }
    pub fn active(&self) -> bool {
        self.receiver.status().is_some()
    }

    // IS-136.1-A §5.4.2. Matching BEGIN replaces the active transaction.
    pub fn receive(
        &mut self,
        tdma_block: u64,
        bits: &[bool],
    ) -> Result<ReceiveEvent, ProcedureError> {
        self.inactivity.observe(tdma_block)?;
        let frame = decode(bits)?;
        let is_begin = matches!(frame, ArqFrame::Begin { .. });
        let Reception::Accepted {
            message,
            status_request,
        } = self.receiver.push_frame(frame)?
        else {
            return Ok(ReceiveEvent::Ignored);
        };
        if is_begin {
            self.inactivity.reset();
        }
        let complete = self.receiver.complete();
        if !complete {
            self.inactivity.received_frame(tdma_block)?;
        }
        let status = status_request.map(|request| StatusTransmission {
            request,
            token: self.inactivity.token(),
            kind: StatusKind::Solicited,
            complete_at_poll: complete,
        });
        if complete && status.is_none() {
            self.reset();
        }
        Ok(ReceiveEvent::Accepted { message, status })
    }
    pub fn advance(&mut self, tdma_block: u64) -> Result<TimerEvent, ProcedureError> {
        self.inactivity.observe(tdma_block)?;
        if self.receiver.complete() {
            return Ok(TimerEvent::None);
        }
        match self.inactivity.advance(tdma_block)? {
            InactivityAction::None => Ok(TimerEvent::None),
            InactivityAction::Terminate => {
                self.reset();
                Ok(TimerEvent::Terminated)
            }
            InactivityAction::RequestUnsolicitedStatus => {
                let status = self
                    .receiver
                    .status()
                    .expect("inactivity armed for an active transaction");
                // IS-136.1-A §5.3.1.2. Unsolicited access uses contention.
                Ok(TimerEvent::SendStatus(StatusTransmission {
                    request: StatusRequest {
                        status,
                        response_mode: ResponseMode::Contention,
                    },
                    token: self.inactivity.token(),
                    kind: StatusKind::Unsolicited,
                    complete_at_poll: false,
                }))
            }
        }
    }

    /// Consumes a status request so one send completion cannot be applied twice.
    pub fn status_sent(
        &mut self,
        tdma_block: u64,
        transmission: StatusTransmission,
        result: StatusResult,
    ) -> Result<SendEvent, ProcedureError> {
        if !self.inactivity.owns(&transmission.token) {
            return Ok(SendEvent::Ignored);
        }
        self.inactivity.observe(tdma_block)?;
        if result == StatusResult::Failed {
            self.reset();
            return Ok(SendEvent::Failed);
        }
        if transmission.complete_at_poll {
            self.reset();
            return Ok(SendEvent::Complete);
        }
        match transmission.kind {
            StatusKind::Solicited => {
                self.inactivity
                    .solicited_status_result(&transmission.token, tdma_block, result)?;
            }
            StatusKind::Unsolicited => {
                self.inactivity.unsolicited_status_result(
                    &transmission.token,
                    tdma_block,
                    result,
                )?;
            }
        }
        Ok(SendEvent::Pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frames(message: &[u8], polling: bool) -> Vec<ArqFrame> {
        fragment_message(
            ArqHeader {
                channel: Channel::Smsch,
                flags: HeaderFlags::default(),
                partial_echo: 42,
                polling,
                response_mode: ResponseMode::Reservation,
            },
            MobileIdentity::Imsi50(1),
            message,
        )
        .unwrap()
    }
    fn status(event: ReceiveEvent) -> StatusTransmission {
        let ReceiveEvent::Accepted {
            status: Some(status),
            ..
        } = event
        else {
            panic!("expected status");
        };
        status
    }
    #[test]
    fn completion_waits_for_final_snapshot_and_replacement_invalidates_old_callbacks() {
        let mut procedure = ArqProcedure::try_new(MobileIdentity::Imsi50(1)).unwrap();
        let frames = frames(&[0xff; 10], true);
        let early = status(procedure.receive(0, &frames[0].encode().unwrap()).unwrap());
        let final_status = status(procedure.receive(1, &frames[1].encode().unwrap()).unwrap());
        assert_eq!(
            procedure.status_sent(2, early, StatusResult::Sent).unwrap(),
            SendEvent::Pending
        );
        assert!(procedure.active());
        assert_eq!(
            procedure
                .status_sent(3, final_status, StatusResult::Sent)
                .unwrap(),
            SendEvent::Complete
        );
        assert!(!procedure.active());
        let stale = status(procedure.receive(4, &frames[0].encode().unwrap()).unwrap());
        let current = status(procedure.receive(5, &frames[0].encode().unwrap()).unwrap());
        assert_eq!(
            procedure
                .status_sent(1000, stale, StatusResult::Failed)
                .unwrap(),
            SendEvent::Ignored
        );
        assert!(procedure.active());
        assert_eq!(
            procedure
                .status_sent(6, current, StatusResult::Failed)
                .unwrap(),
            SendEvent::Failed
        );
        assert!(!procedure.active());
    }
    #[test]
    fn inactivity_requests_contention_status_and_second_deadline_terminates() {
        let mut procedure = ArqProcedure::try_new(MobileIdentity::Imsi50(1)).unwrap();
        let frames = frames(&[0xff; 10], false);
        procedure.receive(0, &frames[0].encode().unwrap()).unwrap();
        assert!(matches!(procedure.advance(31).unwrap(), TimerEvent::None));
        let TimerEvent::SendStatus(request) = procedure.advance(32).unwrap() else {
            panic!("expected unsolicited status");
        };
        assert_eq!(request.request.response_mode, ResponseMode::Contention);
        assert_eq!(
            request.request.status.received_frames,
            [false; ARQ_MAP_BITS]
        );
        procedure
            .status_sent(33, request, StatusResult::Sent)
            .unwrap();
        assert!(matches!(procedure.advance(64).unwrap(), TimerEvent::None));
        assert!(matches!(
            procedure.advance(65).unwrap(),
            TimerEvent::Terminated
        ));
        assert!(!procedure.active());
        assert!(matches!(
            procedure.receive(66, &frames[1].encode().unwrap()).unwrap(),
            ReceiveEvent::Ignored
        ));
    }
    #[test]
    fn unpolled_completion_ends_transaction_and_clock_failure_preserves_it() {
        let mut procedure = ArqProcedure::try_new(MobileIdentity::Imsi50(1)).unwrap();
        let frames = frames(&[0xff; 10], false);
        procedure.receive(10, &frames[0].encode().unwrap()).unwrap();
        assert!(matches!(
            procedure.receive(9, &frames[1].encode().unwrap()),
            Err(ProcedureError::Clock(InactivityError::ClockBackwards))
        ));
        assert!(procedure.active());
        let ReceiveEvent::Accepted {
            message: Some(message),
            status: None,
        } = procedure.receive(11, &frames[1].encode().unwrap()).unwrap()
        else {
            panic!("expected completed message");
        };
        assert_eq!(message.message, [0xff; 10]);
        assert!(!procedure.active());
    }
}
