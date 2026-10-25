use super::{OCTET_BITS, PARAMETER_BITS, Reader, TrafficMessageError, flash, put};
use crate::l3::relay::Direction;
const FIRST_PARAMETER: u64 = 1;
const ACK_CONTROL_CHANNELS: u64 = 2;
const COUNT_BITS: usize = 6;
const SINGLE_VALUE: usize = 1;
const NIBBLE_BITS: usize = 4;
const HYPERBAND_BITS: usize = 2;
const CHANNEL_BITS: usize = 11;
const MAX_CONTROL_CHANNELS: usize = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Hyperband {
    Cellular800 = 0,
    Pcs1900 = 1,
    Cellular700 = 2,
    Imt2000 = 3,
}
impl TryFrom<u8> for Hyperband {
    type Error = TrafficMessageError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Cellular800),
            1 => Ok(Self::Pcs1900),
            2 => Ok(Self::Cellular700),
            3 => Ok(Self::Imt2000),
            _ => Err(TrafficMessageError::Field("hyperband")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlChannel {
    pub hyperband: Hyperband,
    pub channel: u16,
    pub dvcc: u8,
}
impl ControlChannel {
    pub fn validate(self) -> Result<(), TrafficMessageError> {
        if u64::from(self.channel) >= 1 << CHANNEL_BITS {
            return Err(TrafficMessageError::Field("control_channel"));
        }
        Ok(())
    }

    /// IS-136.2-A §1.2.6 DL for the forward DTC CDL field: RF channels 8*DL+1 to 8*DL+8 on
    /// cellular and 16*DL+1 to 16*DL+16 on PCS. DL zero is reserved and is returned when no
    /// seven-bit DL fits or the hyperband has no mapping.
    pub fn locator(self) -> u8 {
        let channels_per_locator = match self.hyperband {
            Hyperband::Cellular800 => CELLULAR_CHANNELS_PER_LOCATOR,
            Hyperband::Pcs1900 => PCS_CHANNELS_PER_LOCATOR,
            Hyperband::Cellular700 | Hyperband::Imt2000 => return 0,
        };
        let Some(offset) = self.channel.checked_sub(1) else {
            return 0;
        };
        u8::try_from(offset / channels_per_locator)
            .ok()
            .filter(|locator| *locator <= MAX_LOCATOR)
            .unwrap_or(0)
    }
}
const CELLULAR_CHANNELS_PER_LOCATOR: u16 = 8;
const PCS_CHANNELS_PER_LOCATOR: u16 = 16;
const MAX_LOCATOR: u8 = 0x7f;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ReleaseReason {
    Normal = 0,
    PowerDown = 15,
}
impl TryFrom<u8> for ReleaseReason {
    type Error = TrafficMessageError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Normal),
            15 => Ok(Self::PowerDown),
            _ => Err(TrafficMessageError::Field("release_reason")),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ReleaseFields {
    #[default]
    None,
    ControlChannels(Vec<ControlChannel>),
    Reason(ReleaseReason),
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseInformation {
    pub fields: ReleaseFields,
    pub unparsed_bits: Vec<bool>,
}
impl ReleaseInformation {
    pub const MESSAGE_TYPE: u8 = 0xda;
    pub fn validate_in(&self, direction: Direction) -> Result<(), TrafficMessageError> {
        if matches!(
            (&self.fields, direction),
            (ReleaseFields::Reason(_), Direction::Forward)
                | (ReleaseFields::ControlChannels(_), Direction::Reverse)
        ) {
            return Err(TrafficMessageError::Field("release_direction"));
        }
        self.encode_octets().map(|_| ())
    }
    pub(super) fn encode_octets(&self) -> Result<Vec<u8>, TrafficMessageError> {
        let mut bits = Vec::new();
        match &self.fields {
            ReleaseFields::None => {}
            ReleaseFields::Reason(reason) => nibble(&mut bits, FIRST_PARAMETER, *reason as u8)?,
            ReleaseFields::ControlChannels(channels) => {
                write_channels(&mut bits, FIRST_PARAMETER, channels)?
            }
        }
        bits.extend(&self.unparsed_bits);
        octets(bits)
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcknowledgmentInformation {
    pub last_decoded_parameter: Option<u8>,
    pub control_channels: Vec<ControlChannel>,
    pub unparsed_bits: Vec<bool>,
}
impl AcknowledgmentInformation {
    pub fn validate_in(&self, direction: Direction) -> Result<(), TrafficMessageError> {
        if direction == Direction::Reverse && !self.control_channels.is_empty() {
            return Err(TrafficMessageError::Field("ack_direction"));
        }
        self.encode_octets().map(|_| ())
    }
    pub(super) fn encode_octets(&self) -> Result<Vec<u8>, TrafficMessageError> {
        let mut bits = Vec::new();
        if let Some(value) = self.last_decoded_parameter {
            nibble(&mut bits, FIRST_PARAMETER, value)?;
        }
        if !self.control_channels.is_empty() {
            write_channels(&mut bits, ACK_CONTROL_CHANNELS, &self.control_channels)?;
        }
        bits.extend(&self.unparsed_bits);
        octets(bits)
    }
}
fn nibble(bits: &mut Vec<bool>, parameter: u64, value: u8) -> Result<(), TrafficMessageError> {
    if u64::from(value) >= 1 << NIBBLE_BITS {
        return Err(TrafficMessageError::Field("last_decoded_parameter"));
    }
    put(bits, parameter, PARAMETER_BITS);
    put(bits, SINGLE_VALUE as u64, COUNT_BITS);
    put(bits, u64::from(value), NIBBLE_BITS);
    Ok(())
}
// TIA-136-133-E §§5.1.2.3.36,5.1.2.4.38 use one to three Hyperband2/Channel11/DVCC8 values.
fn write_channels(
    bits: &mut Vec<bool>,
    parameter: u64,
    channels: &[ControlChannel],
) -> Result<(), TrafficMessageError> {
    if channels.is_empty() || channels.len() > MAX_CONTROL_CHANNELS {
        return Err(TrafficMessageError::Field("control_channel_count"));
    }
    put(bits, parameter, PARAMETER_BITS);
    put(bits, channels.len() as u64, COUNT_BITS);
    for channel in channels {
        channel.validate()?;
        put(bits, channel.hyperband as u64, HYPERBAND_BITS);
        put(bits, u64::from(channel.channel), CHANNEL_BITS);
        put(bits, u64::from(channel.dvcc), OCTET_BITS);
    }
    Ok(())
}
fn read_channels(
    reader: &mut Reader<'_>,
    count: usize,
) -> Result<Vec<ControlChannel>, TrafficMessageError> {
    if !(SINGLE_VALUE..=MAX_CONTROL_CHANNELS).contains(&count) {
        return Err(TrafficMessageError::Field("control_channel_count"));
    }
    (0..count)
        .map(|_| {
            Ok(ControlChannel {
                hyperband: Hyperband::try_from(reader.take(HYPERBAND_BITS)? as u8)?,
                channel: reader.take(CHANNEL_BITS)? as u16,
                dvcc: reader.take(OCTET_BITS)? as u8,
            })
        })
        .collect()
}
fn read_nibble(reader: &mut Reader<'_>, count: usize) -> Result<u8, TrafficMessageError> {
    if count != SINGLE_VALUE {
        return Err(TrafficMessageError::Field("single_value_count"));
    }
    reader.take(NIBBLE_BITS).map(|value| value as u8)
}
fn bits(bytes: &[u8]) -> Vec<bool> {
    bytes
        .iter()
        .flat_map(|byte| {
            (0..OCTET_BITS)
                .rev()
                .map(move |shift| byte & (1 << shift) != 0)
        })
        .collect()
}
fn octets(mut bits: Vec<bool>) -> Result<Vec<u8>, TrafficMessageError> {
    flash::pad(&mut bits)?;
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
fn padding(reader: &Reader<'_>) -> Result<(), TrafficMessageError> {
    if reader.bits[reader.offset..].iter().any(|bit| *bit) {
        return Err(TrafficMessageError::Field("termination_padding"));
    }
    Ok(())
}
pub(super) fn decode_release(
    bytes: &[u8],
    direction: Direction,
) -> Result<ReleaseInformation, TrafficMessageError> {
    let bits = bits(bytes);
    let mut reader = Reader {
        bits: &bits,
        offset: 0,
    };
    let mut result = ReleaseInformation::default();
    let mut seen = false;
    while bits.len() - reader.offset >= OCTET_BITS {
        let start = reader.offset;
        let parameter = reader.take(PARAMETER_BITS)?;
        if parameter != FIRST_PARAMETER {
            result.unparsed_bits = bits[start..].to_vec();
            reader.offset = bits.len();
            break;
        }
        if seen {
            return Err(TrafficMessageError::Field("duplicate_release_parameter"));
        }
        seen = true;
        let count = reader.take(COUNT_BITS)? as usize;
        result.fields = match direction {
            Direction::Forward => {
                ReleaseFields::ControlChannels(read_channels(&mut reader, count)?)
            }
            Direction::Reverse => {
                ReleaseFields::Reason(ReleaseReason::try_from(read_nibble(&mut reader, count)?)?)
            }
        };
    }
    padding(&reader)?;
    Ok(result)
}
pub(super) fn decode_ack(
    bytes: &[u8],
    direction: Direction,
) -> Result<AcknowledgmentInformation, TrafficMessageError> {
    let bits = bits(bytes);
    let mut reader = Reader {
        bits: &bits,
        offset: 0,
    };
    let mut result = AcknowledgmentInformation::default();
    let mut previous = 0;
    while bits.len() - reader.offset >= OCTET_BITS {
        let start = reader.offset;
        let parameter = reader.take(PARAMETER_BITS)?;
        if parameter != FIRST_PARAMETER
            && !(parameter == ACK_CONTROL_CHANNELS && direction == Direction::Forward)
        {
            result.unparsed_bits = bits[start..].to_vec();
            reader.offset = bits.len();
            break;
        }
        if parameter <= previous {
            return Err(TrafficMessageError::Field("ack_parameter_order"));
        }
        previous = parameter;
        let count = reader.take(COUNT_BITS)? as usize;
        if parameter == FIRST_PARAMETER {
            result.last_decoded_parameter = Some(read_nibble(&mut reader, count)?);
        } else {
            result.control_channels = read_channels(&mut reader, count)?;
        }
    }
    padding(&reader)?;
    Ok(result)
}
#[cfg(test)]
mod tests;

#[cfg(test)]
mod locator_tests {
    use super::*;

    fn locator(hyperband: Hyperband, channel: u16) -> u8 {
        ControlChannel {
            hyperband,
            channel,
            dvcc: 1,
        }
        .locator()
    }

    #[test]
    fn locator_follows_the_dl_channel_ranges() {
        assert_eq!(locator(Hyperband::Cellular800, 8), 0);
        assert_eq!(locator(Hyperband::Cellular800, 9), 1);
        assert_eq!(locator(Hyperband::Cellular800, 333), 41);
        assert_eq!(locator(Hyperband::Cellular800, 1023), 127);
        assert_eq!(locator(Hyperband::Pcs1900, 17), 1);
        assert_eq!(locator(Hyperband::Pcs1900, 1999), 124);
        assert_eq!(locator(Hyperband::Cellular800, 0), 0);
    }
}
