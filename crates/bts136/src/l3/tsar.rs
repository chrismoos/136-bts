mod segmentation;
pub use segmentation::{MessagePlan, PlanningError, SegmentedMessage, plan_message};
mod response;
use super::broadcast::ProtocolVersion;
pub use response::{
    AcknowledgmentMap, DecodedResponse, OptionalServicesSupport, Response, Status, decode_response,
};
use std::{error::Error as StdError, fmt};

const HEADER_OCTETS: usize = 3;
const OPTIONAL_WORD_OCTETS: usize = 2;
const CRC16_IE_OCTETS: usize = 3;
const CRC24_IE_OCTETS: usize = 4;
const PROTOCOL_MASK: u8 = 0xc0;
const TYPE_SHIFT: u32 = 3;
const TYPE_MASK: u8 = 7;
const TRANSACTION_MASK: u8 = 7;
const PARAMETER_SHIFT: u32 = 4;
const PARAMETER_MASK: u8 = 0x0f;
const LENGTH_PARAMETER: u8 = 1;
const SERVICES_PARAMETER: u8 = 2;
const CRC16_PARAMETER: u8 = 2;
const CRC24_PARAMETER: u8 = 3;
const LENGTH_BITS: u32 = 12;
const CRC16_BITS: u32 = 16;
const CRC24_BITS: u32 = 24;
const CRC_RESERVED_BITS: u32 = 4;
const OCTET_BITS: u32 = 8;
const COMPRESSION_SHIFT: u32 = 9;
const ENCRYPTION_SHIFT: u32 = 6;
const WINDOW_SHIFT: u32 = 3;
const CRC_STATUS_SHIFT: u32 = 1;
const SERVICE_FIELD_MASK: u16 = 7;
const CRC_STATUS_MASK: u16 = 3;
const SERVICES_RESERVED_MASK: u16 = 1;
const ENCRYPTION_NONE: u16 = 0;
const BEGIN_TYPE: u8 = 0;
const CONTINUE_TYPE: u8 = 1;
const END_TYPE: u8 = 2;
const RESPONSE_TYPE: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Compression {
    None = 0,
    V42Bis = 1,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AcknowledgmentWindow {
    Prohibited = 0,
    Every4 = 1,
    Every8 = 2,
    Every16 = 3,
    Last = 4,
    Exceptions = 5,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CrcStatus {
    None = 0,
    Crc16 = 1,
    Crc24 = 2,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionalServices {
    pub compression: Compression,
    pub acknowledgment_window: AcknowledgmentWindow,
    pub crc_status: CrcStatus,
}
impl Default for OptionalServices {
    // TIA-136-620-A §3.1 specifies the defaults when the IE is absent.
    fn default() -> Self {
        Self {
            compression: Compression::None,
            acknowledgment_window: AcknowledgmentWindow::Prohibited,
            crc_status: CrcStatus::None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageCrc {
    Crc16(u16),
    Crc24(u32),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Begin {
        message_length: Option<u16>,
        services: Option<OptionalServices>,
    },
    Continue {
        message_length: Option<u16>,
    },
    End {
        message_length: Option<u16>,
        crc: Option<MessageCrc>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub transaction_id: u8,
    pub sequence_number: u8,
    pub segment_data: Vec<u8>,
    pub kind: SegmentKind,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSegment {
    pub segment: Segment,
    /// Unknown optional parameters have no known length, so their complete suffix remains for caller admission.
    pub unparsed_octets: Vec<u8>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Version,
    Truncated,
    Protocol,
    MessageType,
    UnsupportedResponse,
    AmbiguousMapLength,
    Field(&'static str),
    Reserved,
    DuplicateParameter,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid TSAR message: {self:?}")
    }
}
impl StdError for Error {}
fn validate_version(version: ProtocolVersion) -> Result<(), Error> {
    if version.code() < ProtocolVersion::Pv1.code() {
        return Err(Error::Version);
    }
    Ok(())
}
impl Segment {
    // TIA-136-620-A §§3.1–3.3 define an octet-aligned header and segment data, then message-specific optional IEs.
    pub fn encode(&self, version: ProtocolVersion) -> Result<Vec<u8>, Error> {
        validate_version(version)?;
        if self.transaction_id > TRANSACTION_MASK {
            return Err(Error::Field("transaction_id"));
        }
        if self.segment_data.len() > u8::MAX as usize {
            return Err(Error::Field("segment_data_length"));
        }
        let (kind, message_length) = match self.kind {
            SegmentKind::Begin { message_length, .. } => {
                if self.sequence_number != 0 {
                    return Err(Error::Field("begin_sequence_number"));
                }
                (BEGIN_TYPE, message_length)
            }
            SegmentKind::Continue { message_length } => (CONTINUE_TYPE, message_length),
            SegmentKind::End { message_length, .. } => (END_TYPE, message_length),
        };
        let mut bytes = vec![
            (kind << TYPE_SHIFT) | self.transaction_id,
            self.sequence_number,
            self.segment_data.len() as u8,
        ];
        bytes.extend_from_slice(&self.segment_data);
        if let Some(length) = message_length {
            if length >= 1 << LENGTH_BITS {
                return Err(Error::Field("message_length"));
            }
            let field = (u16::from(LENGTH_PARAMETER) << LENGTH_BITS) | length;
            bytes.extend(field.to_be_bytes());
        }
        match self.kind {
            SegmentKind::Begin {
                services: Some(services),
                ..
            } => {
                let field = (u16::from(SERVICES_PARAMETER) << LENGTH_BITS)
                    | ((services.compression as u16) << COMPRESSION_SHIFT)
                    | (ENCRYPTION_NONE << ENCRYPTION_SHIFT)
                    | ((services.acknowledgment_window as u16) << WINDOW_SHIFT)
                    | ((services.crc_status as u16) << CRC_STATUS_SHIFT);
                bytes.extend(field.to_be_bytes());
            }
            SegmentKind::End { crc: Some(crc), .. } => {
                let (parameter, width, value) = match crc {
                    MessageCrc::Crc16(value) => (CRC16_PARAMETER, CRC16_BITS, u32::from(value)),
                    MessageCrc::Crc24(value) => {
                        if value >= 1 << CRC24_BITS {
                            return Err(Error::Field("crc24"));
                        }
                        (CRC24_PARAMETER, CRC24_BITS, value)
                    }
                };
                let field = (u32::from(parameter) << (width + CRC_RESERVED_BITS))
                    | (value << CRC_RESERVED_BITS);
                let count = ((width + CRC_RESERVED_BITS + PARAMETER_SHIFT) / OCTET_BITS) as usize;
                let packed = field.to_be_bytes();
                bytes.extend_from_slice(&packed[packed.len() - count..]);
            }
            _ => {}
        }
        Ok(bytes)
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<u32, Error> {
        let end = self.offset.checked_add(count).ok_or(Error::Truncated)?;
        let bytes = self.bytes.get(self.offset..end).ok_or(Error::Truncated)?;
        self.offset = end;
        Ok(bytes
            .iter()
            .fold(0, |value, byte| (value << OCTET_BITS) | u32::from(*byte)))
    }
}
fn decode_services(field: u16) -> Result<OptionalServices, Error> {
    if field & SERVICES_RESERVED_MASK != 0
        || ((field >> ENCRYPTION_SHIFT) & SERVICE_FIELD_MASK) != ENCRYPTION_NONE
    {
        return Err(Error::Reserved);
    }
    let compression = match (field >> COMPRESSION_SHIFT) & SERVICE_FIELD_MASK {
        0 => Compression::None,
        1 => Compression::V42Bis,
        _ => return Err(Error::Reserved),
    };
    let acknowledgment_window = match (field >> WINDOW_SHIFT) & SERVICE_FIELD_MASK {
        0 => AcknowledgmentWindow::Prohibited,
        1 => AcknowledgmentWindow::Every4,
        2 => AcknowledgmentWindow::Every8,
        3 => AcknowledgmentWindow::Every16,
        4 => AcknowledgmentWindow::Last,
        5 => AcknowledgmentWindow::Exceptions,
        _ => return Err(Error::Reserved),
    };
    let crc_status = match (field >> CRC_STATUS_SHIFT) & CRC_STATUS_MASK {
        0 => CrcStatus::None,
        1 => CrcStatus::Crc16,
        2 => CrcStatus::Crc24,
        _ => return Err(Error::Reserved),
    };
    Ok(OptionalServices {
        compression,
        acknowledgment_window,
        crc_status,
    })
}
pub fn decode_segment(bytes: &[u8], version: ProtocolVersion) -> Result<DecodedSegment, Error> {
    validate_version(version)?;
    let header = *bytes.first().ok_or(Error::Truncated)?;
    if header & PROTOCOL_MASK != 0 {
        return Err(Error::Protocol);
    }
    let kind = (header >> TYPE_SHIFT) & TYPE_MASK;
    match kind {
        RESPONSE_TYPE => return Err(Error::UnsupportedResponse),
        BEGIN_TYPE | CONTINUE_TYPE | END_TYPE => {}
        _ => return Err(Error::MessageType),
    };
    if bytes.len() < HEADER_OCTETS {
        return Err(Error::Truncated);
    }
    let sequence_number = bytes[1];
    if kind == BEGIN_TYPE && sequence_number != 0 {
        return Err(Error::Field("begin_sequence_number"));
    }
    let end = HEADER_OCTETS + usize::from(bytes[2]);
    let segment_data = bytes
        .get(HEADER_OCTETS..end)
        .ok_or(Error::Truncated)?
        .to_vec();
    let mut reader = Reader { bytes, offset: end };
    let mut message_length = None;
    let mut services = None;
    let mut crc = None;
    while let Some(first) = bytes.get(reader.offset) {
        let parameter = first >> PARAMETER_SHIFT;
        match parameter {
            LENGTH_PARAMETER => {
                if message_length.is_some() {
                    return Err(Error::DuplicateParameter);
                }
                message_length =
                    Some((reader.take(OPTIONAL_WORD_OCTETS)? as u16) & ((1 << LENGTH_BITS) - 1));
            }
            SERVICES_PARAMETER if kind == BEGIN_TYPE => {
                if services.is_some() {
                    return Err(Error::DuplicateParameter);
                }
                services = Some(decode_services(reader.take(OPTIONAL_WORD_OCTETS)? as u16)?);
            }
            CRC16_PARAMETER | CRC24_PARAMETER if kind == END_TYPE => {
                if crc.is_some() {
                    return Err(Error::DuplicateParameter);
                }
                let count = if parameter == CRC16_PARAMETER {
                    CRC16_IE_OCTETS
                } else {
                    CRC24_IE_OCTETS
                };
                let field = reader.take(count)?;
                if field & u32::from(PARAMETER_MASK) != 0 {
                    return Err(Error::Reserved);
                }
                let value = (field >> CRC_RESERVED_BITS)
                    & if parameter == CRC16_PARAMETER {
                        (1 << CRC16_BITS) - 1
                    } else {
                        (1 << CRC24_BITS) - 1
                    };
                crc = Some(if parameter == CRC16_PARAMETER {
                    MessageCrc::Crc16(value as u16)
                } else {
                    MessageCrc::Crc24(value)
                });
            }
            _ => break,
        }
    }
    let kind = match kind {
        BEGIN_TYPE => SegmentKind::Begin {
            message_length,
            services,
        },
        CONTINUE_TYPE => SegmentKind::Continue { message_length },
        END_TYPE => SegmentKind::End {
            message_length,
            crc,
        },
        _ => unreachable!("segment kind validated before field decoding"),
    };
    Ok(DecodedSegment {
        segment: Segment {
            transaction_id: header & TRANSACTION_MASK,
            sequence_number,
            segment_data,
            kind,
        },
        unparsed_octets: bytes[reader.offset..].to_vec(),
    })
}
#[cfg(test)]
mod tests;
