mod reject_time;
pub use reject_time::{DEFAULT_REJECT_SUPERFRAMES, RegistrationRetry, RejectTime};
mod serial;
pub use serial::SerialNumber;
mod options;
use super::broadcast::ProtocolVersion;
pub use options::{
    AcceptOptions, EncryptionMode, GroupRequest, MultilingualNumber, OptionsError,
    RegistrationOptions, RegistrationRequest,
};
use std::{error::Error, fmt};

const PROTOCOL_BITS: usize = 2;
const TYPE_BITS: usize = 6;
const REGISTRATION_BITS: usize = 4;
const SCM_BITS: usize = 5;
const VERSION_BITS: usize = 4;
const ESN_BITS: usize = 32;
const OCTET_BITS: usize = 8;
const REGISTRATION: u8 = 12;
const SERIAL_NUMBER: u8 = 13;
const REGISTRATION_ACCEPT: u8 = 29;
const REGISTRATION_REJECT: u8 = 30;
const CAUSE_BITS: usize = 4;
const IS136_PROTOCOL: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RegistrationType {
    PowerDown = 0,
    PowerUp = 1,
    LocationArea = 2,
    Forced = 3,
    Periodic = 4,
    Deregistration = 5,
    NewSystem = 6,
    AccToDcch = 7,
    TmsiTimeout = 8,
    UserGroup = 9,
    NewHyperband = 10,
    PcchToDcch = 11,
    DcchToPcch = 12,
}
impl RegistrationType {
    fn decode(value: u8) -> Result<Self, RegistrationError> {
        match value {
            0 => Ok(Self::PowerDown),
            1 => Ok(Self::PowerUp),
            2 => Ok(Self::LocationArea),
            3 => Ok(Self::Forced),
            4 => Ok(Self::Periodic),
            5 => Ok(Self::Deregistration),
            6 => Ok(Self::NewSystem),
            7 => Ok(Self::AccToDcch),
            8 => Ok(Self::TmsiTimeout),
            9 => Ok(Self::UserGroup),
            10 => Ok(Self::NewHyperband),
            11 => Ok(Self::PcchToDcch),
            12 => Ok(Self::DcchToPcch),
            _ => Err(RegistrationError::Field("registration_type")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMessage {
    Registration {
        registration_type: RegistrationType,
        scm: u8,
        protocol_version: ProtocolVersion,
    },
    SerialNumber {
        esn: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RejectCause {
    UnknownMsid = 0,
    Congestion = 1,
    RssTooLow = 2,
    Unknown = 3,
    OperatorDeterminedBarring = 4,
    UserGroupRelease = 5,
    UserGroupBarred = 6,
    ServiceCodeNotSupported = 7,
    ServiceCodeNotAvailable = 8,
    AuthenticationFailure = 9,
    AddressingNotSupported = 10,
    EncryptionModeTemporarilyDenied = 11,
    UnknownUgid = 12,
    PsidRsidRemoval = 13,
    VoiceCodecNotAvailable = 14,
    Extension = 15,
}
impl RejectCause {
    pub(super) fn decode(code: u8) -> Result<Self, RegistrationError> {
        const CAUSES: [RejectCause; 16] = [
            RejectCause::UnknownMsid,
            RejectCause::Congestion,
            RejectCause::RssTooLow,
            RejectCause::Unknown,
            RejectCause::OperatorDeterminedBarring,
            RejectCause::UserGroupRelease,
            RejectCause::UserGroupBarred,
            RejectCause::ServiceCodeNotSupported,
            RejectCause::ServiceCodeNotAvailable,
            RejectCause::AuthenticationFailure,
            RejectCause::AddressingNotSupported,
            RejectCause::EncryptionModeTemporarilyDenied,
            RejectCause::UnknownUgid,
            RejectCause::PsidRsidRemoval,
            RejectCause::VoiceCodecNotAvailable,
            RejectCause::Extension,
        ];
        CAUSES
            .get(usize::from(code))
            .copied()
            .ok_or(RegistrationError::Field("cause"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationResponse {
    Accept,
    Reject { cause: RejectCause },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedRegistrationResponse {
    pub message: RegistrationResponse,
    pub consumed_bits: usize,
    /// Unparsed optional elements and final octet padding remain in this suffix.
    pub remaining_bits: Vec<bool>,
}
impl RegistrationResponse {
    // IS-136.1-A §§6.4.1.1, 6.4.3.14 and 6.4.3.15. Mandatory fields only.
    pub fn encode(self) -> Vec<u8> {
        match self {
            Self::Accept => vec![REGISTRATION_ACCEPT],
            Self::Reject { cause } => vec![REGISTRATION_REJECT, (cause as u8) << CAUSE_BITS],
        }
    }
    pub fn decode_prefix(octets: &[u8]) -> Result<DecodedRegistrationResponse, RegistrationError> {
        let header = *octets.first().ok_or(RegistrationError::Truncated {
            needed_bits: OCTET_BITS,
        })?;
        if u64::from(header >> TYPE_BITS) != IS136_PROTOCOL {
            return Err(RegistrationError::Protocol);
        }
        let (message, consumed_bits) = match header {
            REGISTRATION_ACCEPT => (Self::Accept, OCTET_BITS),
            REGISTRATION_REJECT => {
                let cause_octet = *octets.get(1).ok_or(RegistrationError::Truncated {
                    needed_bits: OCTET_BITS + CAUSE_BITS,
                })?;
                (
                    Self::Reject {
                        cause: RejectCause::decode(cause_octet >> CAUSE_BITS)?,
                    },
                    OCTET_BITS + CAUSE_BITS,
                )
            }
            _ => return Err(RegistrationError::UnsupportedType(header)),
        };
        let remaining_bits = octets
            .iter()
            .flat_map(|octet| {
                (0..OCTET_BITS)
                    .rev()
                    .map(move |bit| octet & (1 << bit) != 0)
            })
            .skip(consumed_bits)
            .collect();
        Ok(DecodedRegistrationResponse {
            message,
            consumed_bits,
            remaining_bits,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationError {
    Truncated { needed_bits: usize },
    Protocol,
    UnsupportedType(u8),
    Field(&'static str),
}
impl fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid IS-136 registration message: {self:?}")
    }
}
impl Error for RegistrationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedAccessMessage {
    pub message: AccessMessage,
    pub consumed_bits: usize,
    /// Unparsed optional elements and final octet padding remain in this suffix.
    pub remaining_bits: Vec<bool>,
}

impl AccessMessage {
    // IS-136.1-A §§6.4.1.1, 6.4.4.12 and 6.4.4.13. Mandatory fields only.
    pub fn encode(self) -> Result<Vec<u8>, RegistrationError> {
        let mut bits = Vec::new();
        put(&mut bits, IS136_PROTOCOL, PROTOCOL_BITS);
        match self {
            Self::Registration {
                registration_type,
                scm,
                protocol_version,
            } => {
                if scm >= 1 << SCM_BITS {
                    return Err(RegistrationError::Field("scm"));
                }
                put(&mut bits, u64::from(REGISTRATION), TYPE_BITS);
                put(&mut bits, registration_type as u64, REGISTRATION_BITS);
                put(&mut bits, u64::from(scm), SCM_BITS);
                put(&mut bits, u64::from(protocol_version.code()), VERSION_BITS);
            }
            Self::SerialNumber { esn } => {
                put(&mut bits, u64::from(SERIAL_NUMBER), TYPE_BITS);
                put(&mut bits, u64::from(esn), ESN_BITS);
            }
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

// IS-136.1-A §6.4.4. Mandatory fields precede optional information elements.
pub fn decode_prefix(octets: &[u8]) -> Result<DecodedAccessMessage, RegistrationError> {
    let bits: Vec<bool> = octets
        .iter()
        .flat_map(|octet| {
            (0..OCTET_BITS)
                .rev()
                .map(move |bit| octet & (1 << bit) != 0)
        })
        .collect();
    let mut offset = 0;
    let mut take = |width: usize| {
        let end = offset + width;
        let field = bits
            .get(offset..end)
            .ok_or(RegistrationError::Truncated { needed_bits: end })?;
        offset = end;
        Ok::<u64, RegistrationError>(
            field
                .iter()
                .fold(0, |value, bit| value * 2 + u64::from(*bit)),
        )
    };
    if take(PROTOCOL_BITS)? != IS136_PROTOCOL {
        return Err(RegistrationError::Protocol);
    }
    let kind = take(TYPE_BITS)? as u8;
    let message = match kind {
        REGISTRATION => AccessMessage::Registration {
            registration_type: RegistrationType::decode(take(REGISTRATION_BITS)? as u8)?,
            scm: take(SCM_BITS)? as u8,
            protocol_version: ProtocolVersion::decode(take(VERSION_BITS)? as u8)
                .map_err(|_| RegistrationError::Field("protocol_version"))?,
        },
        SERIAL_NUMBER => AccessMessage::SerialNumber {
            esn: take(ESN_BITS)? as u32,
        },
        _ => return Err(RegistrationError::UnsupportedType(kind)),
    };
    Ok(DecodedAccessMessage {
        message,
        consumed_bits: offset,
        remaining_bits: bits[offset..].to_vec(),
    })
}

fn put(bits: &mut Vec<bool>, value: u64, width: usize) {
    bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
}

#[cfg(test)]
mod tests {
    #[test]
    fn later_handset_versions_preserve_the_mandatory_registration_fields() {
        for (last_octet, version) in [
            (0xa8, ProtocolVersion::Pv1),
            (0xb0, ProtocolVersion::Pv2),
            (0xb8, ProtocolVersion::Pv3),
            (0xc0, ProtocolVersion::Pv4),
            (0xc8, ProtocolVersion::Pv5),
            (0xd0, ProtocolVersion::Pv6),
            (0xd8, ProtocolVersion::Pv7),
        ] {
            let expected = AccessMessage::Registration {
                registration_type: RegistrationType::PowerUp,
                scm: 21,
                protocol_version: version,
            };
            let wire = [0x0c, 0x1a, last_octet];
            assert_eq!(decode_prefix(&wire).unwrap().message, expected);
            assert_eq!(expected.encode().unwrap(), wire);
        }
    }

    use super::*;

    #[test]
    fn response_vectors_cover_all_causes_and_preserve_optional_suffix() {
        assert_eq!(RegistrationResponse::Accept.encode(), [0x1d]);
        assert_eq!(
            RegistrationResponse::decode_prefix(&[0x1d])
                .unwrap()
                .message,
            RegistrationResponse::Accept
        );
        for code in 0..16 {
            let response = RegistrationResponse::Reject {
                cause: RejectCause::decode(code).unwrap(),
            };
            let wire = [0x1e, code << 4];
            assert_eq!(response.encode(), wire);
            let decoded = RegistrationResponse::decode_prefix(&wire).unwrap();
            assert_eq!(decoded.message, response);
            assert_eq!(decoded.consumed_bits, 12);
            assert_eq!(decoded.remaining_bits, [false; 4]);
        }
        assert_eq!(
            RejectCause::decode(16),
            Err(RegistrationError::Field("cause"))
        );
        assert_eq!(
            RegistrationResponse::decode_prefix(&[0x5d]),
            Err(RegistrationError::Protocol)
        );
        assert_eq!(
            RegistrationResponse::decode_prefix(&[0x0c]),
            Err(RegistrationError::UnsupportedType(12))
        );
        assert!(matches!(
            RegistrationResponse::decode_prefix(&[0x1e]),
            Err(RegistrationError::Truncated { .. })
        ));
        assert_eq!(
            RegistrationResponse::decode_prefix(&[0x1e, 0x1f, 0xff])
                .unwrap()
                .remaining_bits,
            [true; 12]
        );
    }

    #[test]
    fn required_fields_match_fixed_wire_vectors() {
        let request = AccessMessage::Registration {
            registration_type: RegistrationType::PowerUp,
            scm: 0b10101,
            protocol_version: ProtocolVersion::Is136RevA,
        };
        assert_eq!(request.encode().unwrap(), [0x0c, 0x1a, 0xa0]);
        let decoded = decode_prefix(&[0x0c, 0x1a, 0xa0]).unwrap();
        assert_eq!(decoded.message, request);
        assert_eq!(decoded.consumed_bits, 21);
        assert_eq!(decoded.remaining_bits, [false; 3]);
        let serial = AccessMessage::SerialNumber { esn: 0x12345678 };
        assert_eq!(serial.encode().unwrap(), [0x0d, 0x12, 0x34, 0x56, 0x78]);
        assert_eq!(
            decode_prefix(&serial.encode().unwrap()).unwrap().message,
            serial
        );
    }

    #[test]
    fn rejects_reserved_and_truncated_fields_without_discarding_suffix() {
        assert_eq!(
            decode_prefix(&[0x4c, 0, 0]),
            Err(RegistrationError::Protocol)
        );
        assert_eq!(
            decode_prefix(&[0x0c, 0xd0, 0]),
            Err(RegistrationError::Field("registration_type"))
        );
        assert_eq!(
            decode_prefix(&[0x0c, 0x10, 0x18]),
            Err(RegistrationError::Field("protocol_version"))
        );
        assert!(matches!(
            decode_prefix(&[0x0c, 0x10]),
            Err(RegistrationError::Truncated { .. })
        ));
        assert!(matches!(
            decode_prefix(&[0x0d, 0, 0, 0]),
            Err(RegistrationError::Truncated { .. })
        ));
        assert_eq!(
            decode_prefix(&[0x0c, 0x1a, 0xa7, 0xff])
                .unwrap()
                .remaining_bits,
            [true; 11]
        );
    }
}

#[cfg(test)]
mod registration_type_tests {
    use super::*;

    #[test]
    fn pcch_and_dcch_transitions_decode() {
        assert_eq!(
            RegistrationType::decode(11),
            Ok(RegistrationType::PcchToDcch)
        );
        assert_eq!(
            RegistrationType::decode(12),
            Ok(RegistrationType::DcchToPcch)
        );
        assert!(RegistrationType::decode(13).is_err());
    }
}
