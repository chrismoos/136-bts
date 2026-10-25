use super::{IS136_PROTOCOL, RELEASE, REORDER, TYPE_BITS};
pub use crate::l3::control_fields::Error;
use crate::l3::{
    audit::{
        MULTILINGUAL_HEADER_OCTETS, MultilingualDisplay, display_octets, text,
        validate_multilingual,
    },
    broadcast::ProtocolVersion,
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_subaddress, write_subaddress,
    },
    legacy_traffic::Subaddress,
    registration::RejectCause,
    user_alert::Signal,
};
const REGISTRATION_REJECT: u8 = 30;
const REJECT_TIME: u8 = 1;
const TIME_BOUNDARY_BITS: usize = 4;
const RELEASE_SIGNAL: u8 = 1;
const RELEASE_SUBADDRESS: u8 = 2;
const REORDER_SUBADDRESS: u8 = 1;
const RELEASE_CAUSE_EXTENSION: u8 = 3;
const REORDER_CAUSE_EXTENSION: u8 = 2;
const CAUSE_BITS: usize = 4;
const TONE_BITS: usize = 2;
const DISPLAY: u8 = 8;
const MULTILINGUAL: u8 = 9;
const EXTENSION: u8 = 7;
const RESERVED_PARAMETER: u8 = 0;
const FUTURE_PARAMETER: u8 = 15;
const ENCODING_BITS: usize = 5;
const RESERVED_BITS: usize = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseKind {
    Release,
    Reorder,
    RegistrationReject,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Parameter {
    RejectTime,
    Signal,
    Subaddress,
    Display,
    Multilingual,
    CauseExtension,
}
impl ResponseKind {
    fn message_type(self) -> u8 {
        match self {
            Self::Release => RELEASE,
            Self::RegistrationReject => REGISTRATION_REJECT,
            Self::Reorder => REORDER,
        }
    }
    fn subaddress(self) -> u8 {
        match self {
            Self::Release | Self::RegistrationReject => RELEASE_SUBADDRESS,
            Self::Reorder => REORDER_SUBADDRESS,
        }
    }
    fn extended_cause(self) -> u8 {
        match self {
            Self::Release | Self::RegistrationReject => RELEASE_CAUSE_EXTENSION,
            Self::Reorder => REORDER_CAUSE_EXTENSION,
        }
    }
    fn parameter(self, code: u8) -> Option<Parameter> {
        match code {
            REJECT_TIME if self == Self::RegistrationReject => Some(Parameter::RejectTime),
            RELEASE_SIGNAL if self == Self::Release => Some(Parameter::Signal),
            code if code == self.subaddress() => Some(Parameter::Subaddress),
            DISPLAY => Some(Parameter::Display),
            MULTILINGUAL => Some(Parameter::Multilingual),
            code if code == self.extended_cause() => Some(Parameter::CauseExtension),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Tone {
    Reorder = 0,
    Intercept = 1,
}
impl TryFrom<u8> for Tone {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Reorder),
            1 => Ok(Self::Intercept),
            _ => Err(Error::Field("tone")),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExtendedCause {
    DataPrivacyNotSupported = 0,
    DataPrivacyNotAvailable = 1,
}
impl TryFrom<u8> for ExtendedCause {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::DataPrivacyNotSupported),
            1 => Ok(Self::DataPrivacyNotAvailable),
            _ => Err(Error::Field("extended_cause")),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseOptions {
    pub reject_time: Option<crate::l3::registration::RejectTime>,
    pub signal: Option<Signal>,
    pub subaddress: Option<Subaddress>,
    pub display: Option<String>,
    pub multilingual_display: Option<MultilingualDisplay>,
    pub cause_extension: Option<ExtendedCause>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub kind: ResponseKind,
    pub cause: RejectCause,
    pub tone: Option<Tone>,
    pub options: ResponseOptions,
}
impl Response {
    pub fn registration_reject(cause: RejectCause) -> Self {
        Self {
            kind: ResponseKind::RegistrationReject,
            cause,
            tone: None,
            options: Default::default(),
        }
    }

    pub fn release(cause: RejectCause) -> Self {
        Self {
            kind: ResponseKind::Release,
            cause,
            tone: None,
            options: Default::default(),
        }
    }
    pub fn reorder(cause: RejectCause, tone: Tone) -> Self {
        Self {
            kind: ResponseKind::Reorder,
            cause,
            tone: Some(tone),
            options: Default::default(),
        }
    }
    pub fn registration_retry(
        &self,
        random_tenths: u8,
    ) -> Result<crate::l3::registration::RegistrationRetry, Error> {
        if self.kind != ResponseKind::RegistrationReject {
            return Err(Error::Field("response_kind"));
        }
        match self.options.reject_time {
            Some(time) => time.retry_after(random_tenths),
            None => Ok(crate::l3::registration::RegistrationRetry::Superframes(
                crate::l3::registration::DEFAULT_REJECT_SUPERFRAMES,
            )),
        }
    }
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), Error> {
        if (self.options.multilingual_display.is_some() || self.cause == RejectCause::Extension)
            && version.code() < ProtocolVersion::Pv1.code()
        {
            return Err(Error::Field("response_protocol_version"));
        }
        self.encode().map(|_| ())
    }
    fn validate(&self) -> Result<(), Error> {
        if self.kind == ResponseKind::Reorder
            && (self.tone.is_none() || self.options.signal.is_some())
            || self.kind != ResponseKind::Reorder && self.tone.is_some()
            || self.kind != ResponseKind::RegistrationReject && self.options.reject_time.is_some()
            || self.kind == ResponseKind::RegistrationReject && self.options.signal.is_some()
        {
            return Err(Error::Field("response_kind"));
        }
        if self.options.cause_extension.is_some() && self.cause != RejectCause::Extension {
            return Err(Error::Field("cause_extension_marker"));
        }
        Ok(())
    }
    // TIA-136-123-E §§5.3.15–17 put Cause Extension after the displays, despite its lower code.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let mut bits = Vec::new();
        put(&mut bits, u64::from(self.kind.message_type()), OCTET_BITS)?;
        put(&mut bits, self.cause as u64, CAUSE_BITS)?;
        if let Some(tone) = self.tone {
            put(&mut bits, tone as u64, TONE_BITS)?;
        }
        if let Some(time) = self.options.reject_time {
            put(&mut bits, u64::from(REJECT_TIME), PARAMETER_BITS)?;
            put(&mut bits, u64::from(time.lower()), TIME_BOUNDARY_BITS)?;
            put(&mut bits, u64::from(time.upper()), TIME_BOUNDARY_BITS)?;
        }
        if let Some(signal) = self.options.signal {
            signal.write(&mut bits, RELEASE_SIGNAL)?;
        }
        if let Some(address) = &self.options.subaddress {
            write_subaddress(&mut bits, self.kind.subaddress(), address)?;
        }
        if let Some(display) = &self.options.display {
            if !display.is_ascii() {
                return Err(Error::Field("display_encoding"));
            }
            put(&mut bits, u64::from(DISPLAY), PARAMETER_BITS)?;
            text(&mut bits, display.as_bytes())?;
        }
        if let Some(display) = &self.options.multilingual_display {
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
        if let Some(cause) = self.options.cause_extension {
            put(
                &mut bits,
                u64::from(self.kind.extended_cause()),
                PARAMETER_BITS,
            )?;
            put(&mut bits, cause as u64, OCTET_BITS)?;
        }
        bits.extend(&self.options.ignored_bits);
        bits.extend(&self.options.unparsed_bits);
        pack(bits)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let bits = control_fields::bits(bytes)?;
        let mut reader = Reader {
            bits: &bits,
            offset: 0,
        };
        let header = reader.take(OCTET_BITS)? as u8;
        if header >> TYPE_BITS != IS136_PROTOCOL {
            return Err(Error::Protocol);
        }
        let kind = match header {
            RELEASE => ResponseKind::Release,
            REGISTRATION_REJECT => ResponseKind::RegistrationReject,
            REORDER => ResponseKind::Reorder,
            _ => return Err(Error::MessageType),
        };
        let cause = RejectCause::decode(reader.take(CAUSE_BITS)? as u8)
            .map_err(|_| Error::Field("cause"))?;
        let tone = if kind == ResponseKind::Reorder {
            Some(Tone::try_from(reader.take(TONE_BITS)? as u8)?)
        } else {
            None
        };
        let mut response = Self {
            kind,
            cause,
            tone,
            options: Default::default(),
        };
        let mut previous = None;
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
            if code == EXTENSION {
                let auxiliary = reader.take(PARAMETER_BITS)? as u8;
                if matches!(auxiliary, RESERVED_PARAMETER | FUTURE_PARAMETER) {
                    return Err(Error::Field("auxiliary_reserved"));
                }
            }
            let Some(parameter) = kind.parameter(code) else {
                if reader.skip_noncritical(code)? {
                    response
                        .options
                        .ignored_bits
                        .extend_from_slice(&bits[start..reader.offset]);
                    continue;
                }
                response.options.unparsed_bits = bits[start..].to_vec();
                break;
            };
            if previous.is_some_and(|previous| parameter <= previous) {
                return Err(Error::Field("parameter_order"));
            }
            previous = Some(parameter);
            match parameter {
                Parameter::RejectTime => {
                    response.options.reject_time =
                        Some(crate::l3::registration::RejectTime::try_new(
                            reader.take(TIME_BOUNDARY_BITS)? as u8,
                            reader.take(TIME_BOUNDARY_BITS)? as u8,
                        )?);
                }
                Parameter::Signal => response.options.signal = Some(Signal::read(&mut reader)?),
                Parameter::Subaddress => {
                    response.options.subaddress = Some(read_subaddress(&mut reader)?)
                }
                Parameter::Display | Parameter::Multilingual => {
                    let count = reader.take(OCTET_BITS)? as usize;
                    let encoding = if parameter == Parameter::Multilingual {
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
                        let display = MultilingualDisplay { encoding, bytes };
                        validate_multilingual(&display)?;
                        response.options.multilingual_display = Some(display);
                    } else {
                        if !bytes.is_ascii() {
                            return Err(Error::Field("display_encoding"));
                        }
                        response.options.display = Some(
                            String::from_utf8(bytes)
                                .map_err(|_| Error::Field("display_encoding"))?,
                        );
                    }
                }
                Parameter::CauseExtension => {
                    response.options.cause_extension =
                        Some(ExtendedCause::try_from(reader.take(OCTET_BITS)? as u8)?)
                }
            }
        }
        response.validate()?;
        Ok(response)
    }
}
