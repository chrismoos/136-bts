pub use super::control_fields::Error;
use super::{
    access_call::Presentation,
    address::{AddressInfo, AddressValue},
    audit::{
        MULTILINGUAL_HEADER_OCTETS, MultilingualDisplay, display_octets, text,
        validate_multilingual,
    },
    broadcast::ProtocolVersion,
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_address, read_subaddress,
        write_address, write_subaddress,
    },
    legacy_traffic::Subaddress,
    page_response::ServiceCode,
    user_alert::Signal,
};
pub const MESSAGE_TYPE: u8 = 24;
const SERVICE_BITS: usize = 4;
const CALLED_SUBADDRESS: u8 = 1;
const SIGNAL: u8 = 2;
const PRESENTATION: u8 = 3;
const CALLING_NUMBER: u8 = 8;
const CALLING_SUBADDRESS: u8 = 9;
const DISPLAY: u8 = 10;
const CALLED_PARTY: u8 = 11;
const MULTILINGUAL: u8 = 12;
const RESERVED_PARAMETER: u8 = 0;
const FUTURE_PARAMETER: u8 = 15;
const RESERVED_PLAN: u8 = 12;
const ALPHANUMERIC_INTERNET_PLAN: u8 = 14;
const PRESENTATION_BITS: usize = 2;
const RESERVED_PRESENTATION: u8 = 3;
const ENCODING_BITS: usize = 5;
const RESERVED_BITS: usize = 3;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub service: ServiceCode,
    pub called_subaddress: Option<Subaddress>,
    pub signal: Option<Signal>,
    pub presentation: Option<Presentation>,
    pub calling_number: Option<AddressInfo>,
    pub calling_subaddress: Option<Subaddress>,
    pub display: Option<String>,
    pub called_party: Option<AddressInfo>,
    pub multilingual_display: Option<MultilingualDisplay>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            service: ServiceCode::DigitalSpeech,
            called_subaddress: None,
            signal: None,
            presentation: None,
            calling_number: None,
            calling_subaddress: None,
            display: None,
            called_party: None,
            multilingual_display: None,
            ignored_bits: Vec::new(),
            unparsed_bits: Vec::new(),
        }
    }
}
impl Page {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), Error> {
        if self.multilingual_display.is_some() && version.code() < ProtocolVersion::Pv1.code() {
            return Err(Error::Field("multilingual_protocol_version"));
        }
        self.encode().map(|_| ())
    }
    // TIA-136-123-E §5.3.9 uses control Address Info and a separate presentation IE.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bits = Vec::new();
        put(&mut bits, u64::from(MESSAGE_TYPE), OCTET_BITS)?;
        put(&mut bits, self.service as u64, SERVICE_BITS)?;
        if let Some(value) = &self.called_subaddress {
            write_subaddress(&mut bits, CALLED_SUBADDRESS, value)?;
        }
        if let Some(value) = self.signal {
            value.write(&mut bits, SIGNAL)?;
        }
        if let Some(value) = self.presentation {
            validate_presentation(value)?;
            put(&mut bits, u64::from(PRESENTATION), PARAMETER_BITS)?;
            put(&mut bits, u64::from(value.presentation), PRESENTATION_BITS)?;
            put(&mut bits, u64::from(value.screening), PRESENTATION_BITS)?;
        }
        if let Some(value) = &self.calling_number {
            address(&mut bits, CALLING_NUMBER, value)?;
        }
        if let Some(value) = &self.calling_subaddress {
            write_subaddress(&mut bits, CALLING_SUBADDRESS, value)?;
        }
        if let Some(value) = &self.display {
            if !value.is_ascii() {
                return Err(Error::Field("display_encoding"));
            }
            put(&mut bits, u64::from(DISPLAY), PARAMETER_BITS)?;
            text(&mut bits, value.as_bytes())?;
        }
        if let Some(value) = &self.called_party {
            address(&mut bits, CALLED_PARTY, value)?;
        }
        if let Some(value) = &self.multilingual_display {
            validate_multilingual(value)?;
            put(&mut bits, u64::from(MULTILINGUAL), PARAMETER_BITS)?;
            put(
                &mut bits,
                (value.bytes.len() + MULTILINGUAL_HEADER_OCTETS) as u64,
                OCTET_BITS,
            )?;
            put(&mut bits, u64::from(value.encoding), ENCODING_BITS)?;
            put(&mut bits, 0, RESERVED_BITS)?;
            for byte in &value.bytes {
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
        if reader.take(OCTET_BITS)? != u64::from(MESSAGE_TYPE) {
            return Err(Error::MessageType);
        }
        let service = ServiceCode::decode(reader.take(SERVICE_BITS)? as usize)
            .map_err(|_| Error::Field("service_code"))?;
        let mut page = Self {
            service,
            ..Default::default()
        };
        let mut previous = 0;
        while reader.offset < bits.len() {
            let remaining = &bits[reader.offset..];
            if remaining.len() < OCTET_BITS && remaining.iter().all(|bit| !*bit) {
                break;
            }
            let start = reader.offset;
            let code = reader.take(PARAMETER_BITS)? as u8;
            if matches!(code, RESERVED_PARAMETER | FUTURE_PARAMETER) {
                return Err(Error::Field("optional_reserved"));
            }
            if !matches!(
                code,
                CALLED_SUBADDRESS
                    | SIGNAL
                    | PRESENTATION
                    | CALLING_NUMBER
                    | CALLING_SUBADDRESS
                    | DISPLAY
                    | CALLED_PARTY
                    | MULTILINGUAL
            ) {
                if reader.skip_noncritical(code)? {
                    page.ignored_bits
                        .extend_from_slice(&bits[start..reader.offset]);
                    continue;
                }
                page.unparsed_bits = bits[start..].to_vec();
                break;
            }
            if code <= previous {
                return Err(Error::Field("parameter_order"));
            }
            previous = code;
            match code {
                CALLED_SUBADDRESS => page.called_subaddress = Some(read_subaddress(&mut reader)?),
                CALLING_SUBADDRESS => page.calling_subaddress = Some(read_subaddress(&mut reader)?),
                SIGNAL => page.signal = Some(Signal::read(&mut reader)?),
                PRESENTATION => {
                    let value = Presentation {
                        presentation: reader.take(PRESENTATION_BITS)? as u8,
                        screening: reader.take(PRESENTATION_BITS)? as u8,
                    };
                    validate_presentation(value)?;
                    page.presentation = Some(value);
                }
                CALLING_NUMBER | CALLED_PARTY => {
                    let value = read_address(&mut reader)?;
                    validate_address(&value)?;
                    if code == CALLING_NUMBER {
                        page.calling_number = Some(value);
                    } else {
                        page.called_party = Some(value);
                    }
                }
                DISPLAY | MULTILINGUAL => {
                    let count = reader.take(OCTET_BITS)? as usize;
                    let encoding = if code == MULTILINGUAL {
                        let encoding = reader.take(ENCODING_BITS)? as u8;
                        if reader.take(RESERVED_BITS)? != 0 {
                            return Err(Error::Field("display_reserved"));
                        }
                        Some(encoding)
                    } else {
                        None
                    };
                    let count = display_octets(count, encoding)?;
                    let bytes = (0..count)
                        .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Some(encoding) = encoding {
                        let value = MultilingualDisplay { encoding, bytes };
                        validate_multilingual(&value)?;
                        page.multilingual_display = Some(value);
                    } else {
                        if !bytes.is_ascii() {
                            return Err(Error::Field("display_encoding"));
                        }
                        page.display = Some(
                            String::from_utf8(bytes)
                                .map_err(|_| Error::Field("display_encoding"))?,
                        );
                    }
                }
                _ => unreachable!(),
            }
        }
        Ok(page)
    }
}
fn validate_presentation(value: Presentation) -> Result<(), Error> {
    if value.presentation >= RESERVED_PRESENTATION || value.screening >= 1 << PRESENTATION_BITS {
        return Err(Error::Field("presentation"));
    }
    Ok(())
}
fn validate_address(value: &AddressInfo) -> Result<(), Error> {
    if value.numbering_plan == RESERVED_PLAN {
        return Err(Error::Field("reserved_numbering_plan"));
    }
    if value.numbering_plan == ALPHANUMERIC_INTERNET_PLAN
        && !matches!(value.address, AddressValue::Ira(_))
    {
        return Err(Error::Field("internet_address_encoding"));
    }
    Ok(())
}
fn address(bits: &mut Vec<bool>, code: u8, value: &AddressInfo) -> Result<(), Error> {
    validate_address(value)?;
    put(bits, u64::from(code), PARAMETER_BITS)?;
    write_address(bits, value)
}
#[cfg(test)]
mod tests;
