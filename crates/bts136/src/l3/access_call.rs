pub use super::control_fields::Error as OptionsError;
use super::{
    address::{self, AddressInfo},
    broadcast::ProtocolVersion,
    capability_report::ExtendedVoice,
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, put, read_subaddress, write_subaddress,
    },
    legacy_traffic::Subaddress,
    origination::{self, Origination},
    page_response::{self, PageResponse, ServiceCode},
    registration::{EncryptionMode, RejectCause, SerialNumber},
};
use std::{error::Error, fmt};
pub const ORIGINATION_TYPE: u8 = 7;
pub const PAGE_RESPONSE_TYPE: u8 = 8;
const ORIG_CALLED_RANK: u16 = 7;
const ORIG_CALLING_RANK: u16 = 8;
const CMEA: u8 = 1;
const SCEMA: u8 = 2;
const SOC_PRIVACY: u8 = 4;
const VOICE: u8 = 1;
const DATA: u8 = 2;
const ENCRYPTION: u8 = 3;
const BANDWIDTH: u8 = 4;
const PRESENTATION_OR_GROUP: u8 = 5;
const NUMBER_OR_SUBADDRESS: u8 = 6;
const EXTENSION: u8 = 7;
const CALLED_SUBADDRESS: u8 = 8;
const CALLING_SUBADDRESS: u8 = 9;
const FIELD_BITS: usize = 3;
const BANDWIDTH_BITS: usize = 3;
const PRESENTATION_BITS: usize = 2;
const DATA_BITS: usize = 12;
const SYSTEM_BITS: usize = 17;
const NO_VOICE_CODER: u8 = 0;
const LEGACY_LAST_CODER: u8 = 5;
const NO_PRIVACY: u8 = 0;
const CLEAR_ALGORITHM: u8 = 0;
const DOMAIN_A: u8 = 1;
const KEY_A: u8 = 1;
const PI4_DQPSK: u8 = 0;
const NO_MODULATION_PREFERENCE: u8 = 3;
const PRESENTATION_ALLOWED: u8 = 0;
const PRESENTATION_RESERVED: u8 = 3;
const NO_INTERLEAVING_PREFERENCE: u8 = 7;
const DEFAULT_FULL_RATE_INTERLEAVING: u8 = 1;
const SERIAL: u8 = 13;
const RESERVED_PARAMETER: u8 = 0;
const FUTURE_PARAMETER: u8 = 15;
const GROUP_STATUS_BITS: usize = 2;
const GROUP_KIND_BITS: usize = 2;
const GROUP_WIDTHS: [usize; 4] = [20, 24, 34, 50];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Origination,
    PageResponse,
}
impl Kind {
    fn header(self) -> u8 {
        match self {
            Self::Origination => ORIGINATION_TYPE,
            Self::PageResponse => PAGE_RESPONSE_TYPE,
        }
    }
    // TIA-136-123-E §5 requires format-table order, which differs from parameter-code order in Origination.
    fn order(self, code: u8, aux: Option<u8>) -> u16 {
        match (self, code) {
            (Self::Origination, CALLED_SUBADDRESS) => ORIG_CALLED_RANK,
            (Self::Origination, CALLING_SUBADDRESS) => ORIG_CALLING_RANK,
            (Self::Origination, EXTENSION) => ORIG_CALLING_RANK + u16::from(aux.unwrap_or(0)),
            (Self::PageResponse, EXTENSION) => {
                u16::from(NUMBER_OR_SUBADDRESS) + u16::from(aux.unwrap_or(0))
            }
            _ => u16::from(code),
        }
    }
    fn extended_voice(self) -> u8 {
        match self {
            Self::Origination => 3,
            Self::PageResponse => 1,
        }
    }
    fn system(self) -> u8 {
        match self {
            Self::Origination => 4,
            Self::PageResponse => 2,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Bandwidth {
    HalfOnly = 0,
    FullOnly = 1,
    FullPreferred = 2,
    HalfPreferred = 3,
    DoubleOnly = 4,
    TripleOnly = 5,
    DoublePreferred = 6,
    TriplePreferred = 7,
}
impl Bandwidth {
    fn decode(value: u8) -> Result<Self, OptionsError> {
        match value {
            0 => Ok(Self::HalfOnly),
            1 => Ok(Self::FullOnly),
            2 => Ok(Self::FullPreferred),
            3 => Ok(Self::HalfPreferred),
            4 => Ok(Self::DoubleOnly),
            5 => Ok(Self::TripleOnly),
            6 => Ok(Self::DoublePreferred),
            7 => Ok(Self::TriplePreferred),
            _ => Err(OptionsError::Field("bandwidth")),
        }
    }
    fn supports_full(self) -> bool {
        matches!(
            self,
            Self::FullOnly
                | Self::FullPreferred
                | Self::HalfPreferred
                | Self::DoublePreferred
                | Self::TriplePreferred
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyVoice {
    pub coder: u8,
    pub privacy: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presentation {
    pub presentation: u8,
    pub screening: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserGroup {
    pub kind: u8,
    pub value: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallOptions {
    pub voices: Vec<LegacyVoice>,
    pub data_mode: Option<u16>,
    pub encryption: Option<EncryptionMode>,
    pub bandwidth: Option<Bandwidth>,
    pub presentation: Option<Presentation>,
    pub calling_party: Option<AddressInfo>,
    pub called_subaddress: Option<Subaddress>,
    pub calling_subaddress: Option<Subaddress>,
    pub group: Option<UserGroup>,
    pub extended_voices: Vec<ExtendedVoice>,
    pub selected_system: Option<u32>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    Codec,
    Encryption,
    Bandwidth,
    Addressing,
    UserGroup,
    Service,
}
impl AdmissionError {
    pub fn reject_cause(self) -> RejectCause {
        match self {
            Self::Codec => RejectCause::VoiceCodecNotAvailable,
            Self::Encryption => RejectCause::EncryptionModeTemporarilyDenied,
            Self::Bandwidth => RejectCause::ServiceCodeNotAvailable,
            Self::Addressing => RejectCause::AddressingNotSupported,
            Self::UserGroup => RejectCause::UserGroupBarred,
            Self::Service => RejectCause::ServiceCodeNotSupported,
        }
    }
}
impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported call request: {self:?}")
    }
}
impl Error for AdmissionError {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginationRequest {
    pub message: Origination,
    pub options: CallOptions,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageResponseRequest {
    pub message: PageResponse,
    pub options: CallOptions,
}
fn parameter(bits: &mut Vec<bool>, value: u8) -> Result<(), OptionsError> {
    put(bits, u64::from(value), PARAMETER_BITS)
}
fn auxiliary(bits: &mut Vec<bool>, value: u8) -> Result<(), OptionsError> {
    parameter(bits, EXTENSION)?;
    parameter(bits, value)
}
fn address_read(r: &mut Reader<'_>) -> Result<AddressInfo, OptionsError> {
    let count = r.take(OCTET_BITS)? as usize;
    let mut bytes = vec![count as u8];
    for _ in 0..count {
        bytes.push(r.take(OCTET_BITS)? as u8);
    }
    address::decode_prefix(&bytes)
        .map(|value| value.address)
        .map_err(|_| OptionsError::Field("calling_party"))
}
fn address_write(bits: &mut Vec<bool>, value: &AddressInfo) -> Result<(), OptionsError> {
    for byte in value
        .encode()
        .map_err(|_| OptionsError::Field("calling_party"))?
    {
        put(bits, u64::from(byte), OCTET_BITS)?;
    }
    Ok(())
}
fn version_at_least(actual: ProtocolVersion, minimum: ProtocolVersion) -> Result<(), OptionsError> {
    if actual.code() < minimum.code() {
        Err(OptionsError::Field("call_option_version"))
    } else {
        Ok(())
    }
}
impl CallOptions {
    fn validate(
        &self,
        kind: Kind,
        version: ProtocolVersion,
        service: ServiceCode,
    ) -> Result<(), OptionsError> {
        if !self.voices.is_empty() && !self.extended_voices.is_empty() {
            return Err(OptionsError::Field("voice_exclusive"));
        }
        if !self.extended_voices.is_empty() {
            version_at_least(version, ProtocolVersion::Pv3)?;
        }
        if self.selected_system.is_some() {
            version_at_least(version, ProtocolVersion::Pv4)?;
        }
        if self.bandwidth.is_some_and(|value| {
            matches!(
                value,
                Bandwidth::DoublePreferred | Bandwidth::TriplePreferred
            )
        }) {
            version_at_least(version, ProtocolVersion::Pv1)?;
        }
        for voice in &self.voices {
            if voice.coder > LEGACY_LAST_CODER
                || !matches!(voice.privacy, NO_PRIVACY | CMEA | SCEMA | SOC_PRIVACY)
            {
                return Err(OptionsError::Field("voice_mode"));
            }
        }
        if self.presentation.is_some_and(|value| {
            value.presentation >= PRESENTATION_RESERVED || value.screening >= 1 << PRESENTATION_BITS
        }) {
            return Err(OptionsError::Field("presentation"));
        }
        // TIA-136-123-E §6.110 assigns VC000 to DADS and WORT.
        if matches!(service, ServiceCode::DirectAsyncData | ServiceCode::Wort)
            && self
                .voices
                .iter()
                .any(|voice| voice.coder != NO_VOICE_CODER)
        {
            return Err(OptionsError::Field("voice_mode_context"));
        }
        let data = matches!(service, ServiceCode::AsyncData | ServiceCode::G3Fax);
        let speech = matches!(
            service,
            ServiceCode::AnalogSpeech
                | ServiceCode::DigitalSpeech
                | ServiceCode::AnalogPreferred
                | ServiceCode::DigitalPreferred
        );
        if data && self.data_mode.is_none() || speech && self.data_mode.is_some() {
            return Err(OptionsError::Field("data_mode_context"));
        }
        // TIA-136-123-E §§5.4.7/8 requires both Data Mode and Bandwidth for Async Data and G3 Fax.
        if data && self.bandwidth.is_none() {
            return Err(OptionsError::Field("bandwidth_context"));
        }
        if (!self.voices.is_empty() || !self.extended_voices.is_empty())
            && (data || service == ServiceCode::AnalogSpeech)
        {
            return Err(OptionsError::Field("voice_mode_context"));
        }
        if self.bandwidth.is_some() && service == ServiceCode::AnalogSpeech {
            return Err(OptionsError::Field("bandwidth_context"));
        }
        if kind == Kind::Origination && self.group.is_some() {
            return Err(OptionsError::Field("origination_group"));
        }
        if kind == Kind::PageResponse
            && (self.presentation.is_some()
                || self.calling_party.is_some()
                || self.calling_subaddress.is_some())
        {
            return Err(OptionsError::Field("page_response_addressing"));
        }
        Ok(())
    }
    pub fn select_baseline(&self) -> Result<(), AdmissionError> {
        self.select_codec(&[crate::codec::speech::Codec::Vselp])
            .map(|_| ())
    }
    pub fn select_codec(
        &self,
        supported: &[crate::codec::speech::Codec],
    ) -> Result<crate::codec::speech::Codec, AdmissionError> {
        use crate::codec::speech::Codec;
        if self.group.is_some() {
            return Err(AdmissionError::UserGroup);
        }
        if self.selected_system.is_some()
            || self.called_subaddress.is_some()
            || self.calling_subaddress.is_some()
            || self
                .presentation
                .is_some_and(|value| value.presentation != PRESENTATION_ALLOWED)
        {
            return Err(AdmissionError::Addressing);
        }
        if self.data_mode.is_some() {
            return Err(AdmissionError::Service);
        }
        if self.encryption.is_some_and(|value| {
            value.algorithm != CLEAR_ALGORITHM || value.domain != DOMAIN_A || value.key != KEY_A
        }) {
            return Err(AdmissionError::Encryption);
        }
        if !self
            .bandwidth
            .unwrap_or(Bandwidth::FullOnly)
            .supports_full()
        {
            return Err(AdmissionError::Bandwidth);
        }
        if self.voices.is_empty() && self.extended_voices.is_empty() {
            return supported
                .contains(&Codec::Vselp)
                .then_some(Codec::Vselp)
                .ok_or(AdmissionError::Codec);
        }
        let mut private = false;
        for voice in &self.voices {
            if let Some(&codec) = supported.iter().find(|codec| codec.coder() == voice.coder) {
                if voice.privacy == NO_PRIVACY {
                    return Ok(codec);
                }
                private = true;
            }
        }
        for list in &self.extended_voices {
            for mode in &list.modes {
                if let Some(&codec) = supported.iter().find(|codec| codec.coder() == mode.coder) {
                    let interleaving = mode.interleaving == DEFAULT_FULL_RATE_INTERLEAVING
                        || (codec == Codec::AcelpCc1
                            && mode.interleaving == NO_INTERLEAVING_PREFERENCE);
                    if matches!(mode.modulation, PI4_DQPSK | NO_MODULATION_PREFERENCE)
                        && interleaving
                    {
                        if list.privacy == NO_PRIVACY {
                            return Ok(codec);
                        }
                        private = true;
                    }
                }
            }
        }
        Err(if private {
            AdmissionError::Encryption
        } else {
            AdmissionError::Codec
        })
    }
    fn write(
        &self,
        bits: &mut Vec<bool>,
        kind: Kind,
        version: ProtocolVersion,
        service: ServiceCode,
    ) -> Result<(), OptionsError> {
        self.validate(kind, version, service)?;
        for voice in &self.voices {
            parameter(bits, VOICE)?;
            put(bits, u64::from(voice.coder), FIELD_BITS)?;
            put(bits, u64::from(voice.privacy), FIELD_BITS)?;
        }
        if let Some(value) = self.data_mode {
            parameter(bits, DATA)?;
            put(bits, u64::from(value), DATA_BITS)?;
        }
        if let Some(value) = self.encryption {
            parameter(bits, ENCRYPTION)?;
            for value in [value.algorithm, value.domain, value.key] {
                put(bits, u64::from(value), FIELD_BITS)?;
            }
        }
        if let Some(value) = self.bandwidth {
            parameter(bits, BANDWIDTH)?;
            put(bits, value as u64, BANDWIDTH_BITS)?;
        }
        if let Some(value) = self.presentation {
            parameter(bits, PRESENTATION_OR_GROUP)?;
            put(bits, u64::from(value.presentation), PRESENTATION_BITS)?;
            put(bits, u64::from(value.screening), PRESENTATION_BITS)?;
        }
        if let Some(value) = self.group {
            parameter(bits, PRESENTATION_OR_GROUP)?;
            put(bits, 0, GROUP_STATUS_BITS)?;
            put(bits, u64::from(value.kind), GROUP_KIND_BITS)?;
            let width = *GROUP_WIDTHS
                .get(usize::from(value.kind))
                .ok_or(OptionsError::Field("group_kind"))?;
            put(bits, value.value, width)?;
        }
        if let Some(value) = &self.calling_party {
            parameter(bits, NUMBER_OR_SUBADDRESS)?;
            address_write(bits, value)?;
        }
        if kind == Kind::PageResponse
            && let Some(value) = &self.called_subaddress
        {
            write_subaddress(bits, NUMBER_OR_SUBADDRESS, value)?;
        }
        if kind == Kind::Origination
            && let Some(value) = &self.called_subaddress
        {
            write_subaddress(bits, CALLED_SUBADDRESS, value)?;
        }
        if let Some(value) = &self.calling_subaddress {
            write_subaddress(bits, CALLING_SUBADDRESS, value)?;
        }
        for voice in &self.extended_voices {
            auxiliary(bits, kind.extended_voice())?;
            voice.write_body(bits)?;
        }
        if let Some(value) = self.selected_system {
            auxiliary(bits, kind.system())?;
            put(bits, u64::from(value), SYSTEM_BITS)?;
        }
        bits.extend(&self.ignored_bits);
        bits.extend(&self.unparsed_bits);
        Ok(())
    }
    fn read(
        bits: &[bool],
        offset: usize,
        kind: Kind,
        version: ProtocolVersion,
        service: ServiceCode,
    ) -> Result<Self, OptionsError> {
        let mut r = Reader { bits, offset };
        let mut result = Self::default();
        let mut previous = 0;
        while r.offset < r.bits.len() {
            if r.bits.len() - r.offset < OCTET_BITS && r.bits[r.offset..].iter().all(|bit| !*bit) {
                break;
            }
            let start = r.offset;
            let code = r.take(PARAMETER_BITS)? as u8;
            let aux = if code == EXTENSION {
                Some(r.take(PARAMETER_BITS)? as u8)
            } else {
                None
            };
            let known = (VOICE..=NUMBER_OR_SUBADDRESS).contains(&code)
                || (kind == Kind::Origination
                    && matches!(code, CALLED_SUBADDRESS | CALLING_SUBADDRESS))
                || aux.is_some_and(|aux| aux == kind.extended_voice() || aux == kind.system());
            if code == RESERVED_PARAMETER
                || code == FUTURE_PARAMETER
                || aux.is_some_and(|aux| aux == RESERVED_PARAMETER || aux == FUTURE_PARAMETER)
            {
                return Err(OptionsError::Field("optional_reserved"));
            }
            if !known {
                // TIA-136-123-E §5 skips noncritical IEs by length and accepts the prefix before unknown critical IEs.
                if r.skip_noncritical(code)? {
                    result
                        .ignored_bits
                        .extend_from_slice(&r.bits[start..r.offset]);
                    continue;
                }
                result.unparsed_bits = r.bits[start..].to_vec();
                break;
            }
            let order = kind.order(code, aux);
            let repeat = code == VOICE || aux == Some(kind.extended_voice());
            if order < previous || order == previous && !repeat {
                return Err(OptionsError::Field("parameter_order"));
            }
            previous = order;
            match (code, aux) {
                (VOICE, _) => result.voices.push(LegacyVoice {
                    coder: r.take(FIELD_BITS)? as u8,
                    privacy: r.take(FIELD_BITS)? as u8,
                }),
                (DATA, _) => result.data_mode = Some(r.take(DATA_BITS)? as u16),
                (ENCRYPTION, _) => {
                    result.encryption = Some(EncryptionMode {
                        algorithm: r.take(FIELD_BITS)? as u8,
                        domain: r.take(FIELD_BITS)? as u8,
                        key: r.take(FIELD_BITS)? as u8,
                    })
                }
                (BANDWIDTH, _) => {
                    result.bandwidth = Some(Bandwidth::decode(r.take(BANDWIDTH_BITS)? as u8)?)
                }
                (PRESENTATION_OR_GROUP, _) if kind == Kind::Origination => {
                    result.presentation = Some(Presentation {
                        presentation: r.take(PRESENTATION_BITS)? as u8,
                        screening: r.take(PRESENTATION_BITS)? as u8,
                    })
                }
                (PRESENTATION_OR_GROUP, _) => {
                    if r.take(GROUP_STATUS_BITS)? != 0 {
                        return Err(OptionsError::Field("group_status"));
                    }
                    let kind = r.take(GROUP_KIND_BITS)? as u8;
                    result.group = Some(UserGroup {
                        kind,
                        value: r.take(GROUP_WIDTHS[usize::from(kind)])?,
                    });
                }
                (NUMBER_OR_SUBADDRESS, _) if kind == Kind::Origination => {
                    result.calling_party = Some(address_read(&mut r)?)
                }
                (NUMBER_OR_SUBADDRESS, _) | (CALLED_SUBADDRESS, _) => {
                    result.called_subaddress = Some(read_subaddress(&mut r)?)
                }
                (CALLING_SUBADDRESS, _) => {
                    result.calling_subaddress = Some(read_subaddress(&mut r)?)
                }
                (EXTENSION, Some(aux)) if aux == kind.extended_voice() => result
                    .extended_voices
                    .push(ExtendedVoice::read_body(&mut r)?),
                (EXTENSION, _) => result.selected_system = Some(r.take(SYSTEM_BITS)? as u32),
                _ => unreachable!(),
            }
        }
        result.validate(kind, version, service)?;
        Ok(result)
    }
}
impl OriginationRequest {
    pub fn encode(&self) -> Result<Vec<u8>, OptionsError> {
        let bytes = self
            .message
            .encode_mandatory()
            .map_err(|_| OptionsError::Field("origination"))?;
        let decoded =
            origination::decode_prefix(&bytes).map_err(|_| OptionsError::Field("origination"))?;
        let mut bits = control_fields::bits(&bytes)?;
        bits.truncate(decoded.consumed_bits);
        self.options.write(
            &mut bits,
            Kind::Origination,
            self.message.protocol_version,
            self.message.service_code,
        )?;
        control_fields::pack(bits)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, OptionsError> {
        let bits = control_fields::bits(bytes)?;
        let decoded =
            origination::decode_prefix(bytes).map_err(|_| OptionsError::Field("origination"))?;
        let options = CallOptions::read(
            &bits,
            decoded.consumed_bits,
            Kind::Origination,
            decoded.message.protocol_version,
            decoded.message.service_code,
        )?;
        Ok(Self {
            message: decoded.message,
            options,
        })
    }
}
impl PageResponseRequest {
    pub fn encode(&self) -> Result<Vec<u8>, OptionsError> {
        let bytes = self
            .message
            .encode_mandatory()
            .map_err(|_| OptionsError::Field("page_response"))?;
        let decoded = page_response::decode_prefix(&bytes)
            .map_err(|_| OptionsError::Field("page_response"))?;
        let mut bits = control_fields::bits(&bytes)?;
        bits.truncate(decoded.consumed_bits);
        self.options.write(
            &mut bits,
            Kind::PageResponse,
            self.message.protocol_version,
            self.message.service_code,
        )?;
        control_fields::pack(bits)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, OptionsError> {
        let bits = control_fields::bits(bytes)?;
        let decoded = page_response::decode_prefix(bytes)
            .map_err(|_| OptionsError::Field("page_response"))?;
        let options = CallOptions::read(
            &bits,
            decoded.consumed_bits,
            Kind::PageResponse,
            decoded.message.protocol_version,
            decoded.message.service_code,
        )?;
        Ok(Self {
            message: decoded.message,
            options,
        })
    }
}
pub fn validate_companions(
    set: &crate::link::rach::MessageSet,
    kind: Kind,
    version: ProtocolVersion,
) -> Result<(), OptionsError> {
    if set.messages.first().and_then(|bytes| bytes.first()) != Some(&kind.header()) {
        return Err(OptionsError::Field("call_primary_order"));
    }
    let mut serial = false;
    for bytes in set.messages.iter().skip(1) {
        if bytes.first() != Some(&SERIAL) || serial {
            return Err(OptionsError::Field("call_companion"));
        }
        let value = SerialNumber::decode(bytes)?;
        value.validate_version(version)?;
        serial = true;
    }
    Ok(())
}
#[cfg(test)]
mod tests;
