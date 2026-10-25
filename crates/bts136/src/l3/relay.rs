pub mod addressing;
use std::{error::Error, fmt};

pub const MAX_DATA_PAYLOAD_OCTETS: usize = u8::MAX as usize - 1;
const TYPE_MASK: u8 = 0x3f;
const MAX_MESSAGE_OCTETS: usize = 255;
const DATA_HEADER_OCTETS: usize = 4;
const PROTOCOL_HEADER_OCTETS: usize = 1;
const CAUSE_SHIFT: u8 = 1;
const RESERVED_MASK: u8 = 1;
const FORWARD_TYPES: [u8; 3] = [26, 27, 28];
const REVERSE_TYPES: [u8; 3] = [9, 10, 11];
// TIA-136-123-E §6.181 defines direction-specific R-Cause values.
const BOTH_CAUSES: [u8; 7] = [4, 96, 97, 99, 100, 101, 111];
const FORWARD_CAUSES: [u8; 18] = [
    1, 2, 3, 10, 21, 27, 28, 29, 30, 38, 41, 42, 47, 69, 81, 95, 98, 127,
];
const MEMORY_CAPACITY_EXCEEDED: u8 = 22;
const SEGMENTED_PROTOCOL_MASK: u8 = 0x40;
const TELESERVICE_MASK: u8 = 0x3f;
const CARRIER_PROTOCOL_FIRST: u8 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Reverse,
}
impl Direction {
    fn types(self) -> [u8; 3] {
        match self {
            Self::Forward => FORWARD_TYPES,
            Self::Reverse => REVERSE_TYPES,
        }
    }
}
// TIA-136-700-D §2 assigns the low six HLPI bits. Value 2 and values above 8 are reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Teleservice {
    NetworkSpecific = 0,
    Sms = 1,
    Oats = 3,
    Opts = 4,
    Guts = 5,
    ChargeIndication = 6,
    Ghost = 7,
    Samps = 8,
}
impl Teleservice {
    fn decode(code: u8) -> Result<Self, RelayError> {
        match code {
            0 => Ok(Self::NetworkSpecific),
            1 => Ok(Self::Sms),
            3 => Ok(Self::Oats),
            4 => Ok(Self::Opts),
            5 => Ok(Self::Guts),
            6 => Ok(Self::ChargeIndication),
            7 => Ok(Self::Ghost),
            8 => Ok(Self::Samps),
            _ => Err(RelayError::ProtocolIdentifier),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HigherLayerProtocol {
    NetworkSpecific,
    Sms,
    Oats,
    Opts,
    Guts,
    ChargeIndication,
    Ghost,
    Samps,
    Segmented(Teleservice),
    CarrierSpecific(u8),
}
impl HigherLayerProtocol {
    pub(crate) fn code(self) -> Result<u8, RelayError> {
        Ok(match self {
            Self::NetworkSpecific => Teleservice::NetworkSpecific as u8,
            Self::Sms => Teleservice::Sms as u8,
            Self::Oats => Teleservice::Oats as u8,
            Self::Opts => Teleservice::Opts as u8,
            Self::Guts => Teleservice::Guts as u8,
            Self::ChargeIndication => Teleservice::ChargeIndication as u8,
            Self::Ghost => Teleservice::Ghost as u8,
            Self::Samps => Teleservice::Samps as u8,
            Self::Segmented(service) => SEGMENTED_PROTOCOL_MASK | service as u8,
            Self::CarrierSpecific(code) if code >= CARRIER_PROTOCOL_FIRST => code,
            _ => return Err(RelayError::ProtocolIdentifier),
        })
    }
    pub(crate) fn decode(code: u8) -> Result<Self, RelayError> {
        if code >= CARRIER_PROTOCOL_FIRST {
            return Ok(Self::CarrierSpecific(code));
        }
        let service = Teleservice::decode(code & TELESERVICE_MASK)?;
        if code & SEGMENTED_PROTOCOL_MASK != 0 {
            return Ok(Self::Segmented(service));
        }
        Ok(match service {
            Teleservice::NetworkSpecific => Self::NetworkSpecific,
            Teleservice::Sms => Self::Sms,
            Teleservice::Oats => Self::Oats,
            Teleservice::Opts => Self::Opts,
            Teleservice::Guts => Self::Guts,
            Teleservice::ChargeIndication => Self::ChargeIndication,
            Teleservice::Ghost => Self::Ghost,
            Teleservice::Samps => Self::Samps,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayMessage {
    Data {
        transaction_id: u8,
        protocol: HigherLayerProtocol,
        payload: Vec<u8>,
    },
    Accept {
        transaction_id: u8,
    },
    Reject {
        transaction_id: u8,
        cause: u8,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedRelay {
    pub message: RelayMessage,
    pub consumed_octets: usize,
    /// Addressing and other optional information elements remain uninterpreted.
    pub optional_octets: Vec<u8>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayError {
    Truncated,
    ProtocolDiscriminator,
    MessageType,
    Length,
    ProtocolIdentifier,
    Cause,
    Reserved,
}
impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid IS-136 relay message: {self:?}")
    }
}
impl Error for RelayError {}
impl RelayMessage {
    // IS-136.1-A §§6.4.3.11–13 and 6.4.4.9–11. Required fields only.
    pub fn encode(&self, direction: Direction) -> Result<Vec<u8>, RelayError> {
        let [data_type, accept_type, reject_type] = direction.types();
        match self {
            Self::Data {
                transaction_id,
                protocol,
                payload,
            } => {
                if payload.len() > MAX_MESSAGE_OCTETS - DATA_HEADER_OCTETS {
                    return Err(RelayError::Length);
                }
                let mut bytes = vec![
                    data_type,
                    *transaction_id,
                    (payload.len() + PROTOCOL_HEADER_OCTETS) as u8,
                    protocol.code()?,
                ];
                bytes.extend_from_slice(payload);
                Ok(bytes)
            }
            Self::Accept { transaction_id } => Ok(vec![accept_type, *transaction_id]),
            Self::Reject {
                transaction_id,
                cause,
            } => {
                validate_cause(direction, *cause)?;
                Ok(vec![reject_type, *transaction_id, *cause << CAUSE_SHIFT])
            }
        }
    }
}
pub fn decode_prefix(bytes: &[u8], direction: Direction) -> Result<DecodedRelay, RelayError> {
    if bytes.len() > MAX_MESSAGE_OCTETS {
        return Err(RelayError::Length);
    }
    let header = *bytes.first().ok_or(RelayError::Truncated)?;
    if header & !TYPE_MASK != 0 {
        return Err(RelayError::ProtocolDiscriminator);
    }
    let transaction_id = *bytes.get(1).ok_or(RelayError::Truncated)?;
    let [data_type, accept_type, reject_type] = direction.types();
    let (message, consumed_octets) = if header == data_type {
        let length = usize::from(*bytes.get(2).ok_or(RelayError::Truncated)?);
        if length < PROTOCOL_HEADER_OCTETS {
            return Err(RelayError::Length);
        }
        let end = DATA_HEADER_OCTETS - PROTOCOL_HEADER_OCTETS + length;
        let content = bytes
            .get(DATA_HEADER_OCTETS - PROTOCOL_HEADER_OCTETS..end)
            .ok_or(RelayError::Truncated)?;
        (
            RelayMessage::Data {
                transaction_id,
                protocol: HigherLayerProtocol::decode(content[0])?,
                payload: content[PROTOCOL_HEADER_OCTETS..].to_vec(),
            },
            end,
        )
    } else if header == accept_type {
        (RelayMessage::Accept { transaction_id }, 2)
    } else if header == reject_type {
        let value = *bytes.get(2).ok_or(RelayError::Truncated)?;
        if value & RESERVED_MASK != 0 {
            return Err(RelayError::Reserved);
        }
        let cause = value >> CAUSE_SHIFT;
        validate_cause(direction, cause)?;
        (
            RelayMessage::Reject {
                transaction_id,
                cause,
            },
            3,
        )
    } else {
        return Err(RelayError::MessageType);
    };
    Ok(DecodedRelay {
        message,
        consumed_octets,
        optional_octets: bytes[consumed_octets..].to_vec(),
    })
}
pub(super) fn validate_cause(direction: Direction, cause: u8) -> Result<(), RelayError> {
    if BOTH_CAUSES.contains(&cause)
        || match direction {
            Direction::Forward => FORWARD_CAUSES.contains(&cause),
            Direction::Reverse => cause == MEMORY_CAPACITY_EXCEEDED,
        }
    {
        Ok(())
    } else {
        Err(RelayError::Cause)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relay_vectors_and_optional_suffix_match_direction_specific_wire_codes() {
        let message = RelayMessage::Data {
            transaction_id: 42,
            protocol: HigherLayerProtocol::Sms,
            payload: vec![0xaa, 0xbb],
        };
        for (direction, code) in [(Direction::Forward, 26), (Direction::Reverse, 9)] {
            assert_eq!(
                message.encode(direction).unwrap(),
                [code, 42, 3, 1, 0xaa, 0xbb]
            );
            let bytes = [code, 42, 3, 1, 0xaa, 0xbb, 0xff];
            let decoded = decode_prefix(&bytes, direction).unwrap();
            assert_eq!(decoded.message, message);
            assert_eq!(decoded.consumed_octets, 6);
            assert_eq!(decoded.optional_octets, [0xff]);
        }
        assert_eq!(
            RelayMessage::Reject {
                transaction_id: 1,
                cause: 22
            }
            .encode(Direction::Reverse)
            .unwrap(),
            [11, 1, 44]
        );
        assert_eq!(
            RelayMessage::Reject {
                transaction_id: 1,
                cause: 22
            }
            .encode(Direction::Forward),
            Err(RelayError::Cause)
        );
        assert_eq!(
            RelayMessage::Accept { transaction_id: 42 }
                .encode(Direction::Forward)
                .unwrap(),
            [27, 42]
        );
    }
    #[test]
    fn rejection_causes_follow_both_direction_and_network_only_assignments() {
        for cause in [4, 96, 97, 99, 100, 101, 111] {
            for (direction, code) in [(Direction::Forward, 28), (Direction::Reverse, 11)] {
                let expected = [code, 7, cause * 2];
                let message = RelayMessage::Reject {
                    transaction_id: 7,
                    cause,
                };
                assert_eq!(message.encode(direction).unwrap(), expected);
                assert_eq!(
                    decode_prefix(&expected, direction).unwrap().message,
                    message
                );
            }
        }
        assert_eq!(
            RelayMessage::Reject {
                transaction_id: 7,
                cause: 3
            }
            .encode(Direction::Forward)
            .unwrap(),
            [28, 7, 6]
        );
        assert_eq!(
            decode_prefix(&[11, 7, 6], Direction::Reverse),
            Err(RelayError::Cause)
        );
        for cause in [0, 5, 23, 94, 102, 126] {
            for direction in [Direction::Forward, Direction::Reverse] {
                assert_eq!(
                    RelayMessage::Reject {
                        transaction_id: 7,
                        cause
                    }
                    .encode(direction),
                    Err(RelayError::Cause)
                );
            }
        }
    }
    #[test]
    fn reserved_fields_lengths_and_protocols_are_rejected() {
        assert_eq!(
            decode_prefix(&vec![0; 256], Direction::Reverse),
            Err(RelayError::Length)
        );
        assert_eq!(
            decode_prefix(&[0x49, 0], Direction::Reverse),
            Err(RelayError::ProtocolDiscriminator)
        );
        assert_eq!(
            decode_prefix(&[9, 0, 0], Direction::Reverse),
            Err(RelayError::Length)
        );
        assert_eq!(
            decode_prefix(&[9, 0, 1, 2], Direction::Reverse),
            Err(RelayError::ProtocolIdentifier)
        );
        assert_eq!(
            decode_prefix(&[11, 0, 45], Direction::Reverse),
            Err(RelayError::Reserved)
        );
        assert_eq!(
            decode_prefix(&[9, 0, 3, 1], Direction::Reverse),
            Err(RelayError::Truncated)
        );
        let maximum = RelayMessage::Data {
            transaction_id: 0,
            protocol: HigherLayerProtocol::Sms,
            payload: vec![0; 251],
        };
        assert_eq!(maximum.encode(Direction::Forward).unwrap().len(), 255);
        assert_eq!(
            decode_prefix(
                &maximum.encode(Direction::Forward).unwrap(),
                Direction::Forward
            )
            .unwrap()
            .message,
            maximum
        );
        assert_eq!(
            RelayMessage::Data {
                transaction_id: 0,
                protocol: HigherLayerProtocol::Sms,
                payload: vec![0; 252]
            }
            .encode(Direction::Forward),
            Err(RelayError::Length)
        );
    }
}
#[cfg(test)]
mod protocol_tests;
