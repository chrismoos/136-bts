use super::CONFIRMATION_TYPE;
use crate::l3::{
    control_fields::{
        self, Error, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_subaddress,
        write_subaddress,
    },
    legacy_traffic::Subaddress,
};
const IS136_PROTOCOL: u64 = 0;
const TYPE_BITS: usize = 6;
const PROTOCOL_BITS: usize = 2;
const SUBADDRESS: u8 = 1;
fn header(bits: &mut Vec<bool>, kind: u8) -> Result<(), Error> {
    put(bits, IS136_PROTOCOL, PROTOCOL_BITS)?;
    put(bits, u64::from(kind), TYPE_BITS)
}
fn prefix(reader: &mut Reader<'_>, kind: u8) -> Result<(), Error> {
    if reader.take(PROTOCOL_BITS)? != IS136_PROTOCOL {
        return Err(Error::Protocol);
    }
    if reader.take(TYPE_BITS)? != u64::from(kind) {
        return Err(Error::MessageType);
    }
    Ok(())
}
fn next(reader: &mut Reader<'_>) -> Result<Option<(usize, u8)>, Error> {
    let remaining = &reader.bits[reader.offset..];
    if remaining.len() < OCTET_BITS && remaining.iter().all(|bit| !*bit) {
        reader.offset = reader.bits.len();
        return Ok(None);
    }
    let start = reader.offset;
    Ok(Some((start, reader.take(PARAMETER_BITS)? as u8)))
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpachConfirmation {
    pub confirmed_type: u8,
    pub subaddress: Option<Subaddress>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
impl SpachConfirmation {
    // TIA-136-123-E §5.4.15 has six type bits followed immediately by optional IEs.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bits = Vec::new();
        header(&mut bits, CONFIRMATION_TYPE)?;
        put(&mut bits, u64::from(self.confirmed_type), TYPE_BITS)?;
        if let Some(address) = &self.subaddress {
            write_subaddress(&mut bits, SUBADDRESS, address)?;
        }
        bits.extend(&self.ignored_bits);
        bits.extend(&self.unparsed_bits);
        pack(bits)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let bits = control_fields::bits(bytes)?;
        let mut reader = Reader {
            bits: &bits,
            offset: 0,
        };
        prefix(&mut reader, CONFIRMATION_TYPE)?;
        let mut result = Self {
            confirmed_type: reader.take(TYPE_BITS)? as u8,
            ..Default::default()
        };
        while let Some((start, code)) = next(&mut reader)? {
            if code == SUBADDRESS {
                if result.subaddress.is_some() {
                    return Err(Error::Field("duplicate_subaddress"));
                }
                result.subaddress = Some(read_subaddress(&mut reader)?);
            } else if reader.skip_noncritical(code)? {
                result
                    .ignored_bits
                    .extend_from_slice(&bits[start..reader.offset]);
            } else {
                result.unparsed_bits = bits[start..].to_vec();
                break;
            }
        }
        Ok(result)
    }
}
