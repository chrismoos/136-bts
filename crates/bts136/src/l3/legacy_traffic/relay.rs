use super::{
    HEADER_BITS, LENGTH_BITS, MAX_OPTIONAL_OCTETS, OCTET_BITS, Reader, TrafficMessageError, put,
};
use crate::l3::relay::{self, Direction, HigherLayerProtocol, RelayError, RelayMessage};
const DATA: u8 = 0x0b;
const ACCEPT: u8 = 0x0c;
const REJECT: u8 = 0x0d;
const TRANSACTION_BITS: usize = 8;
const UNIT_LENGTH_BITS: usize = 8;
const CAUSE_BITS: usize = 8;
const CAUSE_SHIFT: u32 = 1;
const MAX_CONTROL_MESSAGE_OCTETS: usize = 255;
const MAX_UNIT_OCTETS: usize = u8::MAX as usize;
const PROTOCOL_OCTETS: usize = 1;
pub(super) const MAX_MESSAGE_BITS: usize = HEADER_BITS
    + TRANSACTION_BITS
    + UNIT_LENGTH_BITS
    + MAX_UNIT_OCTETS * OCTET_BITS
    + LENGTH_BITS
    + MAX_OPTIONAL_OCTETS * OCTET_BITS;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrafficRelay {
    pub message: RelayMessage,
    pub optional_octets: Vec<u8>,
}
impl TrafficRelay {
    pub(crate) fn from_addressed(
        relay: &relay::addressing::AddressedRelay,
    ) -> Result<Self, TrafficMessageError> {
        let fields = Self {
            message: relay.message.clone(),
            optional_octets: relay
                .optional_octets()
                .map_err(TrafficMessageError::Addressing)?,
        };
        fields.validate()?;
        Ok(fields)
    }
    pub fn from_control(bytes: &[u8], direction: Direction) -> Result<Self, TrafficMessageError> {
        if bytes.len() > MAX_CONTROL_MESSAGE_OCTETS {
            return Err(TrafficMessageError::Relay(RelayError::Length));
        }
        let decoded = relay::decode_prefix(bytes, direction).map_err(TrafficMessageError::Relay)?;
        let relay = Self {
            message: decoded.message,
            optional_octets: decoded.optional_octets,
        };
        relay.validate()?;
        Ok(relay)
    }
    pub fn control_message(&self, direction: Direction) -> Result<Vec<u8>, TrafficMessageError> {
        self.validate()?;
        let mut bytes = self
            .message
            .encode(direction)
            .map_err(TrafficMessageError::Relay)?;
        bytes.extend_from_slice(&self.optional_octets);
        if bytes.len() > MAX_CONTROL_MESSAGE_OCTETS {
            return Err(TrafficMessageError::Relay(RelayError::Length));
        }
        Ok(bytes)
    }
    pub(super) fn message_type(&self) -> u8 {
        match self.message {
            RelayMessage::Data { .. } => DATA,
            RelayMessage::Accept { .. } => ACCEPT,
            RelayMessage::Reject { .. } => REJECT,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), TrafficMessageError> {
        if self.optional_octets.len() > MAX_OPTIONAL_OCTETS {
            return Err(TrafficMessageError::OptionalLength);
        }
        match &self.message {
            RelayMessage::Data {
                protocol, payload, ..
            } => {
                protocol.code().map_err(TrafficMessageError::Relay)?;
                if payload.len() > relay::MAX_DATA_PAYLOAD_OCTETS {
                    return Err(TrafficMessageError::Relay(RelayError::Length));
                }
            }
            RelayMessage::Accept { .. } | RelayMessage::Reject { .. }
                if !self.optional_octets.is_empty() =>
            {
                return Err(TrafficMessageError::OptionalLength);
            }
            RelayMessage::Reject { cause, .. } => validate_cause(*cause)?,
            _ => {}
        }
        Ok(())
    }
    // TIA-136-133-E §§5.1.2.2.19–21 and 9.1.2.2.24–26 include a seven-bit cause plus reserved bit and a six-bit Remaining Length.
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        self.validate()?;
        let transaction = match self.message {
            RelayMessage::Data { transaction_id, .. }
            | RelayMessage::Accept { transaction_id }
            | RelayMessage::Reject { transaction_id, .. } => transaction_id,
        };
        put(bits, u64::from(transaction), TRANSACTION_BITS);
        match &self.message {
            RelayMessage::Data {
                protocol, payload, ..
            } => {
                put(
                    bits,
                    (payload.len() + PROTOCOL_OCTETS) as u64,
                    UNIT_LENGTH_BITS,
                );
                put(
                    bits,
                    u64::from(protocol.code().map_err(TrafficMessageError::Relay)?),
                    OCTET_BITS,
                );
                for byte in payload {
                    put(bits, u64::from(*byte), OCTET_BITS)
                }
                put(bits, self.optional_octets.len() as u64, LENGTH_BITS);
                for byte in &self.optional_octets {
                    put(bits, u64::from(*byte), OCTET_BITS)
                }
            }
            RelayMessage::Reject { cause, .. } => {
                put(bits, u64::from(*cause) << CAUSE_SHIFT, CAUSE_BITS)
            }
            RelayMessage::Accept { .. } => {}
        }
        Ok(())
    }
}
pub(super) fn decode(
    kind: u8,
    reader: &mut Reader<'_>,
    direction: Direction,
) -> Result<Option<TrafficRelay>, TrafficMessageError> {
    if !matches!(kind, DATA | ACCEPT | REJECT) {
        return Ok(None);
    }
    let transaction_id = reader.take(TRANSACTION_BITS)? as u8;
    let mut optional_octets = Vec::new();
    let message = match kind {
        DATA => {
            let length = reader.take(UNIT_LENGTH_BITS)? as usize;
            if length < PROTOCOL_OCTETS {
                return Err(TrafficMessageError::Relay(RelayError::Length));
            }
            let protocol = HigherLayerProtocol::decode(reader.take(OCTET_BITS)? as u8)
                .map_err(TrafficMessageError::Relay)?;
            let payload = (PROTOCOL_OCTETS..length)
                .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
                .collect::<Result<Vec<_>, _>>()?;
            let remaining = reader.take(LENGTH_BITS)? as usize;
            optional_octets = (0..remaining)
                .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
                .collect::<Result<Vec<_>, _>>()?;
            RelayMessage::Data {
                transaction_id,
                protocol,
                payload,
            }
        }
        ACCEPT => RelayMessage::Accept { transaction_id },
        _ => {
            let field = reader.take(CAUSE_BITS)? as u8;
            // TIA-136-133-E §§5,5.1.2.3.104 ignores received RSVD and uses TIA-136-123-E §6.181 direction assignments.
            let cause = field >> CAUSE_SHIFT;
            relay::validate_cause(direction, cause).map_err(TrafficMessageError::Relay)?;
            RelayMessage::Reject {
                transaction_id,
                cause,
            }
        }
    };
    Ok(Some(TrafficRelay {
        message,
        optional_octets,
    }))
}
fn validate_cause(cause: u8) -> Result<(), TrafficMessageError> {
    if relay::validate_cause(Direction::Forward, cause).is_err()
        && relay::validate_cause(Direction::Reverse, cause).is_err()
    {
        return Err(TrafficMessageError::Relay(RelayError::Cause));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::legacy_traffic::{self, TrafficMessage};
    fn wire(text: &str) -> Vec<bool> {
        text.chars()
            .filter(|bit| *bit != ' ')
            .map(|bit| bit == '1')
            .collect()
    }
    #[test]
    fn traffic_relay_vectors_include_the_dtc_length_and_cause_layout() {
        let cases = [
            (
                TrafficRelay {
                    message: RelayMessage::Data {
                        transaction_id: 42,
                        protocol: HigherLayerProtocol::Sms,
                        payload: vec![0xaa, 0x55],
                    },
                    optional_octets: vec![0x20, 0x31],
                },
                "00 00001011 00101010 00000011 00000001 10101010 01010101 000010 00100000 00110001",
            ),
            (
                TrafficRelay {
                    message: RelayMessage::Accept { transaction_id: 42 },
                    optional_octets: vec![],
                },
                "00 00001100 00101010",
            ),
            (
                TrafficRelay {
                    message: RelayMessage::Reject {
                        transaction_id: 42,
                        cause: 95,
                    },
                    optional_octets: vec![],
                },
                "00 00001101 00101010 10111110",
            ),
        ];
        for (relay, text) in cases {
            let expected = wire(text);
            let message = TrafficMessage::Relay(relay);
            assert_eq!(message.encode().unwrap(), expected);
            let decoded = legacy_traffic::decode_prefix_in(&expected, Direction::Forward).unwrap();
            assert_eq!(decoded.message, message);
            assert_eq!(decoded.consumed_bits, expected.len());
            for length in 0..expected.len() {
                assert!(
                    legacy_traffic::decode_prefix_in(&expected[..length], Direction::Forward)
                        .is_err()
                );
            }
        }
        let received = wire("00 00001101 00101010 10111111");
        let decoded = legacy_traffic::decode_prefix_in(&received, Direction::Forward).unwrap();
        assert_eq!(
            decoded.message.encode().unwrap(),
            wire("00 00001101 00101010 10111110")
        );
    }
    #[test]
    fn received_relay_reserved_bit_is_ignored_but_cause_direction_is_checked() {
        // TIA-136-123-E §6.181 assigns every defined seven-bit cause to one or both directions.
        const FORWARD: &[u8] = &[
            1, 2, 3, 4, 10, 21, 27, 28, 29, 30, 38, 41, 42, 47, 69, 81, 95, 96, 97, 98, 99, 100,
            101, 111, 127,
        ];
        const REVERSE: &[u8] = &[4, 22, 96, 97, 99, 100, 101, 111];
        for (direction, allowed) in [(Direction::Forward, FORWARD), (Direction::Reverse, REVERSE)] {
            for cause in 0..128u8 {
                for reserved in 0..2u8 {
                    let bits = wire(&format!("00 00001101 00101010 {cause:07b}{reserved:01b}"));
                    let result = legacy_traffic::decode_prefix_in(&bits, direction);
                    if allowed.contains(&cause) {
                        let decoded = result.unwrap();
                        assert_eq!(decoded.consumed_bits, bits.len());
                        let expected = TrafficMessage::Relay(TrafficRelay {
                            message: RelayMessage::Reject {
                                transaction_id: 42,
                                cause,
                            },
                            optional_octets: vec![],
                        });
                        assert_eq!(decoded.message, expected);
                        assert_eq!(
                            decoded.message.encode().unwrap(),
                            wire(&format!("00 00001101 00101010 {cause:07b}0"))
                        );
                    } else {
                        assert_eq!(result, Err(TrafficMessageError::Relay(RelayError::Cause)));
                    }
                }
            }
        }
    }
    #[test]
    fn maximum_traffic_relay_survives_word_fragmentation_and_is_bounded() {
        let mut relay = TrafficRelay {
            message: RelayMessage::Data {
                transaction_id: 255,
                protocol: HigherLayerProtocol::Sms,
                payload: (0..254).map(|value| value as u8).collect(),
            },
            optional_octets: vec![0xa5; 63],
        };
        let message = TrafficMessage::Relay(relay.clone());
        let bits = message.encode().unwrap();
        assert_eq!(bits.len(), MAX_MESSAGE_BITS);
        let words = crate::link::traffic::encode(&message).unwrap();
        assert_eq!(words.len(), 54);
        let mut receiver = crate::link::traffic::TrafficMessageReceiver::default();
        let mut decoded = Vec::new();
        for word in words {
            decoded.extend(receiver.push(&word).unwrap());
        }
        assert_eq!(decoded, [message]);
        relay.optional_octets.push(0);
        assert!(TrafficMessage::Relay(relay).encode().is_err());
        let oversized = TrafficRelay {
            message: RelayMessage::Data {
                transaction_id: 0,
                protocol: HigherLayerProtocol::Sms,
                payload: vec![0; 255],
            },
            optional_octets: vec![],
        };
        assert!(TrafficMessage::Relay(oversized).encode().is_err());
    }
    #[test]
    fn dcch_conversion_preserves_addresses_and_direction_specific_types() {
        use crate::l3::{
            address::{AddressInfo, AddressValue},
            relay::addressing::{AddressedRelay, RelayAddresses},
        };
        let addressed = AddressedRelay {
            message: RelayMessage::Data {
                transaction_id: 7,
                protocol: HigherLayerProtocol::Sms,
                payload: vec![1, 2, 3],
            },
            addresses: RelayAddresses {
                destination: Some(AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: AddressValue::Tbcd("1002".into()),
                }),
                ..Default::default()
            },
        };
        for direction in [Direction::Forward, Direction::Reverse] {
            let control = addressed.encode(direction).unwrap();
            let relay = TrafficRelay::from_control(&control, direction).unwrap();
            assert_eq!(relay.control_message(direction).unwrap(), control);
            let traffic = TrafficMessage::Relay(relay.clone()).encode().unwrap();
            assert_eq!(
                legacy_traffic::decode_prefix(&traffic).unwrap().message,
                TrafficMessage::Relay(relay)
            );
        }
        let reject = TrafficRelay {
            message: RelayMessage::Reject {
                transaction_id: 1,
                cause: 22,
            },
            optional_octets: vec![],
        };
        assert_eq!(
            reject.control_message(Direction::Reverse).unwrap(),
            [11, 1, 44]
        );
        assert!(reject.control_message(Direction::Forward).is_err());
        assert_eq!(
            TrafficRelay::from_control(&[27, 42], Direction::Forward)
                .unwrap()
                .control_message(Direction::Reverse)
                .unwrap(),
            [10, 42]
        );
    }
    #[test]
    fn addressed_dtc_conversion_keeps_maximum_payload_and_optional_fields() {
        use crate::l3::{
            address::{AddressInfo, AddressValue},
            relay::addressing::{self, AddressedRelay, RelayAddresses},
        };
        let mut addressed = AddressedRelay {
            message: RelayMessage::Data {
                transaction_id: 42,
                protocol: HigherLayerProtocol::Sms,
                payload: vec![0xa5; relay::MAX_DATA_PAYLOAD_OCTETS],
            },
            addresses: RelayAddresses {
                originating: Some(AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: AddressValue::Ira(vec![b'1'; 60]),
                }),
                ..Default::default()
            },
        };
        assert!(addressed.encode(Direction::Forward).is_err());
        let traffic = TrafficRelay::from_addressed(&addressed).unwrap();
        assert_eq!(traffic.optional_octets.len(), MAX_OPTIONAL_OCTETS);
        let bits = TrafficMessage::Relay(traffic.clone()).encode().unwrap();
        assert_eq!(bits.len(), MAX_MESSAGE_BITS);
        let decoded = addressing::decode_fields(traffic.message, &traffic.optional_octets).unwrap();
        assert_eq!(decoded.relay, addressed);
        assert!(decoded.unparsed_bits.is_empty());
        addressed.addresses.originating.as_mut().unwrap().address =
            AddressValue::Ira(vec![b'1'; 61]);
        assert_eq!(
            TrafficRelay::from_addressed(&addressed),
            Err(TrafficMessageError::OptionalLength)
        );
        assert_eq!(
            addressing::decode(&vec![0; 256], Direction::Forward),
            Err(addressing::AddressingError::Length)
        );
    }
}
