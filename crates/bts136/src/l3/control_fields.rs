use super::legacy_traffic::Subaddress;
use std::{error::Error as StdError, fmt};
pub(super) const OCTET_BITS: usize = 8;
pub(super) const PARAMETER_BITS: usize = 4;
pub(super) const MAX_MESSAGE_OCTETS: usize = 255;
const RESERVED_BITS: usize = 3;
const KIND_BITS: usize = 3;
const NSAP: u8 = 0;
const USER: u8 = 2;
const MAX_SUBADDRESS_OCTETS: usize = 20;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated { needed_bits: usize },
    Length,
    Protocol,
    MessageType,
    Field(&'static str),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid IS-136 control message: {self:?}")
    }
}
impl StdError for Error {}
pub(super) fn put(bits: &mut Vec<bool>, value: u64, width: usize) -> Result<(), Error> {
    if value >= 1 << width {
        return Err(Error::Field("field_width"));
    }
    bits.extend((0..width).rev().map(|shift| value & (1 << shift) != 0));
    Ok(())
}
pub(super) struct Reader<'a> {
    pub bits: &'a [bool],
    pub offset: usize,
}
impl Reader<'_> {
    pub fn take(&mut self, width: usize) -> Result<u64, Error> {
        let end = self.offset + width;
        let bits = self
            .bits
            .get(self.offset..end)
            .ok_or(Error::Truncated { needed_bits: end })?;
        self.offset = end;
        Ok(bits
            .iter()
            .fold(0, |value, bit| value << 1 | u64::from(*bit)))
    }
    // TIA-136-123-E §5 gives unknown parameters 8–14 an octet length and permits continued parsing.
    pub fn skip_noncritical(&mut self, parameter: u8) -> Result<bool, Error> {
        const FIRST_NONCRITICAL: u8 = 8;
        const LAST_NONCRITICAL: u8 = 14;
        if !(FIRST_NONCRITICAL..=LAST_NONCRITICAL).contains(&parameter) {
            return Ok(false);
        }
        let octets = self.take(OCTET_BITS)? as usize;
        let end = self.offset + octets * OCTET_BITS;
        self.bits
            .get(self.offset..end)
            .ok_or(Error::Truncated { needed_bits: end })?;
        self.offset = end;
        Ok(true)
    }
    pub fn flag(&mut self) -> Result<bool, Error> {
        self.take(1).map(|value| value != 0)
    }
}
pub(super) fn bits(bytes: &[u8]) -> Result<Vec<bool>, Error> {
    if bytes.len() > MAX_MESSAGE_OCTETS {
        return Err(Error::Length);
    }
    Ok(bytes
        .iter()
        .flat_map(|byte| {
            (0..OCTET_BITS)
                .rev()
                .map(move |shift| byte & (1 << shift) != 0)
        })
        .collect())
}
pub(super) fn pack(mut bits: Vec<bool>) -> Result<Vec<u8>, Error> {
    bits.resize(bits.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
    if bits.len() / OCTET_BITS > MAX_MESSAGE_OCTETS {
        return Err(Error::Length);
    }
    Ok(bits
        .as_chunks::<OCTET_BITS>()
        .0
        .iter()
        .map(|bits| {
            bits.iter()
                .fold(0u8, |value, bit| value << 1 | u8::from(*bit))
        })
        .collect())
}
// TIA-136-123-E §6.235 includes one header octet in N and up to 20 address octets.
pub(super) fn write_subaddress(
    bits: &mut Vec<bool>,
    parameter: u8,
    address: &Subaddress,
) -> Result<(), Error> {
    if address.address.len() > MAX_SUBADDRESS_OCTETS || !matches!(address.kind, NSAP | USER) {
        return Err(Error::Field("subaddress"));
    }
    put(bits, u64::from(parameter), PARAMETER_BITS)?;
    put(bits, (address.address.len() + 1) as u64, OCTET_BITS)?;
    put(bits, 0, RESERVED_BITS)?;
    bits.push(address.odd);
    put(bits, u64::from(address.kind), KIND_BITS)?;
    bits.push(false);
    for byte in &address.address {
        put(bits, u64::from(*byte), OCTET_BITS)?;
    }
    Ok(())
}
pub(super) fn read_subaddress(reader: &mut Reader<'_>) -> Result<Subaddress, Error> {
    let count = reader.take(OCTET_BITS)? as usize;
    if count == 0 || count > MAX_SUBADDRESS_OCTETS + 1 {
        return Err(Error::Field("subaddress_length"));
    }
    if reader.take(RESERVED_BITS)? != 0 {
        return Err(Error::Field("subaddress_reserved"));
    }
    let odd = reader.flag()?;
    let kind = reader.take(KIND_BITS)? as u8;
    if !matches!(kind, NSAP | USER) || reader.flag()? {
        return Err(Error::Field("subaddress_kind"));
    }
    let address = (1..count)
        .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Subaddress { odd, kind, address })
}

pub(super) fn read_address(reader: &mut Reader<'_>) -> Result<super::address::AddressInfo, Error> {
    let count = reader.take(OCTET_BITS)? as usize;
    let mut bytes = vec![count as u8];
    for _ in 0..count {
        bytes.push(reader.take(OCTET_BITS)? as u8);
    }
    super::address::decode_prefix(&bytes)
        .map(|value| value.address)
        .map_err(|_| Error::Field("address"))
}
pub(super) fn write_address(
    bits: &mut Vec<bool>,
    address: &super::address::AddressInfo,
) -> Result<(), Error> {
    for byte in address.encode().map_err(|_| Error::Field("address"))? {
        put(bits, u64::from(byte), OCTET_BITS)?;
    }
    Ok(())
}
