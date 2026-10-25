use super::{Error, ProtocolVersion, RESPONSE_TYPE, validate_version};
use crate::l3::control_fields::{self, Reader, bits, pack, put};

const PROTOCOL_BITS: usize = 2;
const TSAR_PROTOCOL: u64 = 0;
const UNRESOLVED_MAP_LENGTH: usize = 0;
const TYPE_BITS: usize = 3;
const TRANSACTION_BITS: usize = 3;
const STATUS_BITS: usize = 8;
const MAP_LENGTH_BITS: usize = 8;
const OCTET_BITS: usize = 8;
const MAX_MAP_BITS: usize = 256;
const SUPPORT_PARAMETER_BITS: usize = 4;
const SUPPORT_MAP_BITS: usize = 8;
const SUPPORT_PARAMETER: u64 = 1;
const SUPPORT_RESERVED_MASK: u8 = 0xf0;
const COMPRESSION_SUPPORT: u8 = 1;
const ENCRYPTION_SUPPORT: u8 = 2;
const WINDOW_SUPPORT: u8 = 4;
const CRC_SUPPORT: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Continuing = 0x00,
    InterSegmentTimeout = 0x01,
    Aborted = 0x02,
    CrcSelectionUnsupported = 0x03,
    CompressionUnsupported = 0x04,
    EncryptionUnsupported = 0x05,
    AcknowledgmentWindowUnsupported = 0x06,
    ProtocolUnsupported = 0x07,
    MessageLengthError = 0x08,
    MessageTooLong = 0x09,
    Received = 0x0a,
    DuplicateSizeMismatch = 0x0b,
    CrcError = 0x0c,
    MultipleServicesUnsupported = 0x0d,
}
impl Status {
    fn decode(raw: u8) -> Result<Self, Error> {
        match raw {
            0x00 => Ok(Self::Continuing),
            0x01 => Ok(Self::InterSegmentTimeout),
            0x02 => Ok(Self::Aborted),
            0x03 => Ok(Self::CrcSelectionUnsupported),
            0x04 => Ok(Self::CompressionUnsupported),
            0x05 => Ok(Self::EncryptionUnsupported),
            0x06 => Ok(Self::AcknowledgmentWindowUnsupported),
            0x07 => Ok(Self::ProtocolUnsupported),
            0x08 => Ok(Self::MessageLengthError),
            0x09 => Ok(Self::MessageTooLong),
            0x0a => Ok(Self::Received),
            0x0b => Ok(Self::DuplicateSizeMismatch),
            0x0c => Ok(Self::CrcError),
            0x0d => Ok(Self::MultipleServicesUnsupported),
            _ => Err(Error::Reserved),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcknowledgmentMap {
    received: Vec<bool>,
}
impl AcknowledgmentMap {
    pub fn try_new(received: Vec<bool>) -> Result<Self, Error> {
        if !(1..=MAX_MAP_BITS).contains(&received.len()) {
            return Err(Error::Field("acknowledgment_map_length"));
        }
        Ok(Self { received })
    }
    pub fn bit_count(&self) -> usize {
        self.received.len()
    }
    pub fn received(&self, sequence_number: u8) -> Option<bool> {
        self.received.get(usize::from(sequence_number)).copied()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionalServicesSupport {
    pub compression: bool,
    pub encryption: bool,
    pub acknowledgment_window: bool,
    pub crc: bool,
}
impl OptionalServicesSupport {
    fn code(self) -> u8 {
        (u8::from(self.compression) * COMPRESSION_SUPPORT)
            | (u8::from(self.encryption) * ENCRYPTION_SUPPORT)
            | (u8::from(self.acknowledgment_window) * WINDOW_SUPPORT)
            | (u8::from(self.crc) * CRC_SUPPORT)
    }
    fn decode(raw: u8) -> Result<Self, Error> {
        if raw & SUPPORT_RESERVED_MASK != 0 {
            return Err(Error::Reserved);
        }
        Ok(Self {
            compression: raw & COMPRESSION_SUPPORT != 0,
            encryption: raw & ENCRYPTION_SUPPORT != 0,
            acknowledgment_window: raw & WINDOW_SUPPORT != 0,
            crc: raw & CRC_SUPPORT != 0,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub transaction_id: u8,
    pub status: Status,
    pub acknowledgments: AcknowledgmentMap,
    pub optional_services_support: Option<OptionalServicesSupport>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedResponse {
    pub response: Response,
    /// Unknown optional parameters retain their complete bit suffix because their lengths are not defined here.
    pub unparsed_bits: Vec<bool>,
}
fn wire_error(error: control_fields::Error) -> Error {
    match error {
        control_fields::Error::Truncated { .. } => Error::Truncated,
        control_fields::Error::Length => Error::Field("response_length"),
        control_fields::Error::Protocol => Error::Protocol,
        control_fields::Error::MessageType => Error::MessageType,
        control_fields::Error::Field(field) => Error::Field(field),
    }
}
impl Response {
    pub(crate) fn validate(&self, version: ProtocolVersion) -> Result<(), Error> {
        validate_version(version)?;
        if self.transaction_id >= 1 << TRANSACTION_BITS {
            return Err(Error::Field("transaction_id"));
        }
        if self.optional_services_support.is_some() && version.code() < ProtocolVersion::Pv5.code()
        {
            return Err(Error::Version);
        }
        if self.optional_services_support.is_some()
            && self.status != Status::MultipleServicesUnsupported
        {
            return Err(Error::Field("optional_services_support_status"));
        }
        Ok(())
    }
    pub fn encode(&self, version: ProtocolVersion) -> Result<Vec<u8>, Error> {
        self.validate(version)?;
        if self.acknowledgments.bit_count() == MAX_MAP_BITS {
            return Err(Error::AmbiguousMapLength);
        }
        let mut encoded = Vec::new();
        for (value, width) in [
            (TSAR_PROTOCOL, PROTOCOL_BITS),
            (u64::from(RESPONSE_TYPE), TYPE_BITS),
            (u64::from(self.transaction_id), TRANSACTION_BITS),
            (self.status as u64, STATUS_BITS),
            (self.acknowledgments.bit_count() as u64, MAP_LENGTH_BITS),
        ] {
            put(&mut encoded, value, width).map_err(wire_error)?;
        }
        // TIA-136-620-A §4.8 puts sequence zero at the LSB of the first map octet and transmits only the specified map bits.
        for group in self.acknowledgments.received.chunks(OCTET_BITS) {
            encoded.extend(group.iter().rev().copied());
        }
        if let Some(support) = self.optional_services_support {
            put(&mut encoded, SUPPORT_PARAMETER, SUPPORT_PARAMETER_BITS).map_err(wire_error)?;
            put(&mut encoded, u64::from(support.code()), SUPPORT_MAP_BITS).map_err(wire_error)?;
        }
        pack(encoded).map_err(wire_error)
    }
}
pub fn decode_response(bytes: &[u8], version: ProtocolVersion) -> Result<DecodedResponse, Error> {
    validate_version(version)?;
    let encoded = bits(bytes).map_err(wire_error)?;
    let mut reader = Reader {
        bits: &encoded,
        offset: 0,
    };
    if reader.take(PROTOCOL_BITS).map_err(wire_error)? != TSAR_PROTOCOL {
        return Err(Error::Protocol);
    }
    if reader.take(TYPE_BITS).map_err(wire_error)? != u64::from(RESPONSE_TYPE) {
        return Err(Error::MessageType);
    }
    let transaction_id = reader.take(TRANSACTION_BITS).map_err(wire_error)? as u8;
    let status = Status::decode(reader.take(STATUS_BITS).map_err(wire_error)? as u8)?;
    let count = reader.take(MAP_LENGTH_BITS).map_err(wire_error)? as usize;
    if count == UNRESOLVED_MAP_LENGTH {
        return Err(Error::AmbiguousMapLength);
    }
    let end = reader.offset + count;
    let map = encoded.get(reader.offset..end).ok_or(Error::Truncated)?;
    let received = map
        .chunks(OCTET_BITS)
        .flat_map(|group| group.iter().rev().copied())
        .collect();
    reader.offset = end;
    let mut optional_services_support = None;
    while reader.offset < encoded.len() {
        let remaining = encoded.len() - reader.offset;
        if remaining < OCTET_BITS && encoded[reader.offset..].iter().all(|bit| !*bit) {
            reader.offset = encoded.len();
            break;
        }
        if remaining < SUPPORT_PARAMETER_BITS {
            return Err(Error::Reserved);
        }
        let start = reader.offset;
        let parameter = reader.take(SUPPORT_PARAMETER_BITS).map_err(wire_error)?;
        if parameter != SUPPORT_PARAMETER {
            reader.offset = start;
            break;
        }
        if optional_services_support.is_some() {
            return Err(Error::DuplicateParameter);
        }
        if version.code() < ProtocolVersion::Pv5.code() {
            return Err(Error::Version);
        }
        if status != Status::MultipleServicesUnsupported {
            return Err(Error::Field("optional_services_support_status"));
        }
        optional_services_support = Some(OptionalServicesSupport::decode(
            reader.take(SUPPORT_MAP_BITS).map_err(wire_error)? as u8,
        )?);
    }
    Ok(DecodedResponse {
        response: Response {
            transaction_id,
            status,
            acknowledgments: AcknowledgmentMap::try_new(received)?,
            optional_services_support,
        },
        unparsed_bits: encoded[reader.offset..].to_vec(),
    })
}
#[cfg(test)]
mod tests;
