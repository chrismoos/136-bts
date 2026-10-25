use super::*;
const MAX_SYSTEMS: usize = 16;
const MAX_NAME_CHARACTERS: usize = 15;
const NAME_LENGTH_BITS: usize = 4;
const BMP: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameList {
    encoding: Option<u8>,
    body: Vec<u8>,
}
impl NameList {
    pub fn try_new(encoding: Option<u8>, names: &[Vec<u8>]) -> Result<Self, Error> {
        if names.len() > MAX_SYSTEMS {
            return Err(Error::Field("name_count"));
        }
        let mut bits = Vec::new();
        for name in names {
            validate_name(encoding, name)?;
            let width = if encoding == Some(BMP) { 2 } else { 1 };
            put(&mut bits, (name.len() / width) as u64, NAME_LENGTH_BITS)?;
            for byte in name {
                put(&mut bits, u64::from(*byte), OCTET_BITS)?;
            }
        }
        Self::from_body(encoding, pack(bits)?)
    }
    pub fn encoding(&self) -> Option<u8> {
        self.encoding
    }
    pub fn encoded_names(&self) -> &[u8] {
        &self.body
    }
    // TIA-136-123-E §§6.6/6.141 require the broadcast system count to distinguish an empty final name from filler.
    pub fn resolve(&self, system_count: usize) -> Result<Vec<Vec<u8>>, Error> {
        if system_count > MAX_SYSTEMS {
            return Err(Error::Field("name_count"));
        }
        let bits = control_fields::bits(&self.body)?;
        let mut reader = Reader {
            bits: &bits,
            offset: 0,
        };
        let mut names = Vec::new();
        for _ in 0..system_count {
            names.push(self.read_name(&mut reader)?);
        }
        let remaining = &bits[reader.offset..];
        let padding = if system_count.is_multiple_of(2) {
            0
        } else {
            NAME_LENGTH_BITS
        };
        if remaining.len() != padding || remaining.iter().any(|bit| *bit) {
            return Err(Error::Field("name_list_padding"));
        }
        Ok(names)
    }
    pub(super) fn from_body(encoding: Option<u8>, body: Vec<u8>) -> Result<Self, Error> {
        let value = Self { encoding, body };
        validate_name(encoding, &[])?;
        value.entries()?;
        Ok(value)
    }
    pub(crate) fn entries(&self) -> Result<Vec<Vec<u8>>, Error> {
        let bits = control_fields::bits(&self.body)?;
        let mut reader = Reader {
            bits: &bits,
            offset: 0,
        };
        let mut names = Vec::new();
        while reader.offset < bits.len() {
            let remaining = &bits[reader.offset..];
            if remaining.len() == NAME_LENGTH_BITS
                && remaining.iter().all(|bit| !*bit)
                && names.len() < MAX_SYSTEMS
            {
                break;
            }
            if names.len() == MAX_SYSTEMS {
                return Err(Error::Field("name_count"));
            }
            names.push(self.read_name(&mut reader)?);
        }
        Ok(names)
    }
    fn read_name(&self, reader: &mut Reader<'_>) -> Result<Vec<u8>, Error> {
        let characters = reader.take(NAME_LENGTH_BITS)? as usize;
        let octets = characters * if self.encoding == Some(BMP) { 2 } else { 1 };
        let name = (0..octets)
            .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
            .collect::<Result<Vec<_>, _>>()?;
        validate_name(self.encoding, &name)?;
        Ok(name)
    }
    pub(super) fn validate_map(&self, accepted: u16) -> Result<(), Error> {
        for (index, name) in self.entries()?.iter().enumerate() {
            if !name.is_empty() && accepted & (1 << index) == 0 {
                return Err(Error::Field("unaccepted_system_name"));
            }
        }
        Ok(())
    }
}
pub(super) fn validate_name(encoding: Option<u8>, bytes: &[u8]) -> Result<(), Error> {
    let width = if encoding == Some(BMP) { 2 } else { 1 };
    if bytes.len() > MAX_NAME_CHARACTERS * width {
        return Err(Error::Field("system_name_length"));
    }
    match encoding {
        None if bytes.is_ascii() => Ok(()),
        Some(encoding) if (2..=18).contains(&encoding) => {
            crate::l3::audit::validate_multilingual(&crate::l3::audit::MultilingualDisplay {
                encoding,
                bytes: bytes.to_vec(),
            })
        }
        _ => Err(Error::Field("system_name_encoding")),
    }
}
