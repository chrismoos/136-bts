pub use super::control_fields::Error;
use super::{
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_subaddress, write_subaddress,
    },
    legacy_traffic::Subaddress,
    relay::Direction,
};
const TYPE_BITS: usize = 6;
const AUTHENTICATION: u8 = 2;
const BASE_CHALLENGE: u8 = 3;
const BASE_CONFIRMATION: u8 = 19;
const SSD_ORDER: u8 = 34;
const SSD_CONFIRMATION: u8 = 16;
const UNIQUE_ORDER: u8 = 36;
const UNIQUE_CONFIRMATION: u8 = 18;
const COUNT_BITS: usize = 6;
const RANDC_BITS: usize = 8;
const AUTH_BITS: usize = 18;
const RANDBS_BITS: usize = 32;
const RANDSSD_BITS: usize = 56;
const RANDU_BITS: usize = 24;
const SSD_STATUS_BITS: usize = 2;
const SUBADDRESS: u8 = 1;
const RESERVED: u8 = 0;
const EXTENSION: u8 = 7;
const FUTURE: u8 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SsdUpdateStatus {
    Successful = 0,
    AuthbsMismatch = 1,
}
impl TryFrom<u8> for SsdUpdateStatus {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Successful),
            1 => Ok(Self::AuthbsMismatch),
            _ => Err(Error::Field("ssd_update_status")),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityBody {
    Authentication { count: u8, randc: u8, authr: u32 },
    BaseStationChallenge { randbs: u32 },
    BaseStationConfirmation { authbs: u32 },
    SsdUpdateOrder { randssd: u64 },
    SsdUpdateConfirmation { status: SsdUpdateStatus },
    UniqueChallengeOrder { randu: u32 },
    UniqueChallengeConfirmation { authu: u32 },
}
impl SecurityBody {
    fn header(&self) -> u8 {
        match self {
            Self::Authentication { .. } => AUTHENTICATION,
            Self::BaseStationChallenge { .. } => BASE_CHALLENGE,
            Self::BaseStationConfirmation { .. } => BASE_CONFIRMATION,
            Self::SsdUpdateOrder { .. } => SSD_ORDER,
            Self::SsdUpdateConfirmation { .. } => SSD_CONFIRMATION,
            Self::UniqueChallengeOrder { .. } => UNIQUE_ORDER,
            Self::UniqueChallengeConfirmation { .. } => UNIQUE_CONFIRMATION,
        }
    }
    pub fn direction(&self) -> Direction {
        match self {
            Self::BaseStationConfirmation { .. }
            | Self::SsdUpdateOrder { .. }
            | Self::UniqueChallengeOrder { .. } => Direction::Forward,
            _ => Direction::Reverse,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecurityOptions {
    pub subaddress: Option<Subaddress>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityMessage {
    pub body: SecurityBody,
    pub options: SecurityOptions,
}
impl SecurityMessage {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bits = Vec::new();
        put(&mut bits, u64::from(self.body.header()), OCTET_BITS)?;
        match &self.body {
            SecurityBody::Authentication {
                count,
                randc,
                authr,
            } => {
                if self.options != SecurityOptions::default() {
                    return Err(Error::Field("authentication_options"));
                }
                put(&mut bits, u64::from(*count), COUNT_BITS)?;
                put(&mut bits, u64::from(*randc), RANDC_BITS)?;
                put(&mut bits, u64::from(*authr), AUTH_BITS)?;
            }
            SecurityBody::BaseStationChallenge { randbs } => {
                put(&mut bits, u64::from(*randbs), RANDBS_BITS)?
            }
            SecurityBody::BaseStationConfirmation { authbs } => {
                put(&mut bits, u64::from(*authbs), AUTH_BITS)?
            }
            SecurityBody::SsdUpdateOrder { randssd } => put(&mut bits, *randssd, RANDSSD_BITS)?,
            SecurityBody::SsdUpdateConfirmation { status } => {
                put(&mut bits, *status as u64, SSD_STATUS_BITS)?
            }
            SecurityBody::UniqueChallengeOrder { randu } => {
                put(&mut bits, u64::from(*randu), RANDU_BITS)?
            }
            SecurityBody::UniqueChallengeConfirmation { authu } => {
                put(&mut bits, u64::from(*authu), AUTH_BITS)?
            }
        }
        if let Some(address) = &self.options.subaddress {
            write_subaddress(&mut bits, SUBADDRESS, address)?;
        }
        bits.extend(&self.options.ignored_bits);
        bits.extend(&self.options.unparsed_bits);
        pack(bits)
    }
    pub fn decode(bytes: &[u8], direction: Direction) -> Result<Self, Error> {
        let bits = control_fields::bits(bytes)?;
        let mut reader = Reader {
            bits: &bits,
            offset: 0,
        };
        let header = reader.take(OCTET_BITS)? as u8;
        if header >> TYPE_BITS != 0 {
            return Err(Error::Protocol);
        }
        let body = match (header, direction) {
            (AUTHENTICATION, Direction::Reverse) => SecurityBody::Authentication {
                count: reader.take(COUNT_BITS)? as u8,
                randc: reader.take(RANDC_BITS)? as u8,
                authr: reader.take(AUTH_BITS)? as u32,
            },
            (BASE_CHALLENGE, Direction::Reverse) => SecurityBody::BaseStationChallenge {
                randbs: reader.take(RANDBS_BITS)? as u32,
            },
            (BASE_CONFIRMATION, Direction::Forward) => SecurityBody::BaseStationConfirmation {
                authbs: reader.take(AUTH_BITS)? as u32,
            },
            (SSD_ORDER, Direction::Forward) => SecurityBody::SsdUpdateOrder {
                randssd: reader.take(RANDSSD_BITS)?,
            },
            (SSD_CONFIRMATION, Direction::Reverse) => SecurityBody::SsdUpdateConfirmation {
                status: SsdUpdateStatus::try_from(reader.take(SSD_STATUS_BITS)? as u8)?,
            },
            (UNIQUE_ORDER, Direction::Forward) => SecurityBody::UniqueChallengeOrder {
                randu: reader.take(RANDU_BITS)? as u32,
            },
            (UNIQUE_CONFIRMATION, Direction::Reverse) => {
                SecurityBody::UniqueChallengeConfirmation {
                    authu: reader.take(AUTH_BITS)? as u32,
                }
            }
            _ => return Err(Error::MessageType),
        };
        let mut value = Self {
            body,
            options: Default::default(),
        };
        if matches!(value.body, SecurityBody::Authentication { .. }) {
            if reader.offset != bits.len() {
                return Err(Error::Length);
            }
            return Ok(value);
        }
        while reader.offset < bits.len() {
            let remaining = &bits[reader.offset..];
            if remaining.len() < OCTET_BITS && remaining.iter().all(|bit| !*bit) {
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
            if code == SUBADDRESS {
                if value.options.subaddress.is_some() {
                    return Err(Error::Field("parameter_order"));
                }
                value.options.subaddress = Some(read_subaddress(&mut reader)?);
            } else if reader.skip_noncritical(code)? {
                value
                    .options
                    .ignored_bits
                    .extend_from_slice(&bits[start..reader.offset]);
            } else {
                value.options.unparsed_bits = bits[start..].to_vec();
                break;
            }
        }
        Ok(value)
    }
}
#[cfg(test)]
mod tests;
