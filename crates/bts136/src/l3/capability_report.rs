mod bands;
mod extensions;
pub use super::control_fields::Error as ReportError;
use super::control_fields::{self, Reader, put, read_subaddress, write_subaddress};
use super::{broadcast::ProtocolVersion, legacy_traffic::Subaddress};
pub use extensions::{EncryptionMap, ExtendedVoice, ReportExtensions, VoiceMode};
pub const MESSAGE_TYPE: u8 = 5;
pub const REQUEST_TYPE: u8 = 20;
// TIA-136-123-E §5.3.5 permits Capability Request without an optional subaddress.
pub fn request() -> [u8; 1] {
    [REQUEST_TYPE]
}
#[cfg(test)]
const MAX_REPORT_OCTETS: usize = control_fields::MAX_MESSAGE_OCTETS;
const TYPE_BITS: usize = 6;
const PROTOCOL_BITS: usize = 2;
const VERSION_BITS: usize = 4;
const SCM_BITS: usize = 5;
const VINTAGE_BITS: usize = 6;
const MODEL_BITS: usize = 4;
const PFC_BITS: usize = 3;
const OCTET_BITS: usize = 8;
const PARAMETER_BITS: usize = 4;
const SUBADDRESS: u8 = 1;
const VOICE_CODERS: u8 = 2;
const ALT_SOC: u8 = 3;
const MODEL: u8 = 4;
const CHARACTERS: u8 = 5;
const ARQ: u8 = 6;
const FLAG_BITS: usize = 1;
const CODER_MAP_BITS: usize = 6;
const SOC_BITS: usize = 12;
const CHARACTER_MAP_BITS: usize = 12;
const IS136_PROTOCOL: u64 = 0;
#[cfg(test)]
const MAX_SUBADDRESS_OCTETS: usize = 20;
const VSELP_MAP: u8 = 1;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SupportedServices {
    pub soc: bool,
    pub bsmc: bool,
    pub async_data: bool,
    pub g3_fax: bool,
    pub sms_broadcast: bool,
    pub subaddressing: bool,
    pub ira: bool,
    pub user_group: bool,
    pub analog_800_speech: bool,
    pub half_rate_dtc: bool,
    pub double_rate_dtc: bool,
    pub triple_rate_dtc: bool,
    pub stu_iii: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportOptions {
    pub subaddress: Option<Subaddress>,
    pub voice_coder_map: Option<u8>,
    pub alternate_soc: Option<u16>,
    pub complete_model: Option<u8>,
    pub character_map: Option<u16>,
    pub facch_sacch_arq: Option<bool>,
    pub extensions: ReportExtensions,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityReport {
    pub protocol_version: ProtocolVersion,
    pub scm: u8,
    pub software_vintage: u8,
    pub firmware_vintage: u8,
    pub model: u8,
    pub manufacturer: u8,
    /// TIA-136-123-E §6.120 encodes PFC1–PFC8 as 0–7.
    pub max_supported_pfc: u8,
    pub supported_bands: u8,
    pub services: SupportedServices,
    pub options: ReportOptions,
}
impl CapabilityReport {
    pub fn voice_coder_map(&self) -> Option<u8> {
        self.options
            .voice_coder_map
            .or_else(|| {
                self.options
                    .extensions
                    .voice
                    .as_ref()
                    .map(ExtendedVoice::coder_map)
            })
            .or_else(|| self.options.unparsed_bits.is_empty().then_some(VSELP_MAP))
    }
    // TIA-136-123-E §5.4.5 fixes the mandatory prefix independently of traffic Capability Update.
    pub fn encode(&self) -> Result<Vec<u8>, ReportError> {
        if self.options.voice_coder_map.is_some() && self.options.extensions.voice.is_some() {
            return Err(ReportError::Field("voice_coder_exclusive"));
        }
        let mut bits = Vec::new();
        for (value, width) in [
            (IS136_PROTOCOL, PROTOCOL_BITS),
            (u64::from(MESSAGE_TYPE), TYPE_BITS),
            (u64::from(self.protocol_version.code()), VERSION_BITS),
            (u64::from(self.scm), SCM_BITS),
            (u64::from(self.software_vintage), VINTAGE_BITS),
            (u64::from(self.firmware_vintage), VINTAGE_BITS),
            (u64::from(self.model), MODEL_BITS),
            (u64::from(self.manufacturer), OCTET_BITS),
            (u64::from(self.max_supported_pfc), PFC_BITS),
        ] {
            put(&mut bits, value, width)?;
        }
        let support = &self.services;
        for flag in [
            support.soc,
            support.bsmc,
            support.async_data,
            support.g3_fax,
            support.sms_broadcast,
            support.subaddressing,
        ] {
            bits.push(flag);
        }
        put(&mut bits, u64::from(self.supported_bands), OCTET_BITS)?;
        for flag in [
            support.ira,
            support.user_group,
            support.analog_800_speech,
            support.half_rate_dtc,
            support.double_rate_dtc,
            support.triple_rate_dtc,
        ] {
            bits.push(flag);
        }
        if self.protocol_version.code() >= ProtocolVersion::Is136RevA.code() {
            bits.push(support.stu_iii);
        } else if support.stu_iii {
            return Err(ReportError::Field("stu_iii_protocol_version"));
        }
        if self.protocol_version.code() < ProtocolVersion::Is136RevA.code()
            && (self.options.subaddress.is_some()
                || self.options.voice_coder_map.is_some()
                || self.options.alternate_soc.is_some())
        {
            return Err(ReportError::Field("option_protocol_version"));
        }
        if let Some(subaddress) = &self.options.subaddress {
            write_subaddress(&mut bits, SUBADDRESS, subaddress)?;
        }
        if self.services.soc && self.options.alternate_soc.is_some() {
            return Err(ReportError::Field("alternate_soc"));
        }
        for (parameter, value, width) in [
            (
                VOICE_CODERS,
                self.options.voice_coder_map.map(u64::from),
                CODER_MAP_BITS,
            ),
            (ALT_SOC, self.options.alternate_soc.map(u64::from), SOC_BITS),
            (
                MODEL,
                self.options.complete_model.map(u64::from),
                OCTET_BITS,
            ),
            (
                CHARACTERS,
                self.options.character_map.map(u64::from),
                CHARACTER_MAP_BITS,
            ),
            (ARQ, self.options.facch_sacch_arq.map(u64::from), FLAG_BITS),
        ] {
            if let Some(value) = value {
                if parameter >= MODEL && self.protocol_version.code() < ProtocolVersion::Pv1.code()
                {
                    return Err(ReportError::Field("option_protocol_version"));
                }
                put(&mut bits, u64::from(parameter), PARAMETER_BITS)?;
                put(&mut bits, value, width)?;
            }
        }
        self.options
            .extensions
            .write(&mut bits, self.protocol_version)?;
        bits.extend(&self.options.ignored_bits);
        bits.extend(&self.options.unparsed_bits);
        control_fields::pack(bits)
    }
}
pub fn decode(bytes: &[u8]) -> Result<CapabilityReport, ReportError> {
    let bits = control_fields::bits(bytes)?;
    let mut r = Reader {
        bits: &bits,
        offset: 0,
    };
    if r.take(PROTOCOL_BITS)? != IS136_PROTOCOL {
        return Err(ReportError::Protocol);
    }
    if r.take(TYPE_BITS)? as u8 != MESSAGE_TYPE {
        return Err(ReportError::MessageType);
    }
    let protocol_version = ProtocolVersion::decode(r.take(VERSION_BITS)? as u8)
        .map_err(|_| ReportError::Field("protocol_version"))?;
    let scm = r.take(SCM_BITS)? as u8;
    let software_vintage = r.take(VINTAGE_BITS)? as u8;
    let firmware_vintage = r.take(VINTAGE_BITS)? as u8;
    let model = r.take(MODEL_BITS)? as u8;
    let manufacturer = r.take(OCTET_BITS)? as u8;
    let max_supported_pfc = r.take(PFC_BITS)? as u8;
    let mut services = SupportedServices {
        soc: r.flag()?,
        bsmc: r.flag()?,
        async_data: r.flag()?,
        g3_fax: r.flag()?,
        sms_broadcast: r.flag()?,
        subaddressing: r.flag()?,
        ..Default::default()
    };
    let supported_bands = r.take(OCTET_BITS)? as u8;
    services.ira = r.flag()?;
    services.user_group = r.flag()?;
    services.analog_800_speech = r.flag()?;
    services.half_rate_dtc = r.flag()?;
    services.double_rate_dtc = r.flag()?;
    services.triple_rate_dtc = r.flag()?;
    if protocol_version.code() >= ProtocolVersion::Is136RevA.code() {
        services.stu_iii = r.flag()?;
    }
    let mut options = ReportOptions::default();
    let mut previous = 0;
    while r.bits.len() - r.offset >= PARAMETER_BITS {
        if r.bits.len() - r.offset < OCTET_BITS && r.bits[r.offset..].iter().all(|bit| !*bit) {
            r.offset = r.bits.len();
            break;
        }
        let start = r.offset;
        let parameter = r.take(PARAMETER_BITS)? as u8;
        let auxiliary = if parameter == extensions::EXTENSION {
            Some(r.take(PARAMETER_BITS)? as u8)
        } else {
            None
        };
        if !(SUBADDRESS..=extensions::EXTENSION).contains(&parameter)
            || auxiliary.is_some_and(|code| {
                !(extensions::FIRST_AUXILIARY..=extensions::LAST_AUXILIARY).contains(&code)
            })
            || protocol_version.code() < ProtocolVersion::Is136RevA.code()
            || ((MODEL..=ARQ).contains(&parameter)
                && protocol_version.code() < ProtocolVersion::Pv1.code())
        {
            if r.skip_noncritical(parameter)? {
                options
                    .ignored_bits
                    .extend_from_slice(&r.bits[start..r.offset]);
                continue;
            }
            options.unparsed_bits = r.bits[start..].to_vec();
            r.offset = r.bits.len();
            break;
        }
        let order =
            u16::from(parameter) * (1 << PARAMETER_BITS) + u16::from(auxiliary.unwrap_or(0));
        if order <= previous {
            return Err(ReportError::Field("parameter_order"));
        }
        previous = order;
        if let Some(auxiliary) = auxiliary {
            options
                .extensions
                .read(&mut r, auxiliary, protocol_version)?;
            continue;
        }
        match parameter {
            SUBADDRESS => {
                options.subaddress = Some(read_subaddress(&mut r)?);
            }
            VOICE_CODERS => options.voice_coder_map = Some(r.take(CODER_MAP_BITS)? as u8),
            ALT_SOC => options.alternate_soc = Some(r.take(SOC_BITS)? as u16),
            MODEL => options.complete_model = Some(r.take(OCTET_BITS)? as u8),
            CHARACTERS => options.character_map = Some(r.take(CHARACTER_MAP_BITS)? as u16),
            ARQ => options.facch_sacch_arq = Some(r.flag()?),
            _ => unreachable!(),
        }
    }
    if r.bits[r.offset..].iter().any(|bit| *bit) {
        return Err(ReportError::Field("padding"));
    }
    if options.voice_coder_map.is_some() && options.extensions.voice.is_some() {
        return Err(ReportError::Field("voice_coder_exclusive"));
    }
    if services.soc && options.alternate_soc.is_some() {
        return Err(ReportError::Field("alternate_soc"));
    }
    Ok(CapabilityReport {
        protocol_version,
        scm,
        software_vintage,
        firmware_vintage,
        model,
        manufacturer,
        max_supported_pfc,
        supported_bands,
        services,
        options,
    })
}
#[cfg(test)]
mod tests;
