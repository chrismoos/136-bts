use super::*;
use crate::l3::address::{self, AddressError, AddressInfo, AddressValue};

const PARAMETER_BITS: usize = 4;
const OCTET_BITS: usize = 8;
const SERVER: u8 = 1;
const DESTINATION: u8 = 2;
const ORIGINATING: u8 = 4;
const ORIGINATING_PRESENTATION: u8 = 6;
const INDICATOR_BITS: usize = 2;
const PRESENTATION_ALLOWED: u8 = 0;
const PRESENTATION_UNAVAILABLE: u8 = 2;
const SCREENING_MAX: u8 = 3;
const OPTIONAL_NONCRITICAL_FIRST: u8 = 8;
const OPTIONAL_NONCRITICAL_LAST: u8 = 14;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelayAddresses {
    pub server: Option<AddressInfo>,
    pub destination: Option<AddressInfo>,
    pub originating: Option<AddressInfo>,
    pub originating_presentation: Option<OriginatingPresentation>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OriginatingPresentation {
    pub presentation: u8,
    pub screening: u8,
}
impl RelayAddresses {
    pub(crate) fn restricts_originating_presentation(&self) -> bool {
        self.originating_presentation
            .is_some_and(|value| value.presentation != PRESENTATION_ALLOWED)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressedRelay {
    pub message: RelayMessage,
    pub addresses: RelayAddresses,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedAddressedRelay {
    pub relay: AddressedRelay,
    /// The first unsupported comprehension-required element and the remainder are retained.
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressingError {
    Relay(RelayError),
    Address(AddressError),
    MessageType,
    Length,
    Truncated,
    Reserved,
    Duplicate,
    BinaryUserAddress,
}
impl fmt::Display for AddressingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid R-DATA addressing: {self:?}")
    }
}
impl Error for AddressingError {}
impl From<RelayError> for AddressingError {
    fn from(error: RelayError) -> Self {
        Self::Relay(error)
    }
}
impl From<AddressError> for AddressingError {
    fn from(error: AddressError) -> Self {
        Self::Address(error)
    }
}
impl AddressedRelay {
    // IS-136.1-A §§6.4.3.11/6.4.4.9. Parameter types are four bits, without octet alignment.
    pub fn encode(&self, direction: Direction) -> Result<Vec<u8>, AddressingError> {
        let mut bytes = self.message.encode(direction)?;
        bytes.extend(self.optional_octets()?);
        if bytes.len() > MAX_MESSAGE_OCTETS {
            return Err(AddressingError::Length);
        }
        Ok(bytes)
    }
    pub(crate) fn optional_octets(&self) -> Result<Vec<u8>, AddressingError> {
        if !matches!(self.message, RelayMessage::Data { .. }) {
            return Err(AddressingError::MessageType);
        }
        let mut bits = Vec::new();
        for (kind, address) in [
            (SERVER, &self.addresses.server),
            (DESTINATION, &self.addresses.destination),
            (ORIGINATING, &self.addresses.originating),
        ] {
            let Some(address) = address else {
                continue;
            };
            if kind != SERVER && matches!(address.address, AddressValue::DpcSsn(_)) {
                return Err(AddressingError::BinaryUserAddress);
            }
            bits.extend((0..PARAMETER_BITS).rev().map(|bit| kind & (1 << bit) != 0));
            bits.extend(octet_bits(&address.encode()?));
        }
        if let Some(value) = self.addresses.originating_presentation {
            if value.presentation > PRESENTATION_UNAVAILABLE || value.screening > SCREENING_MAX {
                return Err(AddressingError::Reserved);
            }
            bits.extend(
                (0..PARAMETER_BITS)
                    .rev()
                    .map(|bit| ORIGINATING_PRESENTATION & (1 << bit) != 0),
            );
            bits.extend(
                (0..INDICATOR_BITS)
                    .rev()
                    .map(|bit| value.presentation & (1 << bit) != 0),
            );
            bits.extend(
                (0..INDICATOR_BITS)
                    .rev()
                    .map(|bit| value.screening & (1 << bit) != 0),
            );
        }
        bits.resize(bits.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
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
pub fn decode(
    bytes: &[u8],
    direction: Direction,
) -> Result<DecodedAddressedRelay, AddressingError> {
    if bytes.len() > MAX_MESSAGE_OCTETS {
        return Err(AddressingError::Length);
    }
    let decoded = decode_prefix(bytes, direction)?;
    decode_fields(decoded.message, &decoded.optional_octets)
}
pub(crate) fn decode_fields(
    message: RelayMessage,
    optional_octets: &[u8],
) -> Result<DecodedAddressedRelay, AddressingError> {
    if !matches!(message, RelayMessage::Data { .. }) {
        return Err(AddressingError::MessageType);
    }
    let bits = octet_bits(optional_octets);
    let mut offset = 0;
    let mut addresses = RelayAddresses::default();
    while offset < bits.len() {
        if bits.len() - offset < OCTET_BITS && bits[offset..].iter().all(|bit| !bit) {
            offset = bits.len();
            break;
        }
        let parameter_start = offset;
        let kind = take(&bits, &mut offset, PARAMETER_BITS)?;
        match kind {
            SERVER | DESTINATION | ORIGINATING => {
                let length = take(&bits, &mut offset, OCTET_BITS)?;
                let mut bytes = vec![length];
                for _ in 0..length {
                    bytes.push(take(&bits, &mut offset, OCTET_BITS)?);
                }
                let address = address::decode_prefix(&bytes)?.address;
                if kind != SERVER && matches!(address.address, AddressValue::DpcSsn(_)) {
                    return Err(AddressingError::BinaryUserAddress);
                }
                let slot = match kind {
                    SERVER => &mut addresses.server,
                    DESTINATION => &mut addresses.destination,
                    _ => &mut addresses.originating,
                };
                if slot.is_some() {
                    return Err(AddressingError::Duplicate);
                }
                *slot = Some(address);
            }
            ORIGINATING_PRESENTATION => {
                // TIA-136-123-E §6.260 defines two presentation bits followed by two screening bits.
                let presentation = take(&bits, &mut offset, INDICATOR_BITS)?;
                let screening = take(&bits, &mut offset, INDICATOR_BITS)?;
                if presentation > PRESENTATION_UNAVAILABLE {
                    return Err(AddressingError::Reserved);
                }
                if addresses
                    .originating_presentation
                    .replace(OriginatingPresentation {
                        presentation,
                        screening,
                    })
                    .is_some()
                {
                    return Err(AddressingError::Duplicate);
                }
            }
            OPTIONAL_NONCRITICAL_FIRST..=OPTIONAL_NONCRITICAL_LAST => {
                // IS-136.1-A §6.4.1.1. Unknown noncritical elements have an eight-bit octet length.
                let length = usize::from(take(&bits, &mut offset, OCTET_BITS)?);
                let end = offset + length * OCTET_BITS;
                if end > bits.len() {
                    return Err(AddressingError::Truncated);
                }
                offset = end;
            }
            0 => return Err(AddressingError::Reserved),
            _ => {
                offset = parameter_start;
                break;
            }
        }
    }
    Ok(DecodedAddressedRelay {
        relay: AddressedRelay { message, addresses },
        unparsed_bits: bits[offset..].to_vec(),
    })
}
fn octet_bits(bytes: &[u8]) -> Vec<bool> {
    bytes
        .iter()
        .flat_map(|octet| {
            (0..OCTET_BITS)
                .rev()
                .map(move |bit| octet & (1 << bit) != 0)
        })
        .collect()
}
fn take(bits: &[bool], offset: &mut usize, width: usize) -> Result<u8, AddressingError> {
    let end = *offset + width;
    let field = bits.get(*offset..end).ok_or(AddressingError::Truncated)?;
    *offset = end;
    Ok(field
        .iter()
        .fold(0, |value, bit| value * 2 + u8::from(*bit)))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn number(digits: &str) -> AddressInfo {
        AddressInfo {
            number_type: 0,
            numbering_plan: 1,
            address: AddressValue::Tbcd(digits.into()),
        }
    }
    #[test]
    fn destination_and_originating_addresses_match_unaligned_wire_vector() {
        let relay = AddressedRelay {
            message: RelayMessage::Data {
                transaction_id: 42,
                protocol: HigherLayerProtocol::Sms,
                payload: vec![0xaa],
            },
            addresses: RelayAddresses {
                destination: Some(number("12")),
                originating: Some(number("30")),
                ..RelayAddresses::default()
            },
        };
        assert_eq!(
            relay.encode(Direction::Reverse).unwrap(),
            [9, 42, 2, 1, 0xaa, 0x20, 0x20, 0x31, 0x24, 0x02, 0x03, 0x3a]
        );
        let decoded = decode(
            &relay.encode(Direction::Reverse).unwrap(),
            Direction::Reverse,
        )
        .unwrap();
        assert_eq!(decoded.relay, relay);
        assert!(decoded.unparsed_bits.is_empty());
    }
    #[test]
    fn unknown_optional_rules_preserve_supported_prefix_and_skip_noncritical_fields() {
        let data = [9, 42, 1, 1];
        let mut unknown = data.to_vec();
        unknown.extend([0x30, 0x00]);
        let decoded = decode(&unknown, Direction::Reverse).unwrap();
        assert_eq!(decoded.unparsed_bits, octet_bits(&[0x30, 0x00]));
        let mut noncritical = data.to_vec();
        noncritical.extend([0x80, 0x1f, 0xf2, 0x02, 0x03, 0x12]);
        assert_eq!(
            decode(&noncritical, Direction::Reverse)
                .unwrap()
                .relay
                .addresses
                .destination,
            Some(number("12"))
        );
        assert_eq!(
            decode(&[9, 42, 1, 1, 0x80, 0xff], Direction::Reverse),
            Err(AddressingError::Truncated)
        );
    }
}

#[cfg(test)]
mod nokia_tests {
    use super::*;
    #[test]
    fn nokia_mo_sms_presentation_indicator_round_trips() {
        let bytes = [
            0x09, 0x01, 0x07, 0x01, 0x00, 0x00, 0x09, 0x02, 0x08, 0x4a, 0x20, 0x40, 0x12, 0x22,
            0x22, 0x04, 0x06, 0x01, 0x31, 0x43, 0x78, 0x17, 0x65, 0x60,
        ];
        let decoded = decode(&bytes, Direction::Reverse).unwrap();
        assert!(decoded.unparsed_bits.is_empty());
        assert_eq!(
            decoded.relay.addresses.originating_presentation,
            Some(OriginatingPresentation {
                presentation: 0,
                screening: 0
            })
        );
        assert_eq!(decoded.relay.encode(Direction::Reverse).unwrap(), bytes);
        assert!(!decoded.relay.addresses.restricts_originating_presentation());
        for (value, error) in [
            (0x6c, AddressingError::Reserved),
            (0x60, AddressingError::Duplicate),
        ] {
            let mut invalid = bytes.to_vec();
            if value == 0x6c {
                *invalid.last_mut().unwrap() = value;
            } else {
                invalid.push(value);
            }
            assert_eq!(decode(&invalid, Direction::Reverse), Err(error));
        }
        let mut restricted = bytes;
        *restricted.last_mut().unwrap() = 0x64;
        assert!(
            decode(&restricted, Direction::Reverse)
                .unwrap()
                .relay
                .addresses
                .restricts_originating_presentation()
        );
    }
}
