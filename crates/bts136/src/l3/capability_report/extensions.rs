use super::{ProtocolVersion, ReportError};
use crate::l3::control_fields::{OCTET_BITS, PARAMETER_BITS, Reader, put};
pub(super) const EXTENSION: u8 = 7;
pub(super) const LAST_AUXILIARY: u8 = 7;
pub(super) const FIRST_AUXILIARY: u8 = 1;
const DADS: u8 = 1;
const ENCRYPTION: u8 = 2;
const ADVICE: u8 = 3;
const VOICE: u8 = 4;
const POSITIONING: u8 = 5;
const BANDS: u8 = 6;
const LANGUAGES: u8 = 7;
const DOMAIN_COUNT: usize = 8;
const ALGORITHM_BITS: usize = 4;
const PRIVACY_BITS: usize = 3;
const COUNT_BITS: usize = 3;
const CODER_BITS: usize = 3;
const MODULATION_BITS: usize = 2;
const INTERLEAVING_BITS: usize = 3;
const MAX_MODES: usize = 8;
const LANGUAGE_BITS: usize = 16;
const LANGUAGE_RESERVED_MASK: u16 = 0x0fff;
const NO_PRIVACY: u8 = 0;
const CMEA: u8 = 1;
const SCEMA: u8 = 2;
const SOC_PRIVACY: u8 = 4;
const NO_CODER: u8 = 0;
const VSELP: u8 = 1;
const ACELP_CC1: u8 = 2;
const US1: u8 = 3;
const SOC_CODER: u8 = 4;
const ACELP_CC2: u8 = 5;
const AMR: u8 = 6;
const VSELP_INTERLEAVING: u8 = 1;
const NO_PREFERENCE: u8 = 7;
const LAST_STANDARD_INTERLEAVING: u8 = 3;
const LAST_NO_CODER_INTERLEAVING: u8 = 2;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncryptionMap {
    pub algorithms: [Option<u8>; DOMAIN_COUNT],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceMode {
    pub coder: u8,
    pub modulation: u8,
    pub interleaving: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedVoice {
    pub privacy: u8,
    pub modes: Vec<VoiceMode>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportExtensions {
    pub dads: Option<bool>,
    pub encryption: Option<EncryptionMap>,
    pub advice: Option<bool>,
    pub voice: Option<ExtendedVoice>,
    pub positioning: Option<u8>,
    pub frequency_bands: Option<u8>,
    pub multilingual: Option<u16>,
}
fn version(code: u8) -> ProtocolVersion {
    match code {
        DADS => ProtocolVersion::Pv1,
        ENCRYPTION | ADVICE => ProtocolVersion::Pv4,
        VOICE | POSITIONING | BANDS => ProtocolVersion::Pv5,
        LANGUAGES => ProtocolVersion::Pv6,
        _ => unreachable!(),
    }
}
fn check_version(code: u8, reported: ProtocolVersion) -> Result<(), ReportError> {
    if reported.code() < version(code).code() {
        return Err(ReportError::Field("extension_protocol_version"));
    }
    Ok(())
}
fn header(bits: &mut Vec<bool>, code: u8, reported: ProtocolVersion) -> Result<(), ReportError> {
    check_version(code, reported)?;
    put(bits, u64::from(EXTENSION), PARAMETER_BITS)?;
    put(bits, u64::from(code), PARAMETER_BITS)
}
impl ExtendedVoice {
    fn validate(&self) -> Result<(), ReportError> {
        if self.modes.is_empty() || self.modes.len() > MAX_MODES {
            return Err(ReportError::Field("voice_mode_count"));
        }
        if !matches!(self.privacy, NO_PRIVACY | CMEA | SCEMA | SOC_PRIVACY) {
            return Err(ReportError::Field("voice_privacy"));
        }
        for mode in &self.modes {
            let valid = match mode.coder {
                NO_CODER => {
                    mode.interleaving <= LAST_NO_CODER_INTERLEAVING
                        || mode.interleaving == NO_PREFERENCE
                }
                VSELP => mode.interleaving == VSELP_INTERLEAVING,
                ACELP_CC1 | US1 | ACELP_CC2 | AMR => {
                    mode.interleaving <= LAST_STANDARD_INTERLEAVING
                        || mode.interleaving == NO_PREFERENCE
                }
                SOC_CODER => mode.interleaving < 1 << INTERLEAVING_BITS,
                _ => false,
            };
            if !valid || mode.modulation >= 1 << MODULATION_BITS {
                return Err(ReportError::Field("voice_mode"));
            }
        }
        Ok(())
    }
    pub(in crate::l3) fn write_body(&self, bits: &mut Vec<bool>) -> Result<(), ReportError> {
        self.validate()?;
        put(bits, u64::from(self.privacy), PRIVACY_BITS)?;
        put(bits, (self.modes.len() - 1) as u64, COUNT_BITS)?;
        for mode in &self.modes {
            put(bits, u64::from(mode.coder), CODER_BITS)?;
            put(bits, u64::from(mode.modulation), MODULATION_BITS)?;
            put(bits, u64::from(mode.interleaving), INTERLEAVING_BITS)?;
        }
        Ok(())
    }
    pub(in crate::l3) fn read_body(reader: &mut Reader<'_>) -> Result<Self, ReportError> {
        let privacy = reader.take(PRIVACY_BITS)? as u8;
        let count = reader.take(COUNT_BITS)? as usize + 1;
        let mut modes = Vec::with_capacity(count);
        for _ in 0..count {
            modes.push(VoiceMode {
                coder: reader.take(CODER_BITS)? as u8,
                modulation: reader.take(MODULATION_BITS)? as u8,
                interleaving: reader.take(INTERLEAVING_BITS)? as u8,
            });
        }
        let voice = Self { privacy, modes };
        voice.validate()?;
        Ok(voice)
    }
    // TIA-136-123-E §6.263 bitmap order differs from the §6.74 coder numbers.
    pub fn coder_map(&self) -> u8 {
        const VSELP_MAP: u8 = 1;
        const CC1_MAP: u8 = 2;
        const US1_MAP: u8 = 4;
        const CC2_MAP: u8 = 8;
        const AMR_MAP: u8 = 16;
        const SOC_MAP: u8 = 32;
        self.modes.iter().fold(0, |map, mode| {
            map | match mode.coder {
                VSELP => VSELP_MAP,
                ACELP_CC1 => CC1_MAP,
                US1 => US1_MAP,
                ACELP_CC2 => CC2_MAP,
                AMR => AMR_MAP,
                SOC_CODER => SOC_MAP,
                _ => 0,
            }
        })
    }
}
impl ReportExtensions {
    pub(super) fn write(
        &self,
        bits: &mut Vec<bool>,
        reported: ProtocolVersion,
    ) -> Result<(), ReportError> {
        if let Some(value) = self.dads {
            header(bits, DADS, reported)?;
            bits.push(value);
        }
        if let Some(value) = &self.encryption {
            header(bits, ENCRYPTION, reported)?;
            let domains = value
                .algorithms
                .iter()
                .enumerate()
                .fold(0u8, |map, (i, value)| {
                    map | if value.is_some() { 1 << i } else { 0 }
                });
            put(bits, u64::from(domains), DOMAIN_COUNT)?;
            // TIA-136-123-E §6.125 orders algorithm entries by the domain bitmap from left to right.
            for value in value.algorithms.iter().rev().flatten() {
                put(bits, u64::from(*value), ALGORITHM_BITS)?;
            }
        }
        if let Some(value) = self.advice {
            header(bits, ADVICE, reported)?;
            bits.push(value);
        }
        if let Some(value) = &self.voice {
            header(bits, VOICE, reported)?;
            value.write_body(bits)?;
        }
        if let Some(value) = self.positioning {
            header(bits, POSITIONING, reported)?;
            put(bits, u64::from(value), OCTET_BITS)?;
        }
        if let Some(value) = self.frequency_bands {
            header(bits, BANDS, reported)?;
            put(bits, u64::from(value), OCTET_BITS)?;
        }
        if let Some(value) = self.multilingual {
            if value & LANGUAGE_RESERVED_MASK != 0 {
                return Err(ReportError::Field("multilingual_extension_reserved"));
            }
            header(bits, LANGUAGES, reported)?;
            put(bits, u64::from(value), LANGUAGE_BITS)?;
        }
        Ok(())
    }
    pub(super) fn read(
        &mut self,
        reader: &mut Reader<'_>,
        code: u8,
        reported: ProtocolVersion,
    ) -> Result<(), ReportError> {
        check_version(code, reported)?;
        match code {
            DADS => self.dads = Some(reader.flag()?),
            ENCRYPTION => {
                let domains = reader.take(DOMAIN_COUNT)?;
                let mut map = EncryptionMap::default();
                for domain in (0..DOMAIN_COUNT).rev() {
                    if domains & (1 << domain) != 0 {
                        map.algorithms[domain] = Some(reader.take(ALGORITHM_BITS)? as u8);
                    }
                }
                self.encryption = Some(map);
            }
            ADVICE => self.advice = Some(reader.flag()?),
            VOICE => self.voice = Some(ExtendedVoice::read_body(reader)?),
            POSITIONING => self.positioning = Some(reader.take(OCTET_BITS)? as u8),
            BANDS => self.frequency_bands = Some(reader.take(OCTET_BITS)? as u8),
            LANGUAGES => {
                let value = reader.take(LANGUAGE_BITS)? as u16;
                if value & LANGUAGE_RESERVED_MASK != 0 {
                    return Err(ReportError::Field("multilingual_extension_reserved"));
                }
                self.multilingual = Some(value);
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
