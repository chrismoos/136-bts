mod user_data;
use super::relay::Direction;
use std::{error::Error, fmt};
pub use user_data::{InternationalEncoding, UserData};

const REFERENCE_BITS: usize = 13;
const REFERENCE_MASK: u16 = (1 << REFERENCE_BITS) - 1;
const TYPE_SHIFT: u8 = 5;
const PRIVACY_SHIFT: u8 = 5;
const URGENCY_SHIFT: u8 = 3;
const DELIVERY_SHIFT: u8 = 2;
const MANUAL_SHIFT: u8 = 1;
const URGENCY_MASK: u8 = 3;
pub const NORMAL_URGENCY: u8 = 1;
const ENCODING_SHIFT: u8 = 3;
const RESERVED_MASK: u8 = 7;
const VALIDITY_SHIFT: u8 = 5;
const DISPLAY_SHIFT: u8 = 3;
const HEADER_OCTETS: usize = 3;
const REFERENCE_OCTETS: usize = 2;
const DELIVERY_ACK_TYPE: u8 = 1;
const MANUAL_ACK_TYPE: u8 = 2;
const UNIT_HEADER_OCTETS: usize = 2;
const DELIVERY_OPTIONS_OCTETS: usize = 1;
// TIA-136-133-E §5.1.2.3.105 includes HLPI in the one-octet R-Data Unit length.
pub const MAX_SINGLE_RELAY_USER_OCTETS: usize = super::relay::MAX_DATA_PAYLOAD_OCTETS
    - HEADER_OCTETS
    - DELIVERY_OPTIONS_OCTETS
    - UNIT_HEADER_OCTETS;
pub const MAX_SINGLE_RELAY_IRA_CHARACTERS: usize = MAX_SINGLE_RELAY_USER_OCTETS;
pub const MAX_SINGLE_RELAY_BMP_CHARACTERS: usize = MAX_SINGLE_RELAY_USER_OCTETS / 2;
const MAX_USER_OCTETS: usize = 254;
// TIA-136-710-C §4.12: Parameter Type 0010, 32-bit seconds since 1980-01-01 GMT, 12-bit zone.
const TIME_STAMP_PARAMETER: u64 = 0b0010;
const TIME_STAMP_SECONDS_SHIFT: u32 = 12;
const TIME_STAMP_PARAMETER_SHIFT: u32 = 44;
const TIME_STAMP_OCTETS: usize = 6;
pub(crate) const MAX_SUBMIT_PDU_OCTETS: usize =
    HEADER_OCTETS + UNIT_HEADER_OCTETS + MAX_USER_OCTETS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    pub message_reference: u16,
    pub privacy: u8,
    pub urgency: u8,
    pub delivery_acknowledgment: bool,
    pub manual_acknowledgment: bool,
    pub new_message: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Validity {
    Indefinite = 0,
    PowerDown = 1,
    RegistrationArea = 2,
    DisplayOnly = 3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DisplayTime {
    Temporary = 0,
    Default = 1,
    Invoke = 2,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmsMessage {
    DeliveryAck {
        message_reference: u16,
        user_data: UserData,
    },
    ManualAck {
        message_reference: u16,
        user_data: UserData,
    },
    Submit {
        metadata: Metadata,
        user_data: UserData,
    },
    Deliver {
        metadata: Metadata,
        validity: Validity,
        display_time: DisplayTime,
        user_data: UserData,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSms {
    pub message: SmsMessage,
    pub consumed_octets: usize,
    pub optional_octets: Vec<u8>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmsError {
    Truncated,
    Length,
    MessageType,
    Field(&'static str),
    Reserved,
    Encoding,
}
impl fmt::Display for SmsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid IS-136 SMS: {self:?}")
    }
}
impl Error for SmsError {}
impl SmsMessage {
    // IS-136.1 (1994) §§7.2.1–4. SUBMIT and DELIVER share Message Type Indicator 000.
    pub fn encode(&self) -> Result<Vec<u8>, SmsError> {
        let acknowledgment = match self {
            Self::DeliveryAck {
                message_reference,
                user_data,
            } => Some((DELIVERY_ACK_TYPE, *message_reference, user_data)),
            Self::ManualAck {
                message_reference,
                user_data,
            } => Some((MANUAL_ACK_TYPE, *message_reference, user_data)),
            _ => None,
        };
        if let Some((kind, message_reference, user_data)) = acknowledgment {
            if message_reference > REFERENCE_MASK {
                return Err(SmsError::Field("message_reference"));
            }
            let header = message_reference | (u16::from(kind) << REFERENCE_BITS);
            let mut bytes = header.to_be_bytes().to_vec();
            bytes.extend(user_data.encode()?);
            return Ok(bytes);
        }
        let (metadata, user_data) = match self {
            Self::Submit {
                metadata,
                user_data,
            }
            | Self::Deliver {
                metadata,
                user_data,
                ..
            } => (metadata, user_data),
            _ => unreachable!("acknowledgment variants already encoded"),
        };
        if metadata.message_reference > REFERENCE_MASK {
            return Err(SmsError::Field("message_reference"));
        }
        if metadata.privacy > 3 {
            return Err(SmsError::Field("privacy"));
        }
        if metadata.urgency > URGENCY_MASK {
            return Err(SmsError::Field("urgency"));
        }
        let mut bytes = metadata.message_reference.to_be_bytes().to_vec();
        bytes.push(
            (metadata.privacy << PRIVACY_SHIFT)
                | (metadata.urgency << URGENCY_SHIFT)
                | (u8::from(metadata.delivery_acknowledgment) << DELIVERY_SHIFT)
                | (u8::from(metadata.manual_acknowledgment) << MANUAL_SHIFT)
                | u8::from(metadata.new_message),
        );
        if let Self::Deliver {
            validity,
            display_time,
            ..
        } = self
        {
            bytes
                .push((*validity as u8) << VALIDITY_SHIFT | (*display_time as u8) << DISPLAY_SHIFT);
        }
        bytes.extend(user_data.encode()?);
        Ok(bytes)
    }
}
/// Appends a Message Center Time Stamp to an SMS DELIVER with no optional elements.
/// Returns `None` when the PDU is not such a DELIVER, the time precedes 1980 or the result
/// would not fit in one R-DATA.
pub fn with_time_stamp(
    pdu: &[u8],
    received_unix_ms: i64,
    time_zone: super::time_date::TimeZoneOffset,
) -> Option<Vec<u8>> {
    let decoded = decode_prefix(pdu, Direction::Forward).ok()?;
    if !matches!(decoded.message, SmsMessage::Deliver { .. })
        || !decoded.optional_octets.is_empty()
        || pdu.len() + TIME_STAMP_OCTETS > super::relay::MAX_DATA_PAYLOAD_OCTETS
    {
        return None;
    }
    let seconds =
        u32::try_from(received_unix_ms.div_euclid(1000) - super::time_date::UNIX_TO_1980_SECONDS)
            .ok()?;
    let value = TIME_STAMP_PARAMETER << TIME_STAMP_PARAMETER_SHIFT
        | u64::from(seconds) << TIME_STAMP_SECONDS_SHIFT
        | u64::from(time_zone.bits());
    let mut stamped = pdu.to_vec();
    stamped.extend_from_slice(&value.to_be_bytes()[u64::BITS as usize / 8 - TIME_STAMP_OCTETS..]);
    Some(stamped)
}
pub fn decode_prefix(bytes: &[u8], direction: Direction) -> Result<DecodedSms, SmsError> {
    let reference = bytes.get(..REFERENCE_OCTETS).ok_or(SmsError::Truncated)?;
    let kind = reference[0] >> TYPE_SHIFT;
    if matches!(kind, DELIVERY_ACK_TYPE | MANUAL_ACK_TYPE) {
        let message_reference = u16::from_be_bytes([reference[0], reference[1]]) & REFERENCE_MASK;
        let (user_data, end) = decode_unit(bytes, REFERENCE_OCTETS)?;
        let message = if kind == DELIVERY_ACK_TYPE {
            SmsMessage::DeliveryAck {
                message_reference,
                user_data,
            }
        } else {
            SmsMessage::ManualAck {
                message_reference,
                user_data,
            }
        };
        return Ok(DecodedSms {
            message,
            consumed_octets: end,
            optional_octets: bytes[end..].to_vec(),
        });
    }
    let header = bytes.get(..HEADER_OCTETS).ok_or(SmsError::Truncated)?;
    if header[0] >> TYPE_SHIFT != 0 {
        return Err(SmsError::MessageType);
    }
    let privacy = header[2] >> PRIVACY_SHIFT;
    if privacy > 3 {
        return Err(SmsError::Field("privacy"));
    }
    let metadata = Metadata {
        message_reference: u16::from_be_bytes([header[0], header[1]]) & REFERENCE_MASK,
        privacy,
        urgency: (header[2] >> URGENCY_SHIFT) & URGENCY_MASK,
        delivery_acknowledgment: header[2] & (1 << DELIVERY_SHIFT) != 0,
        manual_acknowledgment: header[2] & (1 << MANUAL_SHIFT) != 0,
        new_message: header[2] & 1 != 0,
    };
    let mut offset = HEADER_OCTETS;
    let display = if direction == Direction::Forward {
        let field = *bytes.get(offset).ok_or(SmsError::Truncated)?;
        offset += 1;
        if field & RESERVED_MASK != 0 {
            return Err(SmsError::Reserved);
        }
        let validity = match field >> VALIDITY_SHIFT {
            0 => Validity::Indefinite,
            1 => Validity::PowerDown,
            2 => Validity::RegistrationArea,
            3 => Validity::DisplayOnly,
            _ => return Err(SmsError::Field("validity")),
        };
        let display_time = match (field >> DISPLAY_SHIFT) & URGENCY_MASK {
            0 => DisplayTime::Temporary,
            1 => DisplayTime::Default,
            2 => DisplayTime::Invoke,
            _ => return Err(SmsError::Field("display_time")),
        };
        Some((validity, display_time))
    } else {
        None
    };
    let (user_data, end) = decode_unit(bytes, offset)?;
    let message = if let Some((validity, display_time)) = display {
        SmsMessage::Deliver {
            metadata,
            validity,
            display_time,
            user_data,
        }
    } else {
        SmsMessage::Submit {
            metadata,
            user_data,
        }
    };
    Ok(DecodedSms {
        message,
        consumed_octets: end,
        optional_octets: bytes[end..].to_vec(),
    })
}
fn decode_unit(bytes: &[u8], offset: usize) -> Result<(UserData, usize), SmsError> {
    let unit_header = bytes
        .get(offset..offset + UNIT_HEADER_OCTETS)
        .ok_or(SmsError::Truncated)?;
    let length = usize::from(unit_header[0]);
    if length == 0 {
        return Err(SmsError::Length);
    }
    if unit_header[1] & RESERVED_MASK != 0 {
        return Err(SmsError::Reserved);
    }
    let end = offset + 1 + length;
    let data = bytes
        .get(offset + UNIT_HEADER_OCTETS..end)
        .ok_or(SmsError::Truncated)?;
    let user_data = UserData::from_wire(unit_header[1] >> ENCODING_SHIFT, data)?;
    Ok((user_data, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metadata() -> Metadata {
        Metadata {
            message_reference: 42,
            privacy: 0,
            urgency: 1,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        }
    }
    #[test]
    fn acknowledgments_preserve_reference_user_data_and_optional_suffix_in_both_directions() {
        let delivery = SmsMessage::DeliveryAck {
            message_reference: 42,
            user_data: UserData::Ira(Vec::new()),
        };
        assert_eq!(delivery.encode().unwrap(), [0x20, 42, 1, 8]);
        let manual = SmsMessage::ManualAck {
            message_reference: 42,
            user_data: UserData::Ira(b"OK".to_vec()),
        };
        assert_eq!(manual.encode().unwrap(), [0x40, 42, 3, 8, b'O', b'K']);
        for direction in [Direction::Forward, Direction::Reverse] {
            for message in [&delivery, &manual] {
                let mut bytes = message.encode().unwrap();
                bytes.push(0xff);
                let decoded = decode_prefix(&bytes, direction).unwrap();
                assert_eq!(&decoded.message, message);
                assert_eq!(decoded.optional_octets, [0xff]);
            }
        }
        assert_eq!(
            SmsMessage::DeliveryAck {
                message_reference: 8192,
                user_data: UserData::Ira(Vec::new())
            }
            .encode(),
            Err(SmsError::Field("message_reference"))
        );
    }

    #[test]
    fn submit_and_deliver_match_fixed_ira_vectors() {
        let submit = SmsMessage::Submit {
            metadata: metadata(),
            user_data: UserData::Ira(b"Hi".to_vec()),
        };
        assert_eq!(submit.encode().unwrap(), [0, 42, 9, 3, 8, 72, 105]);
        assert_eq!(
            decode_prefix(&[0, 42, 9, 3, 8, 72, 105], Direction::Reverse)
                .unwrap()
                .message,
            submit
        );
        let deliver = SmsMessage::Deliver {
            metadata: metadata(),
            validity: Validity::Indefinite,
            display_time: DisplayTime::Default,
            user_data: UserData::Ira(b"Hi".to_vec()),
        };
        assert_eq!(deliver.encode().unwrap(), [0, 42, 9, 8, 3, 8, 72, 105]);
        let decoded =
            decode_prefix(&[0, 42, 9, 8, 3, 8, 72, 105, 0xff], Direction::Forward).unwrap();
        assert_eq!(decoded.message, deliver);
        assert_eq!(decoded.optional_octets, [0xff]);
    }
    #[test]
    fn malformed_metadata_encoding_reserved_bits_and_lengths_are_rejected() {
        assert_eq!(
            decode_prefix(&[0x60, 0, 0], Direction::Reverse),
            Err(SmsError::MessageType)
        );
        assert_eq!(
            decode_prefix(&[0, 0, 0x80], Direction::Reverse),
            Err(SmsError::Field("privacy"))
        );
        assert_eq!(
            decode_prefix(&[0, 0, 0, 1, 9], Direction::Reverse),
            Err(SmsError::Reserved)
        );
        assert_eq!(
            decode_prefix(&[0, 0, 0, 2, 8, 0x80], Direction::Reverse),
            Err(SmsError::Encoding)
        );
        assert_eq!(
            decode_prefix(&[0, 0, 0, 2, 8], Direction::Reverse),
            Err(SmsError::Truncated)
        );
        let message = SmsMessage::Submit {
            metadata: metadata(),
            user_data: UserData::UserSpecific(vec![0xff; 254]),
        };
        assert_eq!(
            decode_prefix(&message.encode().unwrap(), Direction::Reverse)
                .unwrap()
                .message,
            message
        );
        assert_eq!(
            SmsMessage::Submit {
                metadata: metadata(),
                user_data: UserData::Ira(vec![0; 255])
            }
            .encode(),
            Err(SmsError::Length)
        );
    }
}

#[cfg(test)]
mod international_tests {
    use super::*;
    #[test]
    fn international_submit_deliver_and_both_acknowledgments_preserve_data_and_suffix() {
        for data in [
            UserData::from_text("café").unwrap(),
            UserData::from_text("日本語").unwrap(),
            UserData::International {
                encoding: InternationalEncoding::Iso8859_8,
                data: vec![0xe0, 0xe1],
            },
        ] {
            let metadata = Metadata {
                message_reference: 42,
                privacy: 0,
                urgency: 1,
                delivery_acknowledgment: true,
                manual_acknowledgment: true,
                new_message: true,
            };
            for (message, direction) in [
                (
                    SmsMessage::Submit {
                        metadata,
                        user_data: data.clone(),
                    },
                    Direction::Reverse,
                ),
                (
                    SmsMessage::Deliver {
                        metadata,
                        validity: Validity::Indefinite,
                        display_time: DisplayTime::Default,
                        user_data: data.clone(),
                    },
                    Direction::Forward,
                ),
                (
                    SmsMessage::DeliveryAck {
                        message_reference: 42,
                        user_data: data.clone(),
                    },
                    Direction::Reverse,
                ),
                (
                    SmsMessage::ManualAck {
                        message_reference: 42,
                        user_data: data.clone(),
                    },
                    Direction::Forward,
                ),
            ] {
                let mut wire = message.encode().unwrap();
                let length = wire.len();
                wire.extend([0x80, 0x11]);
                let decoded = decode_prefix(&wire, direction).unwrap();
                assert_eq!(decoded.message, message);
                assert_eq!(decoded.consumed_octets, length);
                assert_eq!(decoded.optional_octets, [0x80, 0x11]);
            }
        }
    }

    #[test]
    fn deliver_time_stamp_follows_the_user_data_unit() {
        let deliver = SmsMessage::Deliver {
            metadata: Metadata {
                message_reference: 1,
                privacy: 0,
                urgency: NORMAL_URGENCY,
                delivery_acknowledgment: false,
                manual_acknowledgment: false,
                new_message: true,
            },
            validity: Validity::Indefinite,
            display_time: DisplayTime::Default,
            user_data: UserData::Ira(b"hi".to_vec()),
        }
        .encode()
        .unwrap();
        let received_ms = 1_792_983_066_105;
        let zone = crate::l3::time_date::TimeZoneOffset::try_new(-420, true).unwrap();
        let stamped = with_time_stamp(&deliver, received_ms, zone).unwrap();
        assert_eq!(&stamped[..deliver.len()], deliver.as_slice());
        let seconds = (received_ms / 1000 - crate::l3::time_date::UNIX_TO_1980_SECONDS) as u64;
        let expected = (0b0010u64 << 44 | seconds << 12 | u64::from(zone.bits())).to_be_bytes();
        assert_eq!(&stamped[deliver.len()..], &expected[2..]);
        let decoded = decode_prefix(&stamped, Direction::Forward).unwrap();
        assert_eq!(decoded.optional_octets, &expected[2..]);
        assert!(with_time_stamp(&stamped, received_ms, zone).is_none());
    }
}
