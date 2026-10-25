pub use super::control_fields::Error;
use super::{
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_subaddress, write_subaddress,
    },
    legacy_traffic::Subaddress,
    relay::Direction,
};
const TYPE_BITS: usize = 6;
const SOC_BITS: usize = 12;
const BSMC_BITS: usize = 8;
const FORWARD_SOC: u8 = 9;
const FORWARD_BSMC: u8 = 2;
const REVERSE_SOC: u8 = 14;
const REVERSE_BSMC: u8 = 4;
const SUBADDRESS: u8 = 1;
const RESERVED: u8 = 0;
const EXTENSION: u8 = 7;
const FUTURE: u8 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Soc(u16),
    Bsmc(u8),
}
impl Vendor {
    fn message_type(self, direction: Direction) -> u8 {
        match (self, direction) {
            (Self::Soc(_), Direction::Forward) => FORWARD_SOC,
            (Self::Soc(_), Direction::Reverse) => REVERSE_SOC,
            (Self::Bsmc(_), Direction::Forward) => FORWARD_BSMC,
            (Self::Bsmc(_), Direction::Reverse) => REVERSE_BSMC,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix {
    pub vendor: Vendor,
    /// TIA-136-123-E §6.39 leaves payload framing to the vendor. This suffix includes optional fields and octet padding.
    pub trailing_bits: Vec<bool>,
}
impl Prefix {
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
        let vendor = match (header, direction) {
            (FORWARD_SOC, Direction::Forward) | (REVERSE_SOC, Direction::Reverse) => {
                Vendor::Soc(reader.take(SOC_BITS)? as u16)
            }
            (FORWARD_BSMC, Direction::Forward) | (REVERSE_BSMC, Direction::Reverse) => {
                Vendor::Bsmc(reader.take(BSMC_BITS)? as u8)
            }
            _ => return Err(Error::MessageType),
        };
        Ok(Self {
            vendor,
            trailing_bits: bits[reader.offset..].to_vec(),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub vendor: Vendor,
    pub custom_control: Vec<bool>,
    pub subaddress: Option<Subaddress>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
impl Delivery {
    pub fn encode(&self, direction: Direction) -> Result<Vec<u8>, Error> {
        if self.custom_control.is_empty() {
            return Err(Error::Field("custom_control_length"));
        }
        let mut bits = Vec::new();
        put(
            &mut bits,
            u64::from(self.vendor.message_type(direction)),
            OCTET_BITS,
        )?;
        match self.vendor {
            Vendor::Soc(code) => put(&mut bits, u64::from(code), SOC_BITS)?,
            Vendor::Bsmc(code) => put(&mut bits, u64::from(code), BSMC_BITS)?,
        }
        bits.extend(&self.custom_control);
        if let Some(address) = &self.subaddress {
            write_subaddress(&mut bits, SUBADDRESS, address)?;
        }
        bits.extend(&self.ignored_bits);
        bits.extend(&self.unparsed_bits);
        pack(bits)
    }
    /// The vendor protocol must supply the Custom Control bit length before optional fields can be located.
    pub fn decode(
        bytes: &[u8],
        direction: Direction,
        custom_control_bits: usize,
    ) -> Result<Self, Error> {
        if custom_control_bits == 0 {
            return Err(Error::Field("custom_control_length"));
        }
        let prefix = Prefix::decode(bytes, direction)?;
        let payload = prefix
            .trailing_bits
            .get(..custom_control_bits)
            .ok_or(Error::Field("custom_control_length"))?
            .to_vec();
        let mut value = Self {
            vendor: prefix.vendor,
            custom_control: payload,
            subaddress: None,
            ignored_bits: vec![],
            unparsed_bits: vec![],
        };
        let mut reader = Reader {
            bits: &prefix.trailing_bits,
            offset: custom_control_bits,
        };
        while reader.offset < reader.bits.len() {
            let remaining = &reader.bits[reader.offset..];
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
                if value.subaddress.is_some() {
                    return Err(Error::Field("parameter_order"));
                }
                value.subaddress = Some(read_subaddress(&mut reader)?);
            } else if reader.skip_noncritical(code)? {
                value
                    .ignored_bits
                    .extend_from_slice(&reader.bits[start..reader.offset]);
            } else {
                value.unparsed_bits = reader.bits[start..].to_vec();
                break;
            }
        }
        Ok(value)
    }
}
#[cfg(test)]
mod tests;
