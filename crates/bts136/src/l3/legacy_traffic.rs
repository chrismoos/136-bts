mod termination;
pub use termination::{
    AcknowledgmentInformation, ControlChannel, Hyperband, ReleaseFields, ReleaseInformation,
    ReleaseReason,
};
mod alert;
pub use alert::AlertInformation;
mod service;
pub use service::{ServiceMessage, ServiceOptions, ServiceRejection};
mod flash;
pub use flash::{
    CallInformation, CalledNumber, CallingName, CallingNumber, FlashAcknowledgment, FlashMessage,
    MessageWaiting, ReverseFlashFields,
};
mod amr_control;
pub use amr_control::{
    AmrActiveSet, AmrCodecMode, AmrConfiguration, AmrControlMessage, AmrFreeze, AmrLink, AmrPhase,
    AmrThresholds,
};
mod amr_noise;
pub use amr_noise::{ActiveAmrMode, AmrNoiseMessage, AmrNoiseParameters};
mod comfort_noise;
pub use comfort_noise::ComfortNoiseParameters;
mod dtmf;
pub use dtmf::{DtmfDigit, DtmfMessage};
mod physical_control;
pub use physical_control::{
    MAX_ALIGNMENT_UNITS, MAX_POWER_LEVEL, PhysicalControlFields, PhysicalControlRequest,
};
mod capabilities;
use crate::l3::{broadcast::ProtocolVersion, relay::Direction};
pub use capabilities::{
    BaseStationCapabilities, CapabilityFields, CapabilityResponse, CharacterSupport,
    MobileCapabilities, MobilePv1, MobilePv5, StationPv1, Subaddress,
};
mod status;
pub use status::{StatusFields, TerminalInformation, TrafficTask};
mod relay;
pub use relay::TrafficRelay;
mod measurements;
pub use measurements::{ChannelQualityReport, DownlinkQuality, NeighborGroup};
mod extended_measurements;
pub use extended_measurements::{
    ExtendedChannelQualityReport, ExtendedCurrentQuality, ExtendedMeasurementOrder,
    ExtendedMeasurementResults, ExtendedMeasurementTargets, ExtendedMeasurementType,
    ExtendedNeighborMeasurement, ExtendedQualityMetric, ExtendedServingMeasurement,
    MAX_EXTENDED_MEASUREMENT_CHANNELS, MeasurementChannel,
};
use std::{error::Error, fmt};

const PROTOCOL_BITS: usize = 2;
const TYPE_BITS: usize = 8;
const HEADER_BITS: usize = PROTOCOL_BITS + TYPE_BITS;
const LENGTH_BITS: usize = 6;
const OCTET_BITS: usize = 8;
const PHYSICAL_CONTROL: u8 = 0xf2;
const PHYSICAL_CONTROL_ACK: u8 = 0x91;
const CONNECT: u8 = 0xc1;
const ALERT: u8 = 0x31;
const RELEASE: u8 = ReleaseInformation::MESSAGE_TYPE;
const ACK: u8 = 0x54;
const AUDIT: u8 = 0x61;
const CAPABILITY_REQUEST: u8 = 0x04;
const CAPABILITY_RESPONSE: u8 = 0x0a;
const STATUS_REQUEST: u8 = 0xf7;
const STATUS: u8 = 0x97;
const MEASUREMENT_ORDER: u8 = 0x52;
const MEASUREMENT_ORDER_ACK: u8 = 0xc2;
const STOP_MEASUREMENTS: u8 = 0xc7;
const RF_CHANNEL_PARAMETER: u64 = 1;
const PARAMETER_BITS: usize = 4;
const CHANNEL_COUNT_BITS: usize = 6;
const RF_CHANNEL_BITS: usize = 11;
pub const MAX_MEASUREMENT_CHANNELS: usize = 24;
const LEGACY_PROTOCOL: u64 = 0;
pub const MAX_OPTIONAL_OCTETS: usize = (1 << LENGTH_BITS) - 1;
pub const MAX_MESSAGE_BITS: usize = relay::MAX_MESSAGE_BITS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrafficMessage {
    Connect,
    Dtmf(DtmfMessage),
    Flash(FlashMessage),
    Service(ServiceMessage),
    Audit,
    PhysicalControl(PhysicalControlFields),
    PhysicalControlAck(PhysicalControlFields),
    CapabilityUpdateRequest(ProtocolVersion),
    CapabilityUpdateResponse(CapabilityResponse),
    StatusRequest(StatusFields),
    Status(StatusFields),
    MeasurementOrder {
        rf_channels: Vec<u16>,
    },
    MeasurementOrderAck {
        rf_channels: Vec<u16>,
    },
    StopMeasurements,
    AmrControl(AmrControlMessage),
    AmrNoise(AmrNoiseMessage),
    ComfortNoise(ComfortNoiseParameters),
    ChannelQuality(ChannelQualityReport),
    ExtendedChannelQuality(ExtendedChannelQualityReport),
    ExtendedMeasurementOrder(ExtendedMeasurementOrder),
    ExtendedMeasurementAck(ExtendedMeasurementTargets),
    Relay(TrafficRelay),
    AlertWithInfo {
        information: AlertInformation,
    },
    Release {
        information: ReleaseInformation,
    },
    Ack {
        ack_message_type: u8,
        information: AcknowledgmentInformation,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficMessageError {
    Truncated { needed_bits: usize },
    Protocol,
    UnsupportedType(u8),
    OptionalLength,
    Field(&'static str),
    Relay(crate::l3::relay::RelayError),
    Addressing(crate::l3::relay::addressing::AddressingError),
}
impl fmt::Display for TrafficMessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid IS-136 traffic message: {self:?}")
    }
}
impl Error for TrafficMessageError {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedMessage {
    pub message: TrafficMessage,
    pub consumed_bits: usize,
}

impl TrafficMessage {
    // IS-54-B §2.7.3.1.3.3 Message Type.
    pub fn message_type(&self) -> u8 {
        match self {
            Self::Dtmf(message) => message.message_type(),
            Self::Flash(message) => message.message_type(),
            Self::Service(message) => message.message_type(),
            Self::Connect => CONNECT,
            Self::Audit => AUDIT,
            Self::PhysicalControl(_) => PHYSICAL_CONTROL,
            Self::PhysicalControlAck(_) => PHYSICAL_CONTROL_ACK,
            Self::CapabilityUpdateRequest(_) => CAPABILITY_REQUEST,
            Self::CapabilityUpdateResponse(_) => CAPABILITY_RESPONSE,
            Self::StatusRequest(_) => STATUS_REQUEST,
            Self::Status(_) => STATUS,
            Self::MeasurementOrder { .. } => MEASUREMENT_ORDER,
            Self::MeasurementOrderAck { .. } => MEASUREMENT_ORDER_ACK,
            Self::StopMeasurements => STOP_MEASUREMENTS,
            Self::AmrControl(message) => message.message_type(),
            Self::AmrNoise(message) => message.message_type(),
            Self::ComfortNoise(parameters) => parameters.message_type(),
            Self::ChannelQuality(report) => report.message_type(),
            Self::ExtendedChannelQuality(report) => report.message_type(),
            Self::ExtendedMeasurementOrder(_) => extended_measurements::order::message_type(false),
            Self::ExtendedMeasurementAck(_) => extended_measurements::order::message_type(true),
            Self::Relay(relay) => relay.message_type(),
            Self::AlertWithInfo { .. } => ALERT,
            Self::Release { .. } => RELEASE,
            Self::Ack { .. } => ACK,
        }
    }
    // TIA-136-133-E §§5.1.2.1 and 9.1.2.1 define field order and optional-length framing.
    pub fn encode(&self) -> Result<Vec<bool>, TrafficMessageError> {
        let field_octets = match self {
            Self::Release { information } => Some(information.encode_octets()?),
            Self::Ack { information, .. } => Some(information.encode_octets()?),
            Self::AlertWithInfo { information } => Some(information.encode_octets()?),
            Self::PhysicalControl(fields) => Some(fields.encode_octets()?),
            Self::PhysicalControlAck(fields) => Some(fields.encode_octets()?),
            Self::StatusRequest(fields) => {
                fields.validate_request()?;
                Some(fields.encode_octets()?)
            }
            Self::Status(fields) => Some(fields.encode_octets()?),
            _ => None,
        };
        let optional = match self {
            Self::Release { .. }
            | Self::Ack { .. }
            | Self::AlertWithInfo { .. }
            | Self::StatusRequest(_)
            | Self::Status(_)
            | Self::PhysicalControl(_)
            | Self::PhysicalControlAck(_) => field_octets.as_ref(),
            Self::Service(_)
            | Self::Flash(_)
            | Self::Dtmf(_)
            | Self::Connect
            | Self::Audit
            | Self::CapabilityUpdateRequest(_)
            | Self::CapabilityUpdateResponse(_)
            | Self::AmrControl(_)
            | Self::AmrNoise(_)
            | Self::ComfortNoise(_)
            | Self::ChannelQuality(_)
            | Self::ExtendedChannelQuality(_)
            | Self::ExtendedMeasurementOrder(_)
            | Self::ExtendedMeasurementAck(_)
            | Self::Relay(_)
            | Self::MeasurementOrder { .. }
            | Self::MeasurementOrderAck { .. }
            | Self::StopMeasurements => None,
        };
        if optional.is_some_and(|bytes| bytes.len() > MAX_OPTIONAL_OCTETS) {
            return Err(TrafficMessageError::OptionalLength);
        }
        let mut bits = Vec::new();
        put(&mut bits, LEGACY_PROTOCOL, PROTOCOL_BITS);
        put(&mut bits, u64::from(self.message_type()), TYPE_BITS);
        match self {
            Self::AmrControl(message) => {
                message.encode_fields(&mut bits)?;
                return Ok(bits);
            }
            Self::AmrNoise(message) => {
                message.encode_fields(&mut bits)?;
                return Ok(bits);
            }
            Self::ComfortNoise(parameters) => {
                parameters.encode_fields(&mut bits)?;
                return Ok(bits);
            }
            Self::ExtendedMeasurementOrder(order) => {
                order.encode_fields(&mut bits);
                return Ok(bits);
            }
            Self::ExtendedMeasurementAck(targets) => {
                targets.encode_fields(&mut bits);
                return Ok(bits);
            }
            Self::Service(message) => {
                message.encode_fields(&mut bits)?;
                return Ok(bits);
            }
            Self::Flash(message) => {
                message.encode_fields(&mut bits)?;
                return Ok(bits);
            }
            Self::Dtmf(message) => {
                message.encode_fields(&mut bits)?;
                return Ok(bits);
            }
            Self::CapabilityUpdateRequest(version) => {
                put(&mut bits, u64::from(version.code()), capabilities::PV_BITS);
                return Ok(bits);
            }
            Self::CapabilityUpdateResponse(response) => {
                response.encode_fields(&mut bits)?;
                return Ok(bits);
            }
            _ => {}
        }
        if let Self::Relay(relay) = self {
            relay.encode_fields(&mut bits)?;
            return Ok(bits);
        }
        if let Self::MeasurementOrder { rf_channels } | Self::MeasurementOrderAck { rf_channels } =
            self
        {
            if rf_channels.len() > MAX_MEASUREMENT_CHANNELS
                || rf_channels
                    .iter()
                    .any(|channel| *channel >= 1 << RF_CHANNEL_BITS)
            {
                return Err(TrafficMessageError::Field("rf_channels"));
            }
            put(&mut bits, RF_CHANNEL_PARAMETER, PARAMETER_BITS);
            put(&mut bits, rf_channels.len() as u64, CHANNEL_COUNT_BITS);
            for channel in rf_channels {
                put(&mut bits, u64::from(*channel), RF_CHANNEL_BITS);
            }
            return Ok(bits);
        }
        if let Self::ChannelQuality(report) = self {
            report.encode_fields(&mut bits)?;
            return Ok(bits);
        }
        if let Self::ExtendedChannelQuality(report) = self {
            report.encode_fields(&mut bits)?;
            return Ok(bits);
        }
        if let Self::Ack {
            ack_message_type, ..
        } = self
        {
            put(&mut bits, u64::from(*ack_message_type), TYPE_BITS);
        }
        if let Some(optional) = optional {
            put(&mut bits, optional.len() as u64, LENGTH_BITS);
            for byte in optional {
                put(&mut bits, u64::from(*byte), OCTET_BITS);
            }
        }
        Ok(bits)
    }
}

/// Decodes mobile-to-base traffic. consumed_bits excludes signaling-word filler.
pub fn decode_prefix(bits: &[bool]) -> Result<DecodedMessage, TrafficMessageError> {
    decode_prefix_in(bits, Direction::Reverse)
}

// TIA-136-133-E §§5.1.2.2.23 and 9.1.2.2.23 assign different layouts to type 0x0a.
pub fn decode_prefix_in(
    bits: &[bool],
    direction: Direction,
) -> Result<DecodedMessage, TrafficMessageError> {
    decode_prefix_in_context(bits, direction, None)
}
// TIA-136-133-E §9.1.2.2.32 requires the PV6 field even when its value matches signaling filler.
pub fn decode_prefix_in_version(
    bits: &[bool],
    direction: Direction,
    version: ProtocolVersion,
) -> Result<DecodedMessage, TrafficMessageError> {
    decode_prefix_in_context(bits, direction, Some(version))
}
fn decode_prefix_in_context(
    bits: &[bool],
    direction: Direction,
    version: Option<ProtocolVersion>,
) -> Result<DecodedMessage, TrafficMessageError> {
    let mut reader = Reader { bits, offset: 0 };
    if reader.take(PROTOCOL_BITS)? != LEGACY_PROTOCOL {
        return Err(TrafficMessageError::Protocol);
    }
    let kind = reader.take(TYPE_BITS)? as u8;
    // TIA-136-133-E §§5.1.2.2 and 9.1.2.2 distinguish RDTC responses from FDTC orders.
    let required_direction = match kind {
        CONNECT | MEASUREMENT_ORDER_ACK | STATUS => Some(Direction::Reverse),
        AUDIT | MEASUREMENT_ORDER | STOP_MEASUREMENTS | STATUS_REQUEST => Some(Direction::Forward),
        _ => None,
    };
    if required_direction.is_some_and(|required| required != direction) {
        return Err(TrafficMessageError::Field("traffic_direction"));
    }
    if let Some(message) = amr_control::decode(kind, &mut reader, direction, version)? {
        return Ok(DecodedMessage {
            message: TrafficMessage::AmrControl(message),
            consumed_bits: reader.offset,
        });
    }
    if let Some(message) = amr_noise::decode(kind, &mut reader, direction, version)? {
        return Ok(DecodedMessage {
            message: TrafficMessage::AmrNoise(message),
            consumed_bits: reader.offset,
        });
    }
    if let Some(parameters) = comfort_noise::decode(kind, &mut reader, direction, version)? {
        return Ok(DecodedMessage {
            message: TrafficMessage::ComfortNoise(parameters),
            consumed_bits: reader.offset,
        });
    }
    if let Some(message) =
        extended_measurements::order::decode(kind, &mut reader, direction, version)?
    {
        return Ok(DecodedMessage {
            message,
            consumed_bits: reader.offset,
        });
    }
    if let Some(message) = service::decode(kind, direction, &mut reader)? {
        return Ok(DecodedMessage {
            message: TrafficMessage::Service(message),
            consumed_bits: reader.offset,
        });
    }
    if let Some(message) = flash::decode(kind, direction, &mut reader)? {
        return Ok(DecodedMessage {
            message: TrafficMessage::Flash(message),
            consumed_bits: reader.offset,
        });
    }
    if let Some(message) = dtmf::decode(kind, direction, &mut reader)? {
        return Ok(DecodedMessage {
            message: TrafficMessage::Dtmf(message),
            consumed_bits: reader.offset,
        });
    }
    if let Some(message) = capabilities::decode(kind, direction, &mut reader)? {
        return Ok(DecodedMessage {
            message,
            consumed_bits: reader.offset,
        });
    }
    if let Some(relay) = relay::decode(kind, &mut reader, direction)? {
        return Ok(DecodedMessage {
            message: TrafficMessage::Relay(relay),
            consumed_bits: reader.offset,
        });
    }
    if let Some(report) = measurements::decode(kind, &mut reader, direction)? {
        return Ok(DecodedMessage {
            message: TrafficMessage::ChannelQuality(report),
            consumed_bits: reader.offset,
        });
    }
    if let Some(report) = extended_measurements::decode(kind, &mut reader, direction)? {
        if let Some(version) = version {
            report.validate_version(version)?;
        }
        return Ok(DecodedMessage {
            message: TrafficMessage::ExtendedChannelQuality(report),
            consumed_bits: reader.offset,
        });
    }
    // TIA-136-133-E §§5.1.2.2.2,9.1.2.2.3 and §5.1.2.3.110 use a mandatory RF Channel IE.
    if matches!(kind, MEASUREMENT_ORDER | MEASUREMENT_ORDER_ACK) {
        if reader.take(PARAMETER_BITS)? != RF_CHANNEL_PARAMETER {
            return Err(TrafficMessageError::Field("rf_channel_parameter"));
        }
        let count = reader.take(CHANNEL_COUNT_BITS)? as usize;
        if count > MAX_MEASUREMENT_CHANNELS {
            return Err(TrafficMessageError::Field("rf_channel_count"));
        }
        let rf_channels = (0..count)
            .map(|_| reader.take(RF_CHANNEL_BITS).map(|value| value as u16))
            .collect::<Result<Vec<_>, _>>()?;
        let message = if kind == MEASUREMENT_ORDER {
            TrafficMessage::MeasurementOrder { rf_channels }
        } else {
            TrafficMessage::MeasurementOrderAck { rf_channels }
        };
        return Ok(DecodedMessage {
            message,
            consumed_bits: reader.offset,
        });
    }
    if matches!(kind, CONNECT | AUDIT | STOP_MEASUREMENTS) {
        return Ok(DecodedMessage {
            message: if kind == CONNECT {
                TrafficMessage::Connect
            } else if kind == AUDIT {
                TrafficMessage::Audit
            } else {
                TrafficMessage::StopMeasurements
            },
            consumed_bits: reader.offset,
        });
    }
    if !matches!(
        kind,
        ALERT | RELEASE | ACK | STATUS | STATUS_REQUEST | PHYSICAL_CONTROL | PHYSICAL_CONTROL_ACK
    ) {
        return Err(TrafficMessageError::UnsupportedType(kind));
    }
    if kind == ALERT && direction != Direction::Forward {
        return Err(TrafficMessageError::Field("alert_direction"));
    }
    if (kind == PHYSICAL_CONTROL && direction != Direction::Forward)
        || (kind == PHYSICAL_CONTROL_ACK && direction != Direction::Reverse)
    {
        return Err(TrafficMessageError::Field("physical_control_direction"));
    }
    let ack_message_type = if kind == ACK {
        Some(reader.take(TYPE_BITS)? as u8)
    } else {
        None
    };
    let length = reader.take(LENGTH_BITS)? as usize;
    let end = reader.offset + length * OCTET_BITS;
    if bits.len() < end {
        return Err(TrafficMessageError::Truncated { needed_bits: end });
    }
    let optional_octets = (0..length)
        .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
        .collect::<Result<Vec<_>, _>>()?;
    let message = match kind {
        PHYSICAL_CONTROL | PHYSICAL_CONTROL_ACK => {
            let fields =
                physical_control::decode(&optional_octets).map_err(|error| match error {
                    TrafficMessageError::Truncated { .. } => {
                        TrafficMessageError::Field("physical_control_length")
                    }
                    error => error,
                })?;
            if kind == PHYSICAL_CONTROL {
                TrafficMessage::PhysicalControl(fields)
            } else {
                TrafficMessage::PhysicalControlAck(fields)
            }
        }
        STATUS | STATUS_REQUEST => {
            let fields = status::decode(&optional_octets).map_err(|error| match error {
                TrafficMessageError::Truncated { .. } => {
                    TrafficMessageError::Field("status_element_length")
                }
                error => error,
            })?;
            if kind == STATUS_REQUEST {
                fields.validate_request()?;
                TrafficMessage::StatusRequest(fields)
            } else {
                TrafficMessage::Status(fields)
            }
        }
        ALERT => TrafficMessage::AlertWithInfo {
            information: alert::decode(&optional_octets)?,
        },
        RELEASE => TrafficMessage::Release {
            information: termination::decode_release(&optional_octets, direction).map_err(
                |error| match error {
                    TrafficMessageError::Truncated { .. } => {
                        TrafficMessageError::Field("release_element_length")
                    }
                    error => error,
                },
            )?,
        },
        ACK => TrafficMessage::Ack {
            ack_message_type: ack_message_type.expect("ACK field read before options"),
            information: termination::decode_ack(&optional_octets, direction).map_err(|error| {
                match error {
                    TrafficMessageError::Truncated { .. } => {
                        TrafficMessageError::Field("ack_element_length")
                    }
                    error => error,
                }
            })?,
        },
        _ => unreachable!("recognized legacy traffic message type"),
    };
    Ok(DecodedMessage {
        message,
        consumed_bits: reader.offset,
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
    fn take(&mut self, width: usize) -> Result<u64, TrafficMessageError> {
        let end = self.offset + width;
        if self.bits.len() < end {
            return Err(TrafficMessageError::Truncated { needed_bits: end });
        }
        let value = self.bits[self.offset..end]
            .iter()
            .fold(0, |value, bit| value * 2 + u64::from(*bit));
        self.offset = end;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn measurement_orders_use_mandatory_parameter_count_and_channel_fields() {
        for (kind, header) in [(false, "01010010"), (true, "11000010")] {
            let text = format!("00{header}00010000100111101111111111111111");
            let expected: Vec<bool> = text.chars().map(|bit| bit == '1').collect();
            let message = if kind {
                TrafficMessage::MeasurementOrderAck {
                    rf_channels: vec![991, 2047],
                }
            } else {
                TrafficMessage::MeasurementOrder {
                    rf_channels: vec![991, 2047],
                }
            };
            assert_eq!(message.encode().unwrap(), expected);
            let direction = if kind {
                Direction::Reverse
            } else {
                Direction::Forward
            };
            let decoded = decode_prefix_in(&expected, direction).unwrap();
            assert_eq!(decoded.message, message);
            assert_eq!(decoded.consumed_bits, 42);
        }
        let empty: Vec<bool> = "00010100100001000000"
            .chars()
            .map(|bit| bit == '1')
            .collect();
        assert_eq!(
            TrafficMessage::MeasurementOrder {
                rf_channels: vec![]
            }
            .encode()
            .unwrap(),
            empty
        );
        assert_eq!(
            TrafficMessage::StopMeasurements.encode().unwrap(),
            wire(0xc7, 10)
        );
        for channels in [vec![1; 25], vec![2048]] {
            assert!(
                TrafficMessage::MeasurementOrder {
                    rf_channels: channels
                }
                .encode()
                .is_err()
            );
        }
        let message = TrafficMessage::MeasurementOrder {
            rf_channels: vec![1; MAX_MEASUREMENT_CHANNELS],
        };
        let bits = message.encode().unwrap();
        assert_eq!(bits.len(), 284);
        assert_eq!(
            decode_prefix_in(&bits, Direction::Forward).unwrap().message,
            message
        );
        assert!(decode_prefix_in(&bits[..bits.len() - 1], Direction::Forward).is_err());
    }

    #[test]
    fn published_channel_quality_field_layouts_decode_all_four_groups() {
        let first = "00 01010001 101 10001 00000 00001 00010 00011 11110 11111";
        let bits: Vec<bool> = first
            .chars()
            .filter(|bit| *bit != ' ')
            .map(|bit| bit == '1')
            .collect();
        assert_eq!(bits.len(), 48);
        let expected = TrafficMessage::ChannelQuality(ChannelQualityReport::First {
            current: DownlinkQuality {
                ber_code: 5,
                rssi_code: 17,
            },
            neighbor_rssi_codes: [0, 1, 2, 3, 30, 31],
        });
        let decoded = decode_prefix(&bits).unwrap();
        assert_eq!(decoded.message, expected);
        assert_eq!(decoded.consumed_bits, 48);
        assert_eq!(expected.encode().unwrap(), bits);
        for (header, group) in [
            ("10100111", NeighborGroup::Second),
            ("11011110", NeighborGroup::Third),
            ("10100100", NeighborGroup::Fourth),
        ] {
            let wire = format!("00{header}000000000100010000111111011111");
            let bits: Vec<bool> = wire.chars().map(|bit| bit == '1').collect();
            let expected = TrafficMessage::ChannelQuality(ChannelQualityReport::Later {
                group,
                neighbor_rssi_codes: [0, 1, 2, 3, 30, 31],
            });
            assert_eq!(decode_prefix(&bits).unwrap().message, expected);
            assert_eq!(expected.encode().unwrap(), bits);
            assert!(decode_prefix(&bits[..bits.len() - 1]).is_err());
        }
        let invalid = TrafficMessage::ChannelQuality(ChannelQualityReport::First {
            current: DownlinkQuality {
                ber_code: 8,
                rssi_code: 0,
            },
            neighbor_rssi_codes: [0; 6],
        });
        assert!(invalid.encode().is_err());
    }

    use super::*;
    fn wire(value: u64, width: usize) -> Vec<bool> {
        (0..width)
            .rev()
            .map(|bit| value & (1 << bit) != 0)
            .collect()
    }
    #[test]
    fn baseline_traffic_messages_reject_the_opposite_air_direction() {
        let cases = [
            (TrafficMessage::Connect, Direction::Reverse),
            (TrafficMessage::Audit, Direction::Forward),
            (TrafficMessage::StopMeasurements, Direction::Forward),
            (
                TrafficMessage::MeasurementOrder {
                    rf_channels: vec![991],
                },
                Direction::Forward,
            ),
            (
                TrafficMessage::MeasurementOrderAck {
                    rf_channels: vec![991],
                },
                Direction::Reverse,
            ),
            (
                TrafficMessage::StatusRequest(Default::default()),
                Direction::Forward,
            ),
            (
                TrafficMessage::Status(Default::default()),
                Direction::Reverse,
            ),
            (
                TrafficMessage::ChannelQuality(ChannelQualityReport::First {
                    current: DownlinkQuality {
                        ber_code: 0,
                        rssi_code: 17,
                    },
                    neighbor_rssi_codes: [0; 6],
                }),
                Direction::Reverse,
            ),
            (
                TrafficMessage::ChannelQuality(ChannelQualityReport::Later {
                    group: NeighborGroup::Second,
                    neighbor_rssi_codes: [0; 6],
                }),
                Direction::Reverse,
            ),
            (
                TrafficMessage::ChannelQuality(ChannelQualityReport::Later {
                    group: NeighborGroup::Third,
                    neighbor_rssi_codes: [0; 6],
                }),
                Direction::Reverse,
            ),
            (
                TrafficMessage::ChannelQuality(ChannelQualityReport::Later {
                    group: NeighborGroup::Fourth,
                    neighbor_rssi_codes: [0; 6],
                }),
                Direction::Reverse,
            ),
        ];
        for (message, direction) in cases {
            let bits = message.encode().unwrap();
            let opposite = match direction {
                Direction::Forward => Direction::Reverse,
                Direction::Reverse => Direction::Forward,
            };
            assert_eq!(decode_prefix_in(&bits, direction).unwrap().message, message);
            assert!(decode_prefix_in(&bits, opposite).is_err());
            for version in [
                ProtocolVersion::Is136RevA,
                ProtocolVersion::Pv2,
                ProtocolVersion::Pv5,
            ] {
                assert_eq!(
                    decode_prefix_in_version(&bits, direction, version)
                        .unwrap()
                        .message,
                    message
                );
                assert!(decode_prefix_in_version(&bits, opposite, version).is_err());
            }
        }
        for direction in [Direction::Forward, Direction::Reverse] {
            let message = TrafficMessage::Release {
                information: Default::default(),
            };
            assert_eq!(
                decode_prefix_in(&message.encode().unwrap(), direction)
                    .unwrap()
                    .message,
                message
            );
        }
    }

    #[test]
    fn mandatory_fields_match_fixed_wire_vectors() {
        let cases = [
            (TrafficMessage::Connect, wire(0x0c1, 10)),
            (TrafficMessage::Audit, wire(0x061, 10)),
            (
                TrafficMessage::AlertWithInfo {
                    information: Default::default(),
                },
                wire(0x0c40, 16),
            ),
            (
                TrafficMessage::Release {
                    information: Default::default(),
                },
                wire(0x3680, 16),
            ),
            (
                TrafficMessage::Ack {
                    ack_message_type: 0xc1,
                    information: Default::default(),
                },
                wire(0x153040, 24),
            ),
        ];
        for (message, bits) in cases {
            assert_eq!(message.encode().unwrap(), bits);
            let direction = if message == TrafficMessage::Connect {
                Direction::Reverse
            } else {
                Direction::Forward
            };
            let decoded = decode_prefix_in(&bits, direction).unwrap();
            assert_eq!(decoded.message, message);
            assert_eq!(decoded.consumed_bits, bits.len());
            for length in 0..bits.len() {
                assert!(matches!(
                    decode_prefix_in(&bits[..length], direction),
                    Err(TrafficMessageError::Truncated { .. })
                ));
            }
        }
    }
    #[test]
    fn optional_length_is_bounded_and_payload_filler_bytes_are_preserved() {
        let message = TrafficMessage::Release {
            information: ReleaseInformation {
                unparsed_bits: ([0x0e; MAX_OPTIONAL_OCTETS])
                    .iter()
                    .flat_map(|byte| {
                        (0..u8::BITS)
                            .rev()
                            .map(move |shift| byte & (1 << shift) != 0)
                    })
                    .collect(),
                ..Default::default()
            },
        };
        let bits = message.encode().unwrap();
        assert_eq!(decode_prefix(&bits).unwrap().message, message);
        let too_long = TrafficMessage::Release {
            information: ReleaseInformation {
                unparsed_bits: ([0; MAX_OPTIONAL_OCTETS + 1])
                    .iter()
                    .flat_map(|byte| {
                        (0..u8::BITS)
                            .rev()
                            .map(move |shift| byte & (1 << shift) != 0)
                    })
                    .collect(),
                ..Default::default()
            },
        };
        assert_eq!(too_long.encode(), Err(TrafficMessageError::OptionalLength));
        assert_eq!(
            decode_prefix(&wire(0x0ff, 10)),
            Err(TrafficMessageError::UnsupportedType(0xff))
        );
        assert_eq!(
            decode_prefix(&wire(0x1c1, 10)),
            Err(TrafficMessageError::Protocol)
        );
    }
}
