pub use super::control_fields::Error;
use super::{
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_subaddress, write_subaddress,
    },
    legacy_traffic::{Hyperband, Subaddress},
};
const TYPE_BITS: usize = 6;
const DIRECTED_RETRY: u8 = 22;
const PARAMETER_UPDATE: u8 = 25;
const REQUEST_BITS: usize = 4;
const HYPERBAND_BITS: usize = 2;
const CHANNEL_BITS: usize = 11;
const DTX_BITS: usize = 2;
const MAX_RETRY_CHANNELS: usize = 6;
const RCF_AUTH: u8 = 1;
const DTX: u8 = 2;
const RETRY_CHANNEL: u8 = 3;
const RETRY_SUBADDRESS: u8 = 4;
const UPDATE_SUBADDRESS: u8 = 1;
const EXTENSION: u8 = 7;
const RESERVED: u8 = 0;
const FUTURE: u8 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryChannel {
    pub hyperband: Hyperband,
    pub channel: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RcfAuth {
    pub read_control_filler: bool,
    pub authentication: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AnalogDtx {
    Unsupported = 0,
    Attenuation8Db = 2,
    UnlimitedAttenuation = 3,
}
impl TryFrom<u8> for AnalogDtx {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Unsupported),
            2 => Ok(Self::Attenuation8Db),
            3 => Ok(Self::UnlimitedAttenuation),
            _ => Err(Error::Field("analog_dtx")),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAccess {
    Analog,
    Digital,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectedRetry {
    pub last_try: bool,
    pub rcf_auth: Option<RcfAuth>,
    pub dtx: Option<AnalogDtx>,
    pub channels: Vec<RetryChannel>,
    pub subaddress: Option<Subaddress>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
impl DirectedRetry {
    // TIA-136-123-E §5.3.7 requires both analog access IEs only for retry to an ACC.
    pub fn validate_access(&self, access: RetryAccess) -> Result<(), Error> {
        let valid = match access {
            RetryAccess::Analog => self.rcf_auth.is_some() && self.dtx.is_some(),
            RetryAccess::Digital => self.rcf_auth.is_none() && self.dtx.is_none(),
        };
        if !valid {
            return Err(Error::Field("retry_access"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParameterUpdate {
    pub request_number: u8,
    pub subaddress: Option<Subaddress>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlManagement {
    DirectedRetry(DirectedRetry),
    ParameterUpdate(ParameterUpdate),
}
impl ControlManagement {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bits = Vec::new();
        let (subaddress, code, ignored, unparsed) = match self {
            Self::DirectedRetry(message) => {
                if message.channels.len() > MAX_RETRY_CHANNELS {
                    return Err(Error::Field("retry_channel_count"));
                }
                put(&mut bits, u64::from(DIRECTED_RETRY), OCTET_BITS)?;
                put(&mut bits, u64::from(message.last_try), 1)?;
                if let Some(flags) = message.rcf_auth {
                    put(&mut bits, u64::from(RCF_AUTH), PARAMETER_BITS)?;
                    put(&mut bits, u64::from(flags.read_control_filler), 1)?;
                    put(&mut bits, u64::from(flags.authentication), 1)?;
                }
                if let Some(dtx) = message.dtx {
                    put(&mut bits, u64::from(DTX), PARAMETER_BITS)?;
                    put(&mut bits, dtx as u64, DTX_BITS)?;
                }
                for channel in &message.channels {
                    put(&mut bits, u64::from(RETRY_CHANNEL), PARAMETER_BITS)?;
                    put(&mut bits, channel.hyperband as u64, HYPERBAND_BITS)?;
                    put(&mut bits, u64::from(channel.channel), CHANNEL_BITS)?;
                }
                (
                    &message.subaddress,
                    RETRY_SUBADDRESS,
                    &message.ignored_bits,
                    &message.unparsed_bits,
                )
            }
            Self::ParameterUpdate(message) => {
                put(&mut bits, u64::from(PARAMETER_UPDATE), OCTET_BITS)?;
                put(&mut bits, u64::from(message.request_number), REQUEST_BITS)?;
                (
                    &message.subaddress,
                    UPDATE_SUBADDRESS,
                    &message.ignored_bits,
                    &message.unparsed_bits,
                )
            }
        };
        if let Some(address) = subaddress {
            write_subaddress(&mut bits, code, address)?;
        }
        bits.extend(ignored);
        bits.extend(unparsed);
        pack(bits)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let bits = control_fields::bits(bytes)?;
        let mut reader = Reader {
            bits: &bits,
            offset: 0,
        };
        let header = reader.take(OCTET_BITS)? as u8;
        if header >> TYPE_BITS != 0 {
            return Err(Error::Protocol);
        }
        let mut message = match header {
            DIRECTED_RETRY => Self::DirectedRetry(DirectedRetry {
                last_try: reader.flag()?,
                ..Default::default()
            }),
            PARAMETER_UPDATE => Self::ParameterUpdate(ParameterUpdate {
                request_number: reader.take(REQUEST_BITS)? as u8,
                ..Default::default()
            }),
            _ => return Err(Error::MessageType),
        };
        let mut previous = 0;
        loop {
            let remaining = &bits[reader.offset..];
            if remaining.is_empty()
                || remaining.len() < OCTET_BITS && remaining.iter().all(|bit| !*bit)
            {
                break;
            }
            let start = reader.offset;
            let code = reader.take(PARAMETER_BITS)? as u8;
            if matches!(code, RESERVED | FUTURE) {
                return Err(Error::Field("optional_reserved"));
            }
            if code == EXTENSION {
                let auxiliary = reader.take(PARAMETER_BITS)? as u8;
                if matches!(auxiliary, RESERVED | FUTURE) {
                    return Err(Error::Field("auxiliary_reserved"));
                }
            }
            let known = match &message {
                Self::DirectedRetry(_) => (RCF_AUTH..=RETRY_SUBADDRESS).contains(&code),
                Self::ParameterUpdate(_) => code == UPDATE_SUBADDRESS,
            };
            if !known {
                let (ignored, unparsed) = match &mut message {
                    Self::DirectedRetry(message) => {
                        (&mut message.ignored_bits, &mut message.unparsed_bits)
                    }
                    Self::ParameterUpdate(message) => {
                        (&mut message.ignored_bits, &mut message.unparsed_bits)
                    }
                };
                if reader.skip_noncritical(code)? {
                    ignored.extend_from_slice(&bits[start..reader.offset]);
                    continue;
                }
                *unparsed = bits[start..].to_vec();
                break;
            }
            let repeated_channel =
                matches!(&message, Self::DirectedRetry(_)) && code == RETRY_CHANNEL;
            if code < previous || code == previous && !repeated_channel {
                return Err(Error::Field("parameter_order"));
            }
            previous = code;
            match &mut message {
                Self::DirectedRetry(message) => match code {
                    RCF_AUTH => {
                        message.rcf_auth = Some(RcfAuth {
                            read_control_filler: reader.flag()?,
                            authentication: reader.flag()?,
                        })
                    }
                    DTX => message.dtx = Some(AnalogDtx::try_from(reader.take(DTX_BITS)? as u8)?),
                    RETRY_CHANNEL => {
                        if message.channels.len() == MAX_RETRY_CHANNELS {
                            return Err(Error::Field("retry_channel_count"));
                        }
                        let hyperband = Hyperband::try_from(reader.take(HYPERBAND_BITS)? as u8)
                            .map_err(|_| Error::Field("hyperband"))?;
                        message.channels.push(RetryChannel {
                            hyperband,
                            channel: reader.take(CHANNEL_BITS)? as u16,
                        });
                    }
                    RETRY_SUBADDRESS => message.subaddress = Some(read_subaddress(&mut reader)?),
                    _ => return Err(Error::Field("parameter")),
                },
                Self::ParameterUpdate(message) => {
                    message.subaddress = Some(read_subaddress(&mut reader)?)
                }
            }
        }
        Ok(message)
    }
}
#[cfg(test)]
mod tests;
