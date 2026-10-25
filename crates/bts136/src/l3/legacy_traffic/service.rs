use super::{
    Direction, LENGTH_BITS, MAX_OPTIONAL_OCTETS, OCTET_BITS, PARAMETER_BITS, Reader,
    TrafficMessageError, put,
};
const REQUEST: u8 = 0xf3;
const RESPONSE: u8 = 0xa3;
const REQUEST_BITS: usize = 4;
const SERVICE_BITS: usize = 4;
const BANDWIDTH: u64 = 1;
const VOICE_MODE: u64 = 2;
const DATA_MODE: u64 = 3;
const EXTENDED_VOICE: u64 = 4;
const INTERLEAVING: u64 = 5;
const CAUSE: u64 = 1;
const REJECTED: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ServiceRejection {
    Unknown = 3,
    UnsupportedService = 7,
    TryFullRate = 18,
    UnsupportedPrivacy = 20,
    UnsupportedCodec = 25,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceOptions {
    pub bandwidth: Option<Vec<u8>>,
    pub voice_mode: Option<Vec<u8>>,
    pub data_mode: Option<Vec<u8>>,
    pub extended_voice_mode: Option<Vec<u8>>,
    pub interleaving: Option<Vec<u8>>,
    pub unparsed_bits: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceMessage {
    Request {
        request_number: u8,
        service_code: u8,
        options: ServiceOptions,
    },
    Response {
        request_number: u8,
        service_code: u8,
        causes: Option<Vec<u8>>,
        unparsed_bits: Vec<bool>,
    },
}
impl ServiceMessage {
    pub const REQUEST_TYPE: u8 = REQUEST;
    pub const RESPONSE_TYPE: u8 = RESPONSE;
    pub fn message_type(&self) -> u8 {
        match self {
            Self::Request { .. } => REQUEST,
            Self::Response { .. } => RESPONSE,
        }
    }
    pub fn request_number(&self) -> u8 {
        match self {
            Self::Request { request_number, .. } | Self::Response { request_number, .. } => {
                *request_number
            }
        }
    }
    // TIA-136-133-E §§5.1.2.2.27,9.1.2.2.30 put Service Code after Remaining Length.
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        checked(bits, self.request_number(), REQUEST_BITS)?;
        let mut body = Vec::new();
        match self {
            Self::Request {
                service_code,
                options,
                ..
            } => {
                checked(&mut body, *service_code, SERVICE_BITS)?;
                if options.voice_mode.is_some() && options.extended_voice_mode.is_some() {
                    return Err(TrafficMessageError::Field("service_voice_modes"));
                }
                for (parameter, values, width, maximum) in [
                    (BANDWIDTH, &options.bandwidth, 3, 1),
                    (VOICE_MODE, &options.voice_mode, 6, 1),
                    (DATA_MODE, &options.data_mode, OCTET_BITS, 2),
                    (EXTENDED_VOICE, &options.extended_voice_mode, OCTET_BITS, 1),
                    (INTERLEAVING, &options.interleaving, 3, 1),
                ] {
                    if let Some(values) = values {
                        counted(&mut body, parameter, values, width, maximum)?;
                    }
                }
                body.extend(&options.unparsed_bits);
            }
            Self::Response {
                service_code,
                causes,
                unparsed_bits,
                ..
            } => {
                checked(&mut body, *service_code, SERVICE_BITS)?;
                if *service_code != REJECTED && causes.is_some() {
                    return Err(TrafficMessageError::Field("service_cause"));
                }
                if let Some(causes) = causes {
                    counted(&mut body, CAUSE, causes, OCTET_BITS, 10)?;
                }
                body.extend(unparsed_bits);
            }
        }
        body.resize(body.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
        if body.len() / OCTET_BITS > MAX_OPTIONAL_OCTETS {
            return Err(TrafficMessageError::OptionalLength);
        }
        put(bits, (body.len() / OCTET_BITS) as u64, LENGTH_BITS);
        bits.extend(body);
        Ok(())
    }
}
fn checked(bits: &mut Vec<bool>, value: u8, width: usize) -> Result<(), TrafficMessageError> {
    if u64::from(value) >= 1 << width {
        return Err(TrafficMessageError::Field("service_field"));
    }
    put(bits, u64::from(value), width);
    Ok(())
}
fn counted(
    bits: &mut Vec<bool>,
    parameter: u64,
    values: &[u8],
    width: usize,
    maximum: usize,
) -> Result<(), TrafficMessageError> {
    if values.len() > maximum {
        return Err(TrafficMessageError::Field("service_value_count"));
    }
    put(bits, parameter, PARAMETER_BITS);
    put(bits, values.len() as u64, LENGTH_BITS);
    for value in values {
        checked(bits, *value, width)?;
    }
    Ok(())
}
pub(super) fn decode(
    kind: u8,
    direction: Direction,
    reader: &mut Reader<'_>,
) -> Result<Option<ServiceMessage>, TrafficMessageError> {
    if !matches!(kind, REQUEST | RESPONSE) {
        return Ok(None);
    }
    if (kind == REQUEST) != (direction == Direction::Reverse) {
        return Err(TrafficMessageError::Field("service_direction"));
    }
    let request_number = reader.take(REQUEST_BITS)? as u8;
    let length = reader.take(LENGTH_BITS)? as usize * OCTET_BITS;
    let end = reader.offset + length;
    if end > reader.bits.len() {
        return Err(TrafficMessageError::Truncated { needed_bits: end });
    }
    if length < SERVICE_BITS {
        return Err(TrafficMessageError::Field("service_length"));
    }
    let mut fields = Reader {
        bits: &reader.bits[reader.offset..end],
        offset: 0,
    };
    reader.offset = end;
    let parsed = (|| {
        let service_code = fields.take(SERVICE_BITS)? as u8;
        let mut options = ServiceOptions::default();
        let mut causes = None;
        let mut previous = 0;
        while fields.bits.len() - fields.offset >= OCTET_BITS {
            let start = fields.offset;
            let parameter = fields.take(PARAMETER_BITS)?;
            if (kind == REQUEST && !(BANDWIDTH..=INTERLEAVING).contains(&parameter))
                || (kind == RESPONSE && parameter != CAUSE)
            {
                options.unparsed_bits = fields.bits[start..].to_vec();
                fields.offset = fields.bits.len();
                break;
            }
            if parameter <= previous {
                return Err(TrafficMessageError::Field("service_parameter_order"));
            }
            previous = parameter;
            let count = fields.take(LENGTH_BITS)? as usize;
            let (target, width, maximum) = if kind == RESPONSE {
                (&mut causes, OCTET_BITS, 10)
            } else {
                match parameter {
                    BANDWIDTH => (&mut options.bandwidth, 3, 1),
                    VOICE_MODE => (&mut options.voice_mode, 6, 1),
                    DATA_MODE => (&mut options.data_mode, OCTET_BITS, 2),
                    EXTENDED_VOICE => (&mut options.extended_voice_mode, OCTET_BITS, 1),
                    INTERLEAVING => (&mut options.interleaving, 3, 1),
                    _ => unreachable!(),
                }
            };
            if count > maximum {
                return Err(TrafficMessageError::Field("service_value_count"));
            }
            *target = Some(
                (0..count)
                    .map(|_| fields.take(width).map(|value| value as u8))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        if fields.bits[fields.offset..].iter().any(|bit| *bit) {
            return Err(TrafficMessageError::Field("service_padding"));
        }
        if kind == REQUEST {
            if options.voice_mode.is_some() && options.extended_voice_mode.is_some() {
                return Err(TrafficMessageError::Field("service_voice_modes"));
            }
            Ok(ServiceMessage::Request {
                request_number,
                service_code,
                options,
            })
        } else {
            if service_code != REJECTED && causes.is_some() {
                return Err(TrafficMessageError::Field("service_cause"));
            }
            Ok(ServiceMessage::Response {
                request_number,
                service_code,
                causes,
                unparsed_bits: options.unparsed_bits,
            })
        }
    })();
    parsed.map(Some).map_err(|error| match error {
        TrafficMessageError::Truncated { .. } => {
            TrafficMessageError::Field("service_optional_length")
        }
        error => error,
    })
}
#[cfg(test)]
mod tests;
