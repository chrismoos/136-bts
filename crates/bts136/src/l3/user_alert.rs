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
pub const MESSAGE_TYPE: u8 = 37;
const IS136_PROTOCOL: u64 = 0;
const TYPE_BITS: usize = 6;
const PROTOCOL_BITS: usize = 2;
const SIGNAL: u8 = 1;
const SUBADDRESS: u8 = 2;
const PITCH_BITS: usize = 2;
const CADENCE_BITS: usize = 6;
const DURATION_BITS: usize = 4;
const MAX_CADENCE: u8 = 12;
const STANDARD_CADENCE: u8 = 1;
pub const CONTINUOUS_DURATION: u8 = 15;
const DISPLAY: u8 = 8;
const MULTILINGUAL: u8 = 9;
const ENCODING_BITS: usize = 5;
const RESERVED_BITS: usize = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Pitch {
    Medium = 0,
    High = 1,
    Low = 2,
}
impl TryFrom<u8> for Pitch {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Medium),
            1 => Ok(Self::High),
            2 => Ok(Self::Low),
            _ => Err(Error::Field("signal_pitch")),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    pub pitch: Pitch,
    pub cadence: u8,
    pub duration_code: u8,
}
impl Default for Signal {
    fn default() -> Self {
        Self {
            pitch: Pitch::Medium,
            cadence: STANDARD_CADENCE,
            duration_code: CONTINUOUS_DURATION,
        }
    }
}
impl Signal {
    pub(super) fn write(self, bits: &mut Vec<bool>, parameter: u8) -> Result<(), Error> {
        self.validate()?;
        put(bits, u64::from(parameter), PARAMETER_BITS)?;
        put(bits, self.pitch as u64, PITCH_BITS)?;
        put(bits, u64::from(self.cadence), CADENCE_BITS)?;
        put(bits, u64::from(self.duration_code), DURATION_BITS)
    }
    pub(super) fn read(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let signal = Self {
            pitch: Pitch::try_from(reader.take(PITCH_BITS)? as u8)?,
            cadence: reader.take(CADENCE_BITS)? as u8,
            duration_code: reader.take(DURATION_BITS)? as u8,
        };
        signal.validate()?;
        Ok(signal)
    }
    fn validate(self) -> Result<(), Error> {
        if self.cadence > MAX_CADENCE || self.duration_code > CONTINUOUS_DURATION {
            return Err(Error::Field("signal"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserAlert {
    pub signal: Option<Signal>,
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
impl UserAlert {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), Error> {
        if self.multilingual_display.is_some() && version.code() < ProtocolVersion::Pv1.code() {
            return Err(Error::Field("multilingual_protocol_version"));
        }
        self.encode().map(|_| ())
    }
    // TIA-136-123-E §§5.3.23,6.221 put the optional twelve-bit Signal after parameter code one.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bits = Vec::new();
        header(&mut bits, MESSAGE_TYPE)?;
        if let Some(signal) = self.signal {
            signal.write(&mut bits, SIGNAL)?;
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
        let mut result = Self::default();
        let mut previous = 0;
        while let Some((start, code)) = next(&mut reader)? {
            if !matches!(code, SIGNAL | SUBADDRESS | DISPLAY | MULTILINGUAL) {
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
                SIGNAL => result.signal = Some(Signal::read(&mut reader)?),
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
