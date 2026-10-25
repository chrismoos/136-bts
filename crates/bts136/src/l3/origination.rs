use super::{
    address::{self, AddressError, AddressInfo},
    broadcast::ProtocolVersion,
    page_response::ServiceCode,
};
use std::{error::Error, fmt};

const PROTOCOL_BITS: usize = 2;
const TYPE_BITS: usize = 6;
const VERSION_BITS: usize = 4;
const FLAG_BITS: usize = 1;
const SCM_BITS: usize = 5;
const SERVICE_BITS: usize = 4;
const OCTET_BITS: usize = 8;
const ORIGINATION: u64 = 7;
const IS136_PROTOCOL: u64 = 0;
const MAX_MESSAGE_OCTETS: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origination {
    pub protocol_version: ProtocolVersion,
    pub emergency_call: bool,
    pub called_party: AddressInfo,
    pub last_try: bool,
    pub scm: u8,
    pub service_code: ServiceCode,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedOrigination {
    pub message: Origination,
    pub consumed_bits: usize,
    /// Optional information elements and final octet padding remain uninterpreted.
    pub remaining_bits: Vec<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginationError {
    Truncated { needed_bits: usize },
    Protocol,
    MessageType,
    Field(&'static str),
    Address(AddressError),
    RequiredDataOptions,
    Length,
}
impl fmt::Display for OriginationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid IS-136 Origination: {self:?}")
    }
}
impl Error for OriginationError {}
impl From<AddressError> for OriginationError {
    fn from(error: AddressError) -> Self {
        Self::Address(error)
    }
}
impl Origination {
    // IS-136.1-A §6.4.4.7. Called Party Number begins after 13 bits, without octet alignment.
    pub fn encode(&self) -> Result<Vec<u8>, OriginationError> {
        if matches!(
            self.service_code,
            ServiceCode::AsyncData | ServiceCode::G3Fax
        ) {
            return Err(OriginationError::RequiredDataOptions);
        }
        self.encode_mandatory()
    }
    pub(crate) fn encode_mandatory(&self) -> Result<Vec<u8>, OriginationError> {
        if self.scm >= 1 << SCM_BITS {
            return Err(OriginationError::Field("scm"));
        }
        let mut bits = Vec::new();
        for (value, width) in [
            (IS136_PROTOCOL, PROTOCOL_BITS),
            (ORIGINATION, TYPE_BITS),
            (u64::from(self.protocol_version.code()), VERSION_BITS),
            (u64::from(self.emergency_call), FLAG_BITS),
        ] {
            put(&mut bits, value, width);
        }
        for octet in self.called_party.encode()? {
            put(&mut bits, u64::from(octet), OCTET_BITS);
        }
        for (value, width) in [
            (u64::from(self.last_try), FLAG_BITS),
            (u64::from(self.scm), SCM_BITS),
            (self.service_code as u64, SERVICE_BITS),
        ] {
            put(&mut bits, value, width);
        }
        bits.resize(bits.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
        if bits.len() > MAX_MESSAGE_OCTETS * OCTET_BITS {
            return Err(OriginationError::Length);
        }
        Ok(bits
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|octet| {
                octet
                    .iter()
                    .fold(0, |value, bit| value * 2 + u8::from(*bit))
            })
            .collect())
    }
}
pub fn decode_prefix(octets: &[u8]) -> Result<DecodedOrigination, OriginationError> {
    let bits: Vec<bool> = octets
        .iter()
        .flat_map(|octet| {
            (0..OCTET_BITS)
                .rev()
                .map(move |bit| octet & (1 << bit) != 0)
        })
        .collect();
    let mut reader = Reader {
        bits: &bits,
        offset: 0,
    };
    if reader.take(PROTOCOL_BITS)? != IS136_PROTOCOL {
        return Err(OriginationError::Protocol);
    }
    if reader.take(TYPE_BITS)? != ORIGINATION {
        return Err(OriginationError::MessageType);
    }
    let protocol_version = ProtocolVersion::decode(reader.take(VERSION_BITS)? as u8)
        .map_err(|_| OriginationError::Field("protocol_version"))?;
    let emergency_call = reader.take(FLAG_BITS)? != 0;
    let length = reader.take(OCTET_BITS)? as u8;
    let mut address_bytes = vec![length];
    for _ in 0..length {
        address_bytes.push(reader.take(OCTET_BITS)? as u8);
    }
    let called_party = address::decode_prefix(&address_bytes)?.address;
    let last_try = reader.take(FLAG_BITS)? != 0;
    let scm = reader.take(SCM_BITS)? as u8;
    let service_code = ServiceCode::decode(reader.take(SERVICE_BITS)? as usize)
        .map_err(|_| OriginationError::Field("service_code"))?;
    Ok(DecodedOrigination {
        message: Origination {
            protocol_version,
            emergency_call,
            called_party,
            last_try,
            scm,
            service_code,
        },
        consumed_bits: reader.offset,
        remaining_bits: bits[reader.offset..].to_vec(),
    })
}
fn put(bits: &mut Vec<bool>, value: u64, width: usize) {
    bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
}
struct Reader<'a> {
    bits: &'a [bool],
    offset: usize,
}
impl Reader<'_> {
    fn take(&mut self, width: usize) -> Result<u64, OriginationError> {
        let end = self.offset + width;
        let field = self
            .bits
            .get(self.offset..end)
            .ok_or(OriginationError::Truncated { needed_bits: end })?;
        self.offset = end;
        Ok(field
            .iter()
            .fold(0, |value, bit| value * 2 + u64::from(*bit)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::address::AddressValue;
    fn message() -> Origination {
        Origination {
            protocol_version: ProtocolVersion::Is136RevA,
            emergency_call: false,
            called_party: AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("120*#".into()),
            },
            last_try: true,
            scm: 0b10101,
            service_code: ServiceCode::DigitalSpeech,
        }
    }
    #[test]
    fn mandatory_fields_match_independent_unaligned_vector() {
        let expected = message();
        let bytes = expected.encode().unwrap();
        assert_eq!(bytes, [0x07, 0x40, 0x20, 0x18, 0x95, 0x5e, 0x06, 0xa2]);
        let decoded = decode_prefix(&bytes).unwrap();
        assert_eq!(decoded.message, expected);
        assert_eq!(decoded.consumed_bits, 63);
        assert_eq!(decoded.remaining_bits, [false]);
    }
    #[test]
    fn malformed_fields_and_total_length_are_checked() {
        assert_eq!(decode_prefix(&[0x47]), Err(OriginationError::Protocol));
        assert_eq!(decode_prefix(&[0x08]), Err(OriginationError::MessageType));
        let bytes = message().encode().unwrap();
        assert!(matches!(
            decode_prefix(&bytes[..bytes.len() - 1]),
            Err(OriginationError::Truncated { .. })
        ));
        assert_eq!(
            Origination {
                scm: 32,
                ..message()
            }
            .encode(),
            Err(OriginationError::Field("scm"))
        );
        assert_eq!(
            Origination {
                service_code: ServiceCode::AsyncData,
                ..message()
            }
            .encode(),
            Err(OriginationError::RequiredDataOptions)
        );
        let oversized = AddressInfo {
            number_type: 0,
            numbering_plan: 1,
            address: AddressValue::Ira(vec![b'1'; 254]),
        };
        assert_eq!(
            Origination {
                called_party: oversized,
                ..message()
            }
            .encode(),
            Err(OriginationError::Length)
        );
    }
}

#[cfg(test)]
mod nokia_origination {
    use super::*;
    #[test]
    fn handset_origination_requests_analog_only_without_voice_options() {
        let decoded = decode_prefix(&[0x07, 0x40, 0x18, 0x09, 0x11, 0x11, 0x60]).unwrap();
        assert_eq!(decoded.message.protocol_version, ProtocolVersion::Is136RevA);
        assert_eq!(decoded.message.scm, 11);
        assert_eq!(decoded.message.service_code, ServiceCode::AnalogSpeech);
        assert_eq!(
            decoded.message.called_party.address,
            address::AddressValue::Tbcd("2222".into())
        );
        assert!(decoded.remaining_bits.iter().all(|bit| !*bit));
    }
}
