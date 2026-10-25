use super::{Direction, LENGTH_BITS, OCTET_BITS, PARAMETER_BITS, Reader, TrafficMessageError, put};

const BURST: u8 = 0x92;
const CONTINUOUS: u8 = 0x67;
const BURST_ACK: u8 = 0x37;
const CONTINUOUS_ACK: u8 = 0x57;
const REQUEST_BITS: usize = 4;
const DIGIT_BITS: usize = 4;
const COUNT_BITS: usize = 6;
const PARAMETER: u64 = 1;
pub const MAX_DIGITS: usize = (1 << COUNT_BITS) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DtmfDigit {
    Null = 0,
    One,
    Two,
    Three,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Zero,
    Star,
    Hash,
}
impl DtmfDigit {
    // TIA-136-133-E §5.1.2.4.22 reserves codes 13–15, including analog mode's plus code.
    pub fn decode(code: u8) -> Result<Self, TrafficMessageError> {
        match code {
            0 => Ok(Self::Null),
            1 => Ok(Self::One),
            2 => Ok(Self::Two),
            3 => Ok(Self::Three),
            4 => Ok(Self::Four),
            5 => Ok(Self::Five),
            6 => Ok(Self::Six),
            7 => Ok(Self::Seven),
            8 => Ok(Self::Eight),
            9 => Ok(Self::Nine),
            10 => Ok(Self::Zero),
            11 => Ok(Self::Star),
            12 => Ok(Self::Hash),
            _ => Err(TrafficMessageError::Field("dtmf_digit")),
        }
    }
    pub fn symbol(self) -> char {
        match self {
            Self::Null => '∅',
            Self::One => '1',
            Self::Two => '2',
            Self::Three => '3',
            Self::Four => '4',
            Self::Five => '5',
            Self::Six => '6',
            Self::Seven => '7',
            Self::Eight => '8',
            Self::Nine => '9',
            Self::Zero => '0',
            Self::Star => '*',
            Self::Hash => '#',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DtmfMessage {
    Burst {
        request_number: u8,
        digits: Option<Vec<DtmfDigit>>,
    },
    Continuous {
        request_number: u8,
        digit: DtmfDigit,
    },
    BurstAck {
        request_number: u8,
        last_decoded_parameter: Option<u8>,
    },
    ContinuousAck {
        request_number: u8,
    },
}
impl DtmfMessage {
    pub fn message_type(&self) -> u8 {
        match self {
            Self::Burst { .. } => BURST,
            Self::Continuous { .. } => CONTINUOUS,
            Self::BurstAck { .. } => BURST_ACK,
            Self::ContinuousAck { .. } => CONTINUOUS_ACK,
        }
    }
    pub fn request_number(&self) -> u8 {
        match self {
            Self::Burst { request_number, .. }
            | Self::Continuous { request_number, .. }
            | Self::BurstAck { request_number, .. }
            | Self::ContinuousAck { request_number } => *request_number,
        }
    }
    pub fn acknowledgment(&self) -> Option<Self> {
        match self {
            Self::Burst {
                request_number,
                digits,
            } => Some(Self::BurstAck {
                request_number: *request_number,
                last_decoded_parameter: digits.as_ref().map(|_| PARAMETER as u8),
            }),
            Self::Continuous { request_number, .. } => Some(Self::ContinuousAck {
                request_number: *request_number,
            }),
            _ => None,
        }
    }
    // TIA-136-133-E §§5.1.2.2.11–12 and 9.1.2.2.13–14.
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        if self.request_number() >= 1 << REQUEST_BITS {
            return Err(TrafficMessageError::Field("dtmf_request_number"));
        }
        put(bits, u64::from(self.request_number()), REQUEST_BITS);
        let mut optional = Vec::new();
        match self {
            Self::Continuous { digit, .. } => {
                put(bits, *digit as u64, DIGIT_BITS);
                return Ok(());
            }
            Self::ContinuousAck { .. } => return Ok(()),
            Self::Burst {
                digits: Some(digits),
                ..
            } => {
                if digits.len() > MAX_DIGITS {
                    return Err(TrafficMessageError::Field("dtmf_digit_count"));
                }
                put(&mut optional, PARAMETER, PARAMETER_BITS);
                put(&mut optional, digits.len() as u64, COUNT_BITS);
                for digit in digits {
                    put(&mut optional, *digit as u64, DIGIT_BITS);
                }
            }
            Self::BurstAck {
                last_decoded_parameter: Some(parameter),
                ..
            } => {
                if *parameter >= 1 << PARAMETER_BITS {
                    return Err(TrafficMessageError::Field("last_decoded_parameter"));
                }
                put(&mut optional, PARAMETER, PARAMETER_BITS);
                put(&mut optional, 1, COUNT_BITS);
                put(&mut optional, u64::from(*parameter), PARAMETER_BITS);
            }
            _ => {}
        }
        optional.resize(optional.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
        put(bits, (optional.len() / OCTET_BITS) as u64, LENGTH_BITS);
        bits.extend(optional);
        Ok(())
    }
}
pub(super) fn decode(
    kind: u8,
    direction: Direction,
    reader: &mut Reader<'_>,
) -> Result<Option<DtmfMessage>, TrafficMessageError> {
    if !matches!(kind, BURST | CONTINUOUS | BURST_ACK | CONTINUOUS_ACK) {
        return Ok(None);
    }
    if matches!(kind, BURST | CONTINUOUS) != (direction == Direction::Reverse) {
        return Err(TrafficMessageError::Field("dtmf_direction"));
    }
    let request_number = reader.take(REQUEST_BITS)? as u8;
    match kind {
        CONTINUOUS => {
            return Ok(Some(DtmfMessage::Continuous {
                request_number,
                digit: DtmfDigit::decode(reader.take(DIGIT_BITS)? as u8)?,
            }));
        }
        CONTINUOUS_ACK => return Ok(Some(DtmfMessage::ContinuousAck { request_number })),
        _ => {}
    }
    let length = reader.take(LENGTH_BITS)? as usize * OCTET_BITS;
    let end = reader.offset + length;
    if reader.bits.len() < end {
        return Err(TrafficMessageError::Truncated { needed_bits: end });
    }
    let mut optional = Reader {
        bits: &reader.bits[reader.offset..end],
        offset: 0,
    };
    reader.offset = end;
    let mut digits = None;
    let mut last_decoded_parameter = None;
    if length != 0 {
        let parsed = (|| {
            if optional.take(PARAMETER_BITS)? != PARAMETER {
                return Err(TrafficMessageError::Field("dtmf_parameter"));
            }
            let count = optional.take(COUNT_BITS)? as usize;
            if kind == BURST {
                digits = Some(
                    (0..count)
                        .map(|_| DtmfDigit::decode(optional.take(DIGIT_BITS)? as u8))
                        .collect::<Result<Vec<_>, _>>()?,
                );
            } else {
                if count != 1 {
                    return Err(TrafficMessageError::Field("last_decoded_parameter_count"));
                }
                last_decoded_parameter = Some(optional.take(PARAMETER_BITS)? as u8);
            }
            if optional.bits.len() - optional.offset >= OCTET_BITS
                || optional.bits[optional.offset..].iter().any(|bit| *bit)
            {
                return Err(TrafficMessageError::Field("dtmf_padding"));
            }
            Ok(())
        })();
        parsed.map_err(|error| match error {
            TrafficMessageError::Truncated { .. } => {
                TrafficMessageError::Field("dtmf_optional_length")
            }
            other => other,
        })?;
    }
    Ok(Some(if kind == BURST {
        DtmfMessage::Burst {
            request_number,
            digits,
        }
    } else {
        DtmfMessage::BurstAck {
            request_number,
            last_decoded_parameter,
        }
    }))
}

#[cfg(test)]
mod tests;
