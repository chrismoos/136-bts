use crate::{
    l3::{
        broadcast::ProtocolVersion,
        relay::HigherLayerProtocol,
        tsar::{self, AcknowledgmentWindow, DecodedResponse, Segment, SegmentedMessage, Status},
    },
    link::rach::MobileIdentity,
};
use std::{error::Error as StdError, fmt, sync::Arc};

// TIA-136-620-A §§2.1,2.4,5 anchor SEGINT_TMR to Layer 3 submission and fix it at 330 seconds.
const SEGMENT_INTERVAL_MS: u64 = 330_000;
// TIA-136-700-D §2 requires both carrier-specific and TSAR bits for a segmented carrier HLPI.
const SEGMENTED_CARRIER_BITS: u8 = 0xc0;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    LowerLayerAccepted,
    PeerReceived,
    ExceptionIntervalElapsed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowerLayerFailure {
    Rejected { cause: u8 },
    TimedOut,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderFailure {
    SegmentInterval,
    LowerLayer(LowerLayerFailure),
    Peer(Status),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderState {
    Prepared,
    AwaitingAccept,
    AwaitingResponse,
    Complete(Completion),
    Failed(SenderFailure),
    Cancelled,
}
impl SenderState {
    fn closed(self) -> bool {
        matches!(self, Self::Complete(_) | Self::Failed(_) | Self::Cancelled)
    }
}
#[derive(Debug, Clone)]
pub struct Submission {
    token: Arc<()>,
    segment: Segment,
    prepared_monotonic_ms: u64,
}
impl Submission {
    pub fn segment(&self) -> &Segment {
        &self.segment
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Identity,
    Plan,
    Codec(tsar::Error),
    ClockRegression,
    TimeOverflow,
    CounterOverflow,
    Closed,
    StaleSubmission,
    NotSubmitted,
    ReceiptTime,
    Peer,
    Service,
    TransactionId,
    ResponsesProhibited,
    UnparsedResponse,
    UnsubmittedAcknowledgment,
    ContradictoryReceipt,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TSAR sender failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Acknowledged {
    Unreported,
    Missing,
    Received,
}

/// One outgoing peer/service transaction. Fence retired Layer 3 work and dispatch protocol observations in timestamp order before advancing timers.
pub struct Sender {
    peer: MobileIdentity,
    protocol: HigherLayerProtocol,
    version: ProtocolVersion,
    plan: SegmentedMessage,
    transaction_id: u8,
    state: SenderState,
    current: Option<Submission>,
    submitted: Vec<bool>,
    retry: Vec<bool>,
    acknowledged: Vec<Acknowledged>,
    deadline_monotonic_ms: Option<u64>,
    last_monotonic_ms: u64,
    layer3_submissions: u64,
}
impl Sender {
    pub fn try_new(
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        plan: SegmentedMessage,
        version: ProtocolVersion,
        monotonic_ms: u64,
    ) -> Result<Self, Error> {
        let (_, width, value) = peer.parts();
        if value == 0 || value >= 1 << width {
            return Err(Error::Identity);
        }
        let segmented = match protocol {
            HigherLayerProtocol::Segmented(_) => true,
            HigherLayerProtocol::CarrierSpecific(raw) => {
                raw & SEGMENTED_CARRIER_BITS == SEGMENTED_CARRIER_BITS
            }
            _ => false,
        };
        if !segmented {
            return Err(Error::Service);
        }
        let transaction_id = plan.segments().first().ok_or(Error::Plan)?.transaction_id;
        for segment in plan.segments() {
            segment.encode(version).map_err(Error::Codec)?;
        }
        let count = plan.segments().len();
        let mut sender = Self {
            peer,
            protocol,
            version,
            plan,
            transaction_id,
            state: SenderState::Prepared,
            current: None,
            submitted: vec![false; count],
            retry: vec![false; count],
            acknowledged: vec![Acknowledged::Unreported; count],
            deadline_monotonic_ms: None,
            last_monotonic_ms: monotonic_ms,
            layer3_submissions: 0,
        };
        sender.prepare_next(monotonic_ms);
        Ok(sender)
    }
    pub fn state(&self) -> SenderState {
        self.state
    }
    pub fn deadline_monotonic_ms(&self) -> Option<u64> {
        self.deadline_monotonic_ms
    }
    pub fn layer3_submissions(&self) -> u64 {
        self.layer3_submissions
    }
    pub fn submission(&self) -> Option<&Submission> {
        if self.state != SenderState::Prepared {
            return None;
        }
        self.current.as_ref()
    }
    fn check_clock(&self, now: u64) -> Result<(), Error> {
        if now < self.last_monotonic_ms {
            Err(Error::ClockRegression)
        } else {
            Ok(())
        }
    }
    fn needs_submission(&self, index: usize) -> bool {
        !self.submitted[index] || self.retry[index]
    }
    fn has_pending(&self) -> bool {
        (0..self.submitted.len()).any(|index| self.needs_submission(index))
    }
    fn prepare_next(&mut self, now: u64) {
        let next = (0..self.submitted.len()).find(|index| self.needs_submission(*index));
        let retain = self.state == SenderState::Prepared
            && self
                .current
                .as_ref()
                .is_some_and(|current| Some(usize::from(current.segment.sequence_number)) == next);
        if !retain {
            self.current = next.map(|index| Submission {
                token: Arc::new(()),
                segment: self.plan.segments()[index].clone(),
                prepared_monotonic_ms: now,
            });
        }
        self.state = if self.current.is_some() {
            SenderState::Prepared
        } else {
            SenderState::AwaitingResponse
        };
    }
    fn finish(&mut self, state: SenderState) {
        self.state = state;
        self.current = None;
        self.deadline_monotonic_ms = None;
    }
    fn owns_current(&self, submission: &Submission) -> bool {
        self.current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.token, &submission.token))
    }
    pub fn advance(&mut self, now: u64) -> Result<SenderState, Error> {
        self.check_clock(now)?;
        if !self.state.closed()
            && self
                .deadline_monotonic_ms
                .is_some_and(|deadline| now >= deadline)
        {
            match self.plan.acknowledgment_window() {
                AcknowledgmentWindow::Prohibited => {
                    self.finish(SenderState::Failed(SenderFailure::SegmentInterval))
                }
                AcknowledgmentWindow::Exceptions
                    if self.state == SenderState::AwaitingResponse && !self.has_pending() =>
                {
                    self.finish(SenderState::Complete(Completion::ExceptionIntervalElapsed))
                }
                _ => {
                    if let Some(current) = &self.current {
                        self.retry[usize::from(current.segment.sequence_number)] = true;
                    } else {
                        for (index, acknowledged) in self.acknowledged.iter().enumerate() {
                            if *acknowledged != Acknowledged::Received {
                                self.retry[index] = true;
                            }
                        }
                        if !self.has_pending() {
                            let last = self.retry.len() - 1;
                            self.retry[last] = true;
                        }
                    }
                    self.deadline_monotonic_ms = None;
                    self.prepare_next(now);
                }
            }
        }
        self.last_monotonic_ms = now;
        Ok(self.state)
    }
    /// Supply actual Layer 3 accepted-handoff time separately from observation time. Preparing or merely completing RF transmission does not start this timer.
    pub fn submitted(
        &mut self,
        submission: &Submission,
        queued_monotonic_ms: u64,
        observed_monotonic_ms: u64,
    ) -> Result<SenderState, Error> {
        self.check_clock(observed_monotonic_ms)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        if self.state != SenderState::Prepared || !self.owns_current(submission) {
            return Err(Error::StaleSubmission);
        }
        if queued_monotonic_ms < submission.prepared_monotonic_ms
            || queued_monotonic_ms > observed_monotonic_ms
        {
            return Err(Error::ReceiptTime);
        }
        if self.plan.acknowledgment_window() == AcknowledgmentWindow::Prohibited
            && self
                .deadline_monotonic_ms
                .is_some_and(|deadline| queued_monotonic_ms >= deadline)
        {
            self.advance(observed_monotonic_ms)?;
            return Err(Error::ReceiptTime);
        }
        let index = usize::from(submission.segment.sequence_number);
        let last = !(0..self.submitted.len())
            .any(|candidate| candidate != index && self.needs_submission(candidate));
        let deadline =
            if last && self.plan.acknowledgment_window() == AcknowledgmentWindow::Prohibited {
                None
            } else {
                Some(
                    queued_monotonic_ms
                        .checked_add(SEGMENT_INTERVAL_MS)
                        .ok_or(Error::TimeOverflow)?,
                )
            };
        let count = self
            .layer3_submissions
            .checked_add(1)
            .ok_or(Error::CounterOverflow)?;
        self.submitted[index] = true;
        self.retry[index] = false;
        self.layer3_submissions = count;
        self.deadline_monotonic_ms = deadline;
        self.state = SenderState::AwaitingAccept;
        self.advance(observed_monotonic_ms)
    }
    /// Supply the matching R-DATA ACCEPT from the Layer 3 exchange, not a local queue or TX completion.
    pub fn accepted(&mut self, submission: &Submission, now: u64) -> Result<SenderState, Error> {
        self.check_clock(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        if self.state != SenderState::AwaitingAccept || !self.owns_current(submission) {
            return Err(Error::StaleSubmission);
        }
        self.advance(now)?;
        if self.state != SenderState::AwaitingAccept || !self.owns_current(submission) {
            return Err(Error::StaleSubmission);
        }
        self.current = None;
        self.prepare_next(now);
        if self.state == SenderState::AwaitingResponse
            && self.plan.acknowledgment_window() == AcknowledgmentWindow::Prohibited
        {
            self.finish(SenderState::Complete(Completion::LowerLayerAccepted));
        }
        Ok(self.state)
    }
    pub fn lower_failed(
        &mut self,
        submission: &Submission,
        failure: LowerLayerFailure,
        now: u64,
    ) -> Result<SenderState, Error> {
        self.check_clock(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        if self.state != SenderState::AwaitingAccept || !self.owns_current(submission) {
            return Err(Error::StaleSubmission);
        }
        self.advance(now)?;
        if self.state != SenderState::AwaitingAccept || !self.owns_current(submission) {
            return Err(Error::StaleSubmission);
        }
        self.finish(SenderState::Failed(SenderFailure::LowerLayer(failure)));
        Ok(self.state)
    }
    /// Bind a CRC-valid TSAR response to this peer/service stream. Wire transaction IDs do not identify local generations.
    pub fn receive_response(
        &mut self,
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        decoded: &DecodedResponse,
        now: u64,
    ) -> Result<SenderState, Error> {
        self.check_clock(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        if peer != self.peer {
            return Err(Error::Peer);
        }
        if protocol != self.protocol {
            return Err(Error::Service);
        }
        if !decoded.unparsed_bits.is_empty() {
            return Err(Error::UnparsedResponse);
        }
        let response = &decoded.response;
        response.validate(self.version).map_err(Error::Codec)?;
        if response.transaction_id != self.transaction_id {
            return Err(Error::TransactionId);
        }
        if self.plan.acknowledgment_window() == AcknowledgmentWindow::Prohibited {
            return Err(Error::ResponsesProhibited);
        }
        if !self.submitted.iter().any(|value| *value) {
            return Err(Error::NotSubmitted);
        }
        for index in 0..response.acknowledgments.bit_count() {
            if response.acknowledgments.received(index as u8) == Some(true)
                && !self.submitted.get(index).copied().unwrap_or(false)
            {
                return Err(Error::UnsubmittedAcknowledgment);
            }
        }
        if response.status == Status::Received
            && (!self.submitted.iter().all(|value| *value)
                || (0..self.submitted.len())
                    .any(|index| response.acknowledgments.received(index as u8) == Some(false)))
        {
            return Err(Error::ContradictoryReceipt);
        }
        if self
            .deadline_monotonic_ms
            .is_some_and(|deadline| now > deadline)
        {
            self.advance(now)?;
            if self.state.closed() {
                return Err(Error::Closed);
            }
        }
        match response.status {
            Status::Received => self.finish(SenderState::Complete(Completion::PeerReceived)),
            Status::Continuing | Status::InterSegmentTimeout => {
                for index in 0..self.submitted.len() {
                    self.acknowledged[index] = match response.acknowledgments.received(index as u8)
                    {
                        Some(true) => Acknowledged::Received,
                        Some(false) => Acknowledged::Missing,
                        None => Acknowledged::Unreported,
                    };
                    if self.submitted[index] {
                        match self.acknowledged[index] {
                            Acknowledged::Received => self.retry[index] = false,
                            Acknowledged::Missing => self.retry[index] = true,
                            Acknowledged::Unreported
                                if response.status == Status::InterSegmentTimeout =>
                            {
                                self.retry[index] = true
                            }
                            _ => {}
                        }
                    }
                }
                if !self.has_pending()
                    && (response.status == Status::InterSegmentTimeout
                        || self.deadline_monotonic_ms.is_none())
                {
                    let last = self.retry.len() - 1;
                    self.retry[last] = true;
                }
                if self.state != SenderState::AwaitingAccept {
                    self.prepare_next(now);
                }
            }
            Status::CrcError => {
                for index in 0..self.submitted.len() {
                    if self.submitted[index] {
                        self.retry[index] = true;
                    }
                    self.acknowledged[index] = Acknowledged::Unreported;
                }
                if self.state != SenderState::AwaitingAccept {
                    self.prepare_next(now);
                }
            }
            status => self.finish(SenderState::Failed(SenderFailure::Peer(status))),
        }
        self.advance(now)
    }
    pub fn cancel(&mut self, now: u64) -> Result<SenderState, Error> {
        self.advance(now)?;
        if !self.state.closed() {
            self.finish(SenderState::Cancelled);
        }
        Ok(self.state)
    }
}
#[cfg(test)]
mod tests;
