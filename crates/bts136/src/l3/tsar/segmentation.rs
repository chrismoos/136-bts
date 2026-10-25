use super::{
    AcknowledgmentWindow, Compression, CrcStatus, Error as CodecError, HEADER_OCTETS, LENGTH_BITS,
    OPTIONAL_WORD_OCTETS, OptionalServices, ProtocolVersion, Segment, SegmentKind,
};
use crate::l3::relay::MAX_DATA_PAYLOAD_OCTETS;
use std::{error::Error as StdError, fmt};

const FIRST_SEQUENCE_NUMBER: u8 = 0;
const MAX_SEGMENTS: usize = u8::MAX as usize + 1;
const MAX_SEGMENT_DATA_OCTETS: usize = u8::MAX as usize;
const MAX_LENGTH_IE_MESSAGE_OCTETS: usize = (1 << LENGTH_BITS) - 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessagePlan {
    Unsegmented(Vec<u8>),
    Segmented(SegmentedMessage),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentedMessage {
    segments: Vec<Segment>,
    message_octets: usize,
    payload_budget_octets: usize,
    acknowledgment_window: AcknowledgmentWindow,
}
impl SegmentedMessage {
    /// Restore exact transmitted boundaries without rerunning the segmentation planner.
    pub fn try_from_frozen(
        segments: Vec<Segment>,
        message: &[u8],
        payload_budget_octets: usize,
        version: ProtocolVersion,
    ) -> Result<Self, PlanningError> {
        if payload_budget_octets > MAX_DATA_PAYLOAD_OCTETS {
            return Err(PlanningError::PayloadBudget);
        }
        if !(2..=MAX_SEGMENTS).contains(&segments.len()) {
            return Err(PlanningError::SegmentCount);
        }
        let transaction_id = segments[0].transaction_id;
        let SegmentKind::Begin { services, .. } = segments[0].kind else {
            return Err(PlanningError::FrozenPlan);
        };
        let services = services.unwrap_or_default();
        if services.compression != Compression::None || services.crc_status != CrcStatus::None {
            return Err(PlanningError::FrozenPlan);
        }
        let mut offset = 0usize;
        for (index, segment) in segments.iter().enumerate() {
            let encoded = segment.encode(version).map_err(PlanningError::Codec)?;
            if encoded.len() > payload_budget_octets {
                return Err(PlanningError::PayloadBudget);
            }
            if segment.transaction_id != transaction_id || segment.sequence_number as usize != index
            {
                return Err(PlanningError::FrozenPlan);
            }
            let length = match segment.kind {
                SegmentKind::Begin { message_length, .. } if index == 0 => message_length,
                SegmentKind::Continue { message_length }
                    if index > 0 && index + 1 < segments.len() =>
                {
                    message_length
                }
                SegmentKind::End {
                    message_length,
                    crc: None,
                } if index + 1 == segments.len() => message_length,
                _ => return Err(PlanningError::FrozenPlan),
            };
            if length.is_some_and(|length| usize::from(length) != message.len()) {
                return Err(PlanningError::FrozenPlan);
            }
            let end = offset
                .checked_add(segment.segment_data.len())
                .ok_or(PlanningError::FrozenPlan)?;
            if message.get(offset..end) != Some(segment.segment_data.as_slice()) {
                return Err(PlanningError::FrozenPlan);
            }
            offset = end;
        }
        if offset != message.len() {
            return Err(PlanningError::FrozenPlan);
        }
        Ok(Self {
            segments,
            message_octets: message.len(),
            payload_budget_octets,
            acknowledgment_window: services.acknowledgment_window,
        })
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }
    pub fn message_octets(&self) -> usize {
        self.message_octets
    }
    pub fn payload_budget_octets(&self) -> usize {
        self.payload_budget_octets
    }
    pub fn acknowledgment_window(&self) -> AcknowledgmentWindow {
        self.acknowledgment_window
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningError {
    PayloadBudget,
    HeaderCapacity,
    SegmentCount,
    FrozenPlan,
    Codec(CodecError),
}
impl fmt::Display for PlanningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TSAR segmentation failed: {self:?}")
    }
}
impl StdError for PlanningError {}

/// Supply the teleservice-payload budget after transport addressing and HLPI overhead. This does not compute the DCCH access/network limit.
pub fn plan_message(
    message: &[u8],
    payload_budget_octets: usize,
    transaction_id: u8,
    acknowledgment_window: AcknowledgmentWindow,
    version: ProtocolVersion,
) -> Result<MessagePlan, PlanningError> {
    if payload_budget_octets > MAX_DATA_PAYLOAD_OCTETS {
        return Err(PlanningError::PayloadBudget);
    }
    // TIA-136-620-A §2.1 step 3 bypasses TSAR unchanged when the teleservice message fits.
    if message.len() <= payload_budget_octets {
        return Ok(MessagePlan::Unsegmented(message.to_vec()));
    }
    let services =
        (acknowledgment_window != AcknowledgmentWindow::Prohibited).then_some(OptionalServices {
            acknowledgment_window,
            ..OptionalServices::default()
        });
    let mut begin = Segment {
        transaction_id,
        sequence_number: FIRST_SEQUENCE_NUMBER,
        segment_data: Vec::new(),
        kind: SegmentKind::Begin {
            message_length: None,
            services,
        },
    };
    let minimum_begin_octets = begin.encode(version).map_err(PlanningError::Codec)?.len();
    if minimum_begin_octets > payload_budget_octets {
        return Err(PlanningError::HeaderCapacity);
    }
    let later_capacity = payload_budget_octets
        .checked_sub(HEADER_OCTETS)
        .filter(|capacity| *capacity != 0)
        .ok_or(PlanningError::HeaderCapacity)?
        .min(MAX_SEGMENT_DATA_OCTETS);
    // TIA-136-620-A §4.11 makes original length optional and limits only that IE to 12 bits.
    let message_length = if message.len() <= MAX_LENGTH_IE_MESSAGE_OCTETS
        && payload_budget_octets > minimum_begin_octets + OPTIONAL_WORD_OCTETS
    {
        Some(message.len() as u16)
    } else {
        None
    };
    begin.kind = SegmentKind::Begin {
        message_length,
        services,
    };
    let begin_octets = begin.encode(version).map_err(PlanningError::Codec)?.len();
    let first_capacity = (payload_budget_octets - begin_octets).min(MAX_SEGMENT_DATA_OCTETS);
    let following = (message.len() - first_capacity).div_ceil(later_capacity);
    let count = following
        .checked_add(1)
        .filter(|count| *count <= MAX_SEGMENTS)
        .ok_or(PlanningError::SegmentCount)?;
    begin.segment_data = message[..first_capacity].to_vec();
    let mut segments = Vec::with_capacity(count);
    segments.push(begin);
    for (index, data) in message[first_capacity..].chunks(later_capacity).enumerate() {
        let sequence = index + 1;
        let kind = if sequence == count - 1 {
            SegmentKind::End {
                message_length: None,
                crc: None,
            }
        } else {
            SegmentKind::Continue {
                message_length: None,
            }
        };
        segments.push(Segment {
            transaction_id,
            sequence_number: sequence as u8,
            segment_data: data.to_vec(),
            kind,
        });
    }
    for segment in &segments {
        if segment.encode(version).map_err(PlanningError::Codec)?.len() > payload_budget_octets {
            return Err(PlanningError::PayloadBudget);
        }
    }
    Ok(MessagePlan::Segmented(SegmentedMessage {
        segments,
        message_octets: message.len(),
        payload_budget_octets,
        acknowledgment_window,
    }))
}
#[cfg(test)]
mod tests;
