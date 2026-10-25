pub use super::control_fields::Error;
use super::{
    audit::{
        MULTILINGUAL_HEADER_OCTETS, MultilingualDisplay, display_octets, text,
        validate_multilingual,
    },
    broadcast::ProtocolVersion,
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_subaddress, write_subaddress,
    },
    legacy_traffic::Subaddress,
};
pub const MESSAGE_TYPE: u8 = 23;
const IS136_PROTOCOL: u64 = 0;
const VOICE_KIND: u8 = 0;
const SMS_KIND: u8 = 1;
const FAX_KIND: u8 = 2;
const FIRST_USER_KIND: u8 = 4;
const LAST_USER_KIND: u8 = 7;
const TYPE_BITS: usize = 6;
const PROTOCOL_BITS: usize = 2;
const INSTANCE_BITS: usize = 4;
const COUNT_BITS: usize = 6;
const SUBADDRESS: u8 = 1;
const DISPLAY: u8 = 8;
const MULTILINGUAL: u8 = 9;
const ENCODING_BITS: usize = 5;
const RESERVED_BITS: usize = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waiting {
    pub kind: u8,
    pub count: u8,
}
impl Waiting {
    fn validate(self) -> Result<(), Error> {
        if !matches!(
            self.kind,
            VOICE_KIND | SMS_KIND | FAX_KIND | FIRST_USER_KIND..=LAST_USER_KIND
        ) || u64::from(self.count) >= 1 << COUNT_BITS
        {
            return Err(Error::Field("message_waiting"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageWaiting {
    pub entries: Vec<Waiting>,
    pub subaddress: Option<Subaddress>,
    pub display: Option<String>,
    pub multilingual_display: Option<MultilingualDisplay>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
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
impl MessageWaiting {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), Error> {
        if self.multilingual_display.is_some() && version.code() < ProtocolVersion::Pv1.code() {
            return Err(Error::Field("multilingual_protocol_version"));
        }
        self.encode().map(|_| ())
    }
    // TIA-136-123-E §§5.3.8,6.129 encode one to sixteen distinct kind/count pairs.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        if self.entries.is_empty() || self.entries.len() > 1 << INSTANCE_BITS {
            return Err(Error::Field("message_waiting_instances"));
        }
        let mut bits = Vec::new();
        header(&mut bits, MESSAGE_TYPE)?;
        put(&mut bits, (self.entries.len() - 1) as u64, INSTANCE_BITS)?;
        let mut seen = 0u16;
        for entry in &self.entries {
            entry.validate()?;
            let mask = 1 << entry.kind;
            if seen & mask != 0 {
                return Err(Error::Field("message_waiting_duplicate"));
            }
            seen |= mask;
            put(&mut bits, u64::from(entry.kind), INSTANCE_BITS)?;
            put(&mut bits, u64::from(entry.count), COUNT_BITS)?;
        }
        if let Some(address) = &self.subaddress {
            write_subaddress(&mut bits, SUBADDRESS, address)?;
        }
        if let Some(display) = &self.display {
            if !display.is_ascii() {
                return Err(Error::Field("display_encoding"));
            }
            put(&mut bits, u64::from(DISPLAY), PARAMETER_BITS)?;
            text(&mut bits, display.as_bytes())?;
        }
        if let Some(display) = &self.multilingual_display {
            validate_multilingual(display)?;
            put(&mut bits, u64::from(MULTILINGUAL), PARAMETER_BITS)?;
            put(
                &mut bits,
                (display.bytes.len() + MULTILINGUAL_HEADER_OCTETS) as u64,
                OCTET_BITS,
            )?;
            put(&mut bits, u64::from(display.encoding), ENCODING_BITS)?;
            put(&mut bits, 0, RESERVED_BITS)?;
            for byte in &display.bytes {
                put(&mut bits, u64::from(*byte), OCTET_BITS)?;
            }
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
        prefix(&mut reader, MESSAGE_TYPE)?;
        let count = reader.take(INSTANCE_BITS)? as usize + 1;
        let mut result = Self::default();
        let mut seen = 0u16;
        for _ in 0..count {
            let entry = Waiting {
                kind: reader.take(INSTANCE_BITS)? as u8,
                count: reader.take(COUNT_BITS)? as u8,
            };
            entry.validate()?;
            let mask = 1 << entry.kind;
            if seen & mask != 0 {
                return Err(Error::Field("message_waiting_duplicate"));
            }
            seen |= mask;
            result.entries.push(entry);
        }
        let mut previous = 0;
        while let Some((start, code)) = next(&mut reader)? {
            if !matches!(code, SUBADDRESS | DISPLAY | MULTILINGUAL) {
                if reader.skip_noncritical(code)? {
                    result
                        .ignored_bits
                        .extend_from_slice(&bits[start..reader.offset]);
                    continue;
                }
                result.unparsed_bits = bits[start..].to_vec();
                break;
            }
            if code <= previous {
                return Err(Error::Field("parameter_order"));
            }
            previous = code;
            match code {
                SUBADDRESS => result.subaddress = Some(read_subaddress(&mut reader)?),
                DISPLAY | MULTILINGUAL => {
                    let length = reader.take(OCTET_BITS)? as usize;
                    let encoding = if code == MULTILINGUAL {
                        let encoding = reader.take(ENCODING_BITS)? as u8;
                        if reader.take(RESERVED_BITS)? != 0 {
                            return Err(Error::Field("display_reserved"));
                        }
                        Some(encoding)
                    } else {
                        None
                    };
                    let length = display_octets(length, encoding)?;
                    let bytes = (0..length)
                        .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Some(encoding) = encoding {
                        let display = MultilingualDisplay { encoding, bytes };
                        validate_multilingual(&display)?;
                        result.multilingual_display = Some(display);
                    } else {
                        if !bytes.is_ascii() {
                            return Err(Error::Field("display_encoding"));
                        }
                        result.display = Some(
                            String::from_utf8(bytes)
                                .map_err(|_| Error::Field("display_encoding"))?,
                        );
                    }
                }
                _ => unreachable!(),
            }
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests;
