mod names;
pub use super::control_fields::Error;
use super::{
    address::AddressInfo,
    audit::MultilingualDisplay,
    broadcast::ProtocolVersion,
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_address, read_subaddress,
        write_address, write_subaddress,
    },
    legacy_traffic::Subaddress,
};
pub use names::NameList;
use names::validate_name;
pub const REQUEST_TYPE: u8 = 17;
pub const RESPONSE_TYPE: u8 = 35;
const MAP_BITS: usize = 16;
const SUBADDRESS: u8 = 1;
const NUMBER_REQUEST: u8 = 2;
const SYSTEM_ID: u8 = 8;
const NAMES: u8 = 9;
const DIRECTORY: u8 = 10;
const MULTILINGUAL_ID: u8 = 11;
const MULTILINGUAL_NAMES: u8 = 12;
const ENCODING_BITS: usize = 5;
const RESERVED_BITS: usize = 3;
const EXTENSION: u8 = 7;
const RESERVED: u8 = 0;
const FUTURE: u8 = 15;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    pub selected_systems: u16,
    pub subaddress: Option<Subaddress>,
    pub number_request: Option<bool>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    pub accepted_systems: u16,
    pub subaddress: Option<Subaddress>,
    pub system_id: Option<String>,
    pub names: Option<NameList>,
    pub directory: Option<AddressInfo>,
    pub multilingual_id: Option<MultilingualDisplay>,
    pub multilingual_names: Option<NameList>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
fn header(bits: &mut Vec<bool>, kind: u8, map: u16) -> Result<(), Error> {
    put(bits, u64::from(kind), OCTET_BITS)?;
    put(bits, u64::from(map), MAP_BITS)
}
fn parameter(reader: &mut Reader<'_>) -> Result<Option<(usize, u8)>, Error> {
    let remaining = &reader.bits[reader.offset..];
    if remaining.is_empty() || remaining.len() < OCTET_BITS && remaining.iter().all(|bit| !*bit) {
        return Ok(None);
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
    Ok(Some((start, code)))
}
fn unknown(
    reader: &mut Reader<'_>,
    start: usize,
    code: u8,
    ignored: &mut Vec<bool>,
    unparsed: &mut Vec<bool>,
) -> Result<bool, Error> {
    if reader.skip_noncritical(code)? {
        ignored.extend_from_slice(&reader.bits[start..reader.offset]);
        return Ok(false);
    }
    *unparsed = reader.bits[start..].to_vec();
    Ok(true)
}
fn order(previous: &mut u8, code: u8) -> Result<(), Error> {
    if code <= *previous {
        return Err(Error::Field("parameter_order"));
    }
    *previous = code;
    Ok(())
}
fn read_octets(reader: &mut Reader<'_>, count: usize) -> Result<Vec<u8>, Error> {
    (0..count)
        .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
        .collect()
}
fn write_counted(bits: &mut Vec<bool>, code: u8, bytes: &[u8]) -> Result<(), Error> {
    put(bits, u64::from(code), PARAMETER_BITS)?;
    put(bits, bytes.len() as u64, OCTET_BITS)?;
    for byte in bytes {
        put(bits, u64::from(*byte), OCTET_BITS)?;
    }
    Ok(())
}
fn read_encoding(reader: &mut Reader<'_>) -> Result<u8, Error> {
    let encoding = reader.take(ENCODING_BITS)? as u8;
    if reader.take(RESERVED_BITS)? != 0 {
        return Err(Error::Field("system_encoding_reserved"));
    }
    validate_name(Some(encoding), &[])?;
    Ok(encoding)
}
impl Request {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), Error> {
        if self.number_request.is_some() && version.code() < ProtocolVersion::Pv1.code() {
            return Err(Error::Field("test_registration_version"));
        }
        self.encode().map(|_| ())
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bits = vec![];
        header(&mut bits, REQUEST_TYPE, self.selected_systems)?;
        if let Some(address) = &self.subaddress {
            write_subaddress(&mut bits, SUBADDRESS, address)?;
        }
        if let Some(request) = self.number_request {
            put(&mut bits, u64::from(NUMBER_REQUEST), PARAMETER_BITS)?;
            put(&mut bits, u64::from(request), PARAMETER_BITS)?;
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
        if reader.take(OCTET_BITS)? != u64::from(REQUEST_TYPE) {
            return Err(Error::MessageType);
        }
        let mut value = Self {
            selected_systems: reader.take(MAP_BITS)? as u16,
            ..Default::default()
        };
        let mut previous = 0;
        while let Some((start, code)) = parameter(&mut reader)? {
            match code {
                SUBADDRESS => {
                    order(&mut previous, code)?;
                    value.subaddress = Some(read_subaddress(&mut reader)?);
                }
                NUMBER_REQUEST => {
                    order(&mut previous, code)?;
                    value.number_request = Some(match reader.take(PARAMETER_BITS)? {
                        0 => false,
                        1 => true,
                        _ => return Err(Error::Field("number_request")),
                    });
                }
                _ => {
                    if unknown(
                        &mut reader,
                        start,
                        code,
                        &mut value.ignored_bits,
                        &mut value.unparsed_bits,
                    )? {
                        break;
                    }
                }
            }
        }
        Ok(value)
    }
}
impl Response {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), Error> {
        if (self.directory.is_some()
            || self.multilingual_id.is_some()
            || self.multilingual_names.is_some())
            && version.code() < ProtocolVersion::Pv1.code()
        {
            return Err(Error::Field("test_registration_version"));
        }
        self.encode().map(|_| ())
    }
    pub fn validate_for_request(
        &self,
        request: &Request,
        version: ProtocolVersion,
    ) -> Result<(), Error> {
        request.validate_version(version)?;
        self.validate_version(version)?;
        if request.number_request == Some(true) && self.directory.is_none() {
            return Err(Error::Field("directory_required"));
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bits = vec![];
        header(&mut bits, RESPONSE_TYPE, self.accepted_systems)?;
        if let Some(address) = &self.subaddress {
            write_subaddress(&mut bits, SUBADDRESS, address)?;
        }
        if let Some(name) = &self.system_id {
            validate_name(None, name.as_bytes())?;
            write_counted(&mut bits, SYSTEM_ID, name.as_bytes())?;
        }
        if let Some(names) = &self.names {
            if names.encoding().is_some() {
                return Err(Error::Field("ira_names_encoding"));
            }
            names.validate_map(self.accepted_systems)?;
            write_counted(&mut bits, NAMES, names.encoded_names())?;
        }
        if let Some(address) = &self.directory {
            put(&mut bits, u64::from(DIRECTORY), PARAMETER_BITS)?;
            write_address(&mut bits, address)?;
        }
        if let Some(name) = &self.multilingual_id {
            validate_name(Some(name.encoding), &name.bytes)?;
            let mut bytes = vec![name.encoding << RESERVED_BITS];
            bytes.extend(&name.bytes);
            write_counted(&mut bits, MULTILINGUAL_ID, &bytes)?;
        }
        if let Some(names) = &self.multilingual_names {
            let encoding = names
                .encoding()
                .ok_or(Error::Field("multilingual_names_encoding"))?;
            names.validate_map(self.accepted_systems)?;
            let mut bytes = vec![encoding << RESERVED_BITS];
            bytes.extend(names.encoded_names());
            write_counted(&mut bits, MULTILINGUAL_NAMES, &bytes)?;
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
        if reader.take(OCTET_BITS)? != u64::from(RESPONSE_TYPE) {
            return Err(Error::MessageType);
        }
        let mut value = Self {
            accepted_systems: reader.take(MAP_BITS)? as u16,
            ..Default::default()
        };
        let mut previous = 0;
        while let Some((start, code)) = parameter(&mut reader)? {
            if code != SUBADDRESS && !(SYSTEM_ID..=MULTILINGUAL_NAMES).contains(&code) {
                if unknown(
                    &mut reader,
                    start,
                    code,
                    &mut value.ignored_bits,
                    &mut value.unparsed_bits,
                )? {
                    break;
                }
                continue;
            }
            order(&mut previous, code)?;
            match code {
                SUBADDRESS => value.subaddress = Some(read_subaddress(&mut reader)?),
                DIRECTORY => value.directory = Some(read_address(&mut reader)?),
                _ => {
                    let count = reader.take(OCTET_BITS)? as usize;
                    let encoding = if matches!(code, MULTILINGUAL_ID | MULTILINGUAL_NAMES) {
                        if count == 0 {
                            return Err(Error::Field("system_encoding_length"));
                        }
                        Some(read_encoding(&mut reader)?)
                    } else {
                        None
                    };
                    let bytes = read_octets(&mut reader, count - usize::from(encoding.is_some()))?;
                    match code {
                        SYSTEM_ID => {
                            validate_name(None, &bytes)?;
                            value.system_id = Some(
                                String::from_utf8(bytes).map_err(|_| Error::Field("system_id"))?,
                            );
                        }
                        NAMES => {
                            let names = NameList::from_body(None, bytes)?;
                            names.validate_map(value.accepted_systems)?;
                            value.names = Some(names);
                        }
                        MULTILINGUAL_ID => {
                            validate_name(encoding, &bytes)?;
                            value.multilingual_id = Some(MultilingualDisplay {
                                encoding: encoding.ok_or(Error::Field("system_encoding"))?,
                                bytes,
                            });
                        }
                        MULTILINGUAL_NAMES => {
                            let names = NameList::from_body(encoding, bytes)?;
                            names.validate_map(value.accepted_systems)?;
                            value.multilingual_names = Some(names);
                        }
                        _ => return Err(Error::Field("parameter")),
                    }
                }
            }
        }
        Ok(value)
    }
}
#[cfg(test)]
mod tests;
