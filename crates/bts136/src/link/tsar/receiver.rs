use crate::{
    l3::{
        broadcast::ProtocolVersion,
        relay::HigherLayerProtocol,
        tsar::{
            self, AcknowledgmentMap, AcknowledgmentWindow, Compression, CrcStatus, DecodedSegment,
            OptionalServices, OptionalServicesSupport, Response, Segment, SegmentKind, Status,
        },
    },
    link::rach::MobileIdentity,
};
use std::{error::Error as StdError, fmt};

// TIA-136-620-A §§2.2,2.3,5 fix REASSEM_TMR at 300 seconds.
const REASSEMBLY_MS: u64 = 300_000;
const SEQUENCE_COUNT: usize = 256;
const SEGMENTED_CARRIER_BITS: u8 = 0xc0;
const WINDOW_FOUR: usize = 4;
const WINDOW_EIGHT: usize = 8;
const WINDOW_SIXTEEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverState {
    AwaitingBegin,
    Receiving,
    Complete,
    Failed(Status),
    Expired,
    Canceled,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reception {
    pub response: Option<Response>,
    pub message: Option<Vec<u8>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveError {
    Identity,
    Service,
    TransactionId,
    Codec(tsar::Error),
    ClockRegression,
    TimeOverflow,
    UnparsedSegment,
    BeginRequired,
    TimerDue,
    Closed,
}
impl fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TSAR receiver failed: {self:?}")
    }
}
impl StdError for ReceiveError {}

/// One peer/service transaction. Dispatch received segments in timestamp order before advancing timers. Retain completed ownership until transaction-ID reuse is safe.
pub struct Receiver {
    peer: MobileIdentity,
    protocol: HigherLayerProtocol,
    transaction_id: u8,
    version: ProtocolVersion,
    maximum_message_octets: usize,
    state: ReceiverState,
    services: OptionalServices,
    segments: Vec<Option<Segment>>,
    end_sequence: Option<u8>,
    message_length: Option<u16>,
    stored_octets: usize,
    deadline_monotonic_ms: Option<u64>,
    last_monotonic_ms: u64,
}
impl Receiver {
    pub fn try_new(
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        transaction_id: u8,
        version: ProtocolVersion,
        maximum_message_octets: usize,
        monotonic_ms: u64,
    ) -> Result<Self, ReceiveError> {
        let (_, width, value) = peer.parts();
        if value == 0 || value >= 1 << width {
            return Err(ReceiveError::Identity);
        }
        if !matches!(protocol, HigherLayerProtocol::Segmented(_))
            && !matches!(protocol, HigherLayerProtocol::CarrierSpecific(raw) if raw & SEGMENTED_CARRIER_BITS == SEGMENTED_CARRIER_BITS)
        {
            return Err(ReceiveError::Service);
        }
        Segment {
            transaction_id,
            sequence_number: 0,
            segment_data: Vec::new(),
            kind: SegmentKind::Begin {
                message_length: None,
                services: None,
            },
        }
        .encode(version)
        .map_err(ReceiveError::Codec)?;
        Ok(Self {
            peer,
            protocol,
            transaction_id,
            version,
            maximum_message_octets,
            state: ReceiverState::AwaitingBegin,
            services: OptionalServices::default(),
            segments: vec![None; SEQUENCE_COUNT],
            end_sequence: None,
            message_length: None,
            stored_octets: 0,
            deadline_monotonic_ms: None,
            last_monotonic_ms: monotonic_ms,
        })
    }
    pub fn state(&self) -> ReceiverState {
        self.state
    }
    pub fn deadline_monotonic_ms(&self) -> Option<u64> {
        self.deadline_monotonic_ms
    }
    pub fn stored_octets(&self) -> usize {
        self.stored_octets
    }
    fn clock(&self, now: u64) -> Result<(), ReceiveError> {
        if now < self.last_monotonic_ms {
            return Err(ReceiveError::ClockRegression);
        }
        Ok(())
    }
    fn response(&self, status: Status, include_missing_next: bool) -> Option<Response> {
        if self.services.acknowledgment_window == AcknowledgmentWindow::Prohibited {
            return None;
        }
        let last = self.end_sequence.map(usize::from).unwrap_or_else(|| {
            let highest = self.segments.iter().rposition(Option::is_some).unwrap_or(0);
            if include_missing_next {
                (highest + 1).min(SEQUENCE_COUNT - 1)
            } else {
                highest
            }
        });
        Some(Response {
            transaction_id: self.transaction_id,
            status,
            acknowledgments: AcknowledgmentMap::try_new(
                self.segments[..=last].iter().map(Option::is_some).collect(),
            )
            .expect("bounded sequence space produces a nonempty acknowledgment map"),
            optional_services_support: None,
        })
    }
    fn fail(&mut self, status: Status) -> Reception {
        let response = self.response(status, false);
        self.state = ReceiverState::Failed(status);
        self.deadline_monotonic_ms = None;
        self.clear_data();
        Reception {
            response,
            message: None,
        }
    }
    fn clear_data(&mut self) {
        self.segments.fill(None);
        self.stored_octets = 0;
    }
    pub fn advance(&mut self, now: u64) -> Result<Option<Response>, ReceiveError> {
        self.clock(now)?;
        self.last_monotonic_ms = now;
        if !self
            .deadline_monotonic_ms
            .is_some_and(|deadline| now >= deadline)
        {
            return Ok(None);
        }
        self.deadline_monotonic_ms = None;
        if self.services.acknowledgment_window == AcknowledgmentWindow::Prohibited {
            self.state = ReceiverState::Expired;
            self.clear_data();
            return Ok(None);
        }
        Ok(self.response(Status::InterSegmentTimeout, true))
    }
    pub fn cancel(&mut self, now: u64) -> Result<(), ReceiveError> {
        self.clock(now)?;
        if !matches!(
            self.state,
            ReceiverState::AwaitingBegin | ReceiverState::Receiving
        ) {
            return Err(ReceiveError::Closed);
        }
        self.last_monotonic_ms = now;
        self.deadline_monotonic_ms = None;
        self.state = ReceiverState::Canceled;
        self.clear_data();
        Ok(())
    }
    pub fn receive(
        &mut self,
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        decoded: &DecodedSegment,
        now: u64,
    ) -> Result<Reception, ReceiveError> {
        self.clock(now)?;
        if peer != self.peer {
            return Err(ReceiveError::Identity);
        }
        if protocol != self.protocol {
            return Err(ReceiveError::Service);
        }
        let segment = &decoded.segment;
        if segment.transaction_id != self.transaction_id {
            return Err(ReceiveError::TransactionId);
        }
        segment.encode(self.version).map_err(ReceiveError::Codec)?;
        if !decoded.unparsed_octets.is_empty() {
            return Err(ReceiveError::UnparsedSegment);
        }
        if !matches!(
            self.state,
            ReceiverState::AwaitingBegin | ReceiverState::Receiving | ReceiverState::Complete
        ) {
            return Err(ReceiveError::Closed);
        }
        if self.state == ReceiverState::AwaitingBegin
            && !matches!(segment.kind, SegmentKind::Begin { .. })
        {
            return Err(ReceiveError::BeginRequired);
        }
        let deadline = now
            .checked_add(REASSEMBLY_MS)
            .ok_or(ReceiveError::TimeOverflow)?;
        // A segment at the timer boundary wins only when its observation precedes timer advancement.
        if self
            .deadline_monotonic_ms
            .is_some_and(|cutoff| now > cutoff)
        {
            if self.services.acknowledgment_window == AcknowledgmentWindow::Prohibited {
                self.advance(now)?;
                return Err(ReceiveError::Closed);
            }
            return Err(ReceiveError::TimerDue);
        }
        self.last_monotonic_ms = now;
        let index = usize::from(segment.sequence_number);
        if let Some(existing) = &self.segments[index] {
            if existing != segment {
                let status = if existing.segment_data.len() != segment.segment_data.len() {
                    Status::DuplicateSizeMismatch
                } else {
                    Status::Aborted
                };
                return Ok(self.fail(status));
            }
            let status = if self.state == ReceiverState::Complete {
                Status::Received
            } else {
                Status::Continuing
            };
            if self.state == ReceiverState::Receiving {
                self.deadline_monotonic_ms = Some(deadline);
            }
            return Ok(Reception {
                response: self.required_response(segment, status),
                message: None,
            });
        }
        if self.state == ReceiverState::Complete {
            return Err(ReceiveError::Closed);
        }
        if matches!(segment.kind, SegmentKind::Continue { .. }) && index == SEQUENCE_COUNT - 1 {
            return Ok(self.fail(Status::Aborted));
        }
        let length = match segment.kind {
            SegmentKind::Begin {
                message_length,
                services,
            } => {
                self.services = services.unwrap_or_default();
                self.state = ReceiverState::Receiving;
                let compression = self.services.compression != Compression::None;
                let crc = self.services.crc_status != CrcStatus::None;
                if compression || crc {
                    let status = match (compression, crc) {
                        (true, true) => Status::MultipleServicesUnsupported,
                        (true, false) => Status::CompressionUnsupported,
                        _ => Status::CrcSelectionUnsupported,
                    };
                    let mut result = self.fail(status);
                    if status == Status::MultipleServicesUnsupported
                        && self.version.code() >= ProtocolVersion::Pv5.code()
                        && let Some(response) = &mut result.response
                    {
                        response.optional_services_support = Some(OptionalServicesSupport {
                            compression: false,
                            encryption: true,
                            acknowledgment_window: true,
                            crc: false,
                        });
                    }
                    return Ok(result);
                }
                message_length
            }
            SegmentKind::Continue { message_length } => message_length,
            SegmentKind::End {
                message_length,
                crc,
            } => {
                if crc.is_some() {
                    return Ok(self.fail(Status::CrcSelectionUnsupported));
                }
                if index == 0
                    || self
                        .end_sequence
                        .is_some_and(|end| end != segment.sequence_number)
                    || self.segments[index + 1..].iter().any(Option::is_some)
                {
                    return Ok(self.fail(Status::Aborted));
                }
                message_length
            }
        };
        if self
            .end_sequence
            .is_some_and(|end| segment.sequence_number > end)
        {
            return Ok(self.fail(Status::Aborted));
        }
        if let Some(length) = length {
            if self
                .message_length
                .is_some_and(|existing| existing != length)
            {
                return Ok(self.fail(Status::MessageLengthError));
            }
            self.message_length = Some(length);
        }
        if self
            .message_length
            .is_some_and(|length| usize::from(length) > self.maximum_message_octets)
            || segment.segment_data.len()
                > self
                    .maximum_message_octets
                    .saturating_sub(self.stored_octets)
        {
            return Ok(self.fail(Status::MessageTooLong));
        }
        if self.message_length.is_some_and(|length| {
            self.stored_octets + segment.segment_data.len() > usize::from(length)
        }) {
            return Ok(self.fail(Status::MessageLengthError));
        }
        self.stored_octets += segment.segment_data.len();
        self.segments[index] = Some(segment.clone());
        if matches!(segment.kind, SegmentKind::End { .. }) {
            self.end_sequence = Some(segment.sequence_number);
        }
        if let Some(end) = self.end_sequence
            && self.segments[..=usize::from(end)]
                .iter()
                .all(Option::is_some)
        {
            if self
                .message_length
                .is_some_and(|length| usize::from(length) != self.stored_octets)
            {
                return Ok(self.fail(Status::MessageLengthError));
            }
            let message = self.segments[..=usize::from(end)]
                .iter()
                .filter_map(Option::as_ref)
                .flat_map(|segment| segment.segment_data.iter().copied())
                .collect();
            self.state = ReceiverState::Complete;
            self.deadline_monotonic_ms = None;
            return Ok(Reception {
                response: self.required_response(segment, Status::Received),
                message: Some(message),
            });
        }
        self.deadline_monotonic_ms = Some(deadline);
        Ok(Reception {
            response: self.required_response(segment, Status::Continuing),
            message: None,
        })
    }
    fn required_response(&self, segment: &Segment, status: Status) -> Option<Response> {
        let periodic = match self.services.acknowledgment_window {
            AcknowledgmentWindow::Every4 => Some(WINDOW_FOUR),
            AcknowledgmentWindow::Every8 => Some(WINDOW_EIGHT),
            AcknowledgmentWindow::Every16 => Some(WINDOW_SIXTEEN),
            _ => None,
        };
        let enabled = !matches!(
            self.services.acknowledgment_window,
            AcknowledgmentWindow::Prohibited | AcknowledgmentWindow::Exceptions
        );
        if enabled
            && (status == Status::Received
                || matches!(segment.kind, SegmentKind::End { .. })
                || periodic
                    .is_some_and(|window| (usize::from(segment.sequence_number) + 1) % window == 0))
        {
            self.response(status, false)
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests;
