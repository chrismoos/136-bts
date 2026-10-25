use super::*;

pub(super) const PV_BITS: usize = 4;
const PRIVACY_BITS: usize = 4;
const CODER_BITS: usize = 6;
const KEY_BITS: usize = 4;
const SOC_BITS: usize = 12;
const SMS_BITS: usize = 2;
const ALGORITHM_BITS: usize = 4;
const SCM_BITS: usize = 5;
const VINTAGE_BITS: usize = 6;
const PFC_BITS: usize = 3;
const NSAP_SUBADDRESS: u8 = 0;
const USER_SPECIFIED_SUBADDRESS: u8 = 2;
const SUBADDRESS_RESERVED_BITS: usize = 3;
const SUBADDRESS_KIND_BITS: usize = 3;
const FLAG_BITS: usize = 1;
const MAX_SUBADDRESS_CONTENT_OCTETS: usize = 21;
const EXTENDED_CHARACTER_FLAGS: usize = 13;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityFields {
    pub protocol_version: ProtocolVersion,
    pub voice_privacy_map: u8,
    pub voice_coder_map: u8,
    pub encryption_key_map: u8,
    pub facch_sacch_arq: bool,
    pub soc: u16,
    pub bsmc: u8,
    pub sms_map: u8,
    pub ira: bool,
    pub encryption_algorithms: [Option<u8>; OCTET_BITS],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharacterSupport {
    pub latin1: bool,
    pub bmp: bool,
    pub latin_hebrew: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StationPv1 {
    pub async_data: bool,
    pub g3_fax: bool,
    pub dads: bool,
    pub user_group: bool,
    pub oats: bool,
    pub characters: CharacterSupport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseStationCapabilities {
    pub common: CapabilityFields,
    pub pv1: Option<StationPv1>,
    pub extended_bands: Option<u8>,
    /// TIA-136-133-E §9.1.2.2.23 orders ISO-8859 parts 2–7 and 9–15.
    pub extended_characters: Option<[bool; EXTENDED_CHARACTER_FLAGS]>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subaddress {
    pub odd: bool,
    pub kind: u8,
    pub address: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MobilePv1 {
    pub scm: u8,
    pub software_vintage: u8,
    pub firmware_vintage: u8,
    pub model: u8,
    pub manufacturer: u8,
    /// TIA-136-123-E §6.120 encodes PFC1–PFC8 as 0–7.
    pub max_supported_pfc: u8,
    pub async_data: bool,
    pub g3_fax: bool,
    pub dads: bool,
    pub subaddress: Option<Subaddress>,
    pub user_group: bool,
    pub half_rate_dtc: bool,
    pub double_rate_dtc: bool,
    pub triple_rate_dtc: bool,
    pub stu_iii: bool,
    pub characters: CharacterSupport,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MobilePv5 {
    pub positioning_map: u8,
    pub extended_bands: u8,
    pub modulation_map: u8,
    pub interleaving_map: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileCapabilities {
    pub common: CapabilityFields,
    pub supported_bands: u8,
    pub analog_800_speech: bool,
    pub pv1: Option<MobilePv1>,
    pub advice: Option<bool>,
    pub pv5: Option<MobilePv5>,
    /// TIA-136-133-E §5.1.2.2.23 orders ISO-8859 parts 2–7 and 9–15.
    pub extended_characters: Option<[bool; EXTENDED_CHARACTER_FLAGS]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityResponse {
    BaseStation(BaseStationCapabilities),
    Mobile(MobileCapabilities),
}

fn at_least(version: ProtocolVersion, minimum: ProtocolVersion) -> bool {
    version.code() >= minimum.code()
}

fn field(bits: &mut Vec<bool>, value: u64, width: usize) -> Result<(), TrafficMessageError> {
    if value >= 1 << width {
        return Err(TrafficMessageError::Field("capability_field"));
    }
    put(bits, value, width);
    Ok(())
}

fn presence<T>(value: &Option<T>, required: bool) -> Result<(), TrafficMessageError> {
    if value.is_some() != required {
        return Err(TrafficMessageError::Field("capability_revision"));
    }
    Ok(())
}

impl CapabilityResponse {
    pub fn common(&self) -> &CapabilityFields {
        match self {
            Self::BaseStation(station) => &station.common,
            Self::Mobile(mobile) => &mobile.common,
        }
    }

    // TIA-136-133-E §§5.1.2.2.23 and 9.1.2.2.23. PV columns identify appended mandatory IEs.
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        let common = self.common();
        let version = common.protocol_version;
        common.encode_prefix(bits)?;
        if let Self::Mobile(mobile) = self {
            put(bits, u64::from(mobile.supported_bands), OCTET_BITS);
            put(bits, u64::from(mobile.analog_800_speech), FLAG_BITS);
        }
        let domains = common
            .encryption_algorithms
            .iter()
            .enumerate()
            .fold(0u8, |map, (domain, algorithms)| {
                map | if algorithms.is_some() { 1 << domain } else { 0 }
            });
        put(bits, u64::from(domains), OCTET_BITS);
        for algorithms in common.encryption_algorithms.iter().rev().flatten() {
            field(bits, u64::from(*algorithms), ALGORITHM_BITS)?;
        }
        let extended_characters = match self {
            Self::BaseStation(station) => {
                presence(&station.pv1, at_least(version, ProtocolVersion::Pv1))?;
                presence(
                    &station.extended_bands,
                    at_least(version, ProtocolVersion::Pv5),
                )?;
                if let Some(extension) = &station.pv1 {
                    for flag in [
                        extension.async_data,
                        extension.g3_fax,
                        extension.dads,
                        extension.user_group,
                        extension.oats,
                    ] {
                        put(bits, u64::from(flag), FLAG_BITS);
                    }
                    extension.characters.encode(bits);
                }
                if let Some(bands) = station.extended_bands {
                    put(bits, u64::from(bands), OCTET_BITS);
                }
                &station.extended_characters
            }
            Self::Mobile(mobile) => {
                presence(&mobile.pv1, at_least(version, ProtocolVersion::Pv1))?;
                presence(&mobile.advice, at_least(version, ProtocolVersion::Pv4))?;
                presence(&mobile.pv5, at_least(version, ProtocolVersion::Pv5))?;
                if let Some(extension) = &mobile.pv1 {
                    extension.encode(bits)?;
                }
                if let Some(advice) = mobile.advice {
                    put(bits, u64::from(advice), FLAG_BITS);
                }
                if let Some(extension) = &mobile.pv5 {
                    for value in [
                        extension.positioning_map,
                        extension.extended_bands,
                        extension.modulation_map,
                        extension.interleaving_map,
                    ] {
                        put(bits, u64::from(value), OCTET_BITS);
                    }
                }
                &mobile.extended_characters
            }
        };
        presence(extended_characters, at_least(version, ProtocolVersion::Pv6))?;
        if let Some(flags) = extended_characters {
            for flag in flags {
                put(bits, u64::from(*flag), FLAG_BITS);
            }
        }
        Ok(())
    }
}

impl CapabilityFields {
    fn encode_prefix(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        for (value, width) in [
            (u64::from(self.protocol_version.code()), PV_BITS),
            (u64::from(self.voice_privacy_map), PRIVACY_BITS),
            (u64::from(self.voice_coder_map), CODER_BITS),
            (u64::from(self.encryption_key_map), KEY_BITS),
            (u64::from(self.facch_sacch_arq), FLAG_BITS),
            (u64::from(self.soc), SOC_BITS),
            (u64::from(self.bsmc), OCTET_BITS),
            (u64::from(self.sms_map), SMS_BITS),
            (u64::from(self.ira), FLAG_BITS),
        ] {
            field(bits, value, width)?;
        }
        Ok(())
    }
}

impl CharacterSupport {
    fn encode(&self, bits: &mut Vec<bool>) {
        for flag in [self.latin1, self.bmp, self.latin_hebrew] {
            put(bits, u64::from(flag), FLAG_BITS);
        }
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, TrafficMessageError> {
        Ok(Self {
            latin1: flag(reader)?,
            bmp: flag(reader)?,
            latin_hebrew: flag(reader)?,
        })
    }
}

impl MobilePv1 {
    fn encode(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        for (value, width) in [
            (self.scm, SCM_BITS),
            (self.software_vintage, VINTAGE_BITS),
            (self.firmware_vintage, VINTAGE_BITS),
            (self.model, OCTET_BITS),
            (self.manufacturer, OCTET_BITS),
            (self.max_supported_pfc, PFC_BITS),
        ] {
            field(bits, u64::from(value), width)?;
        }
        for value in [
            self.async_data,
            self.g3_fax,
            self.dads,
            self.subaddress.is_some(),
        ] {
            put(bits, u64::from(value), FLAG_BITS);
        }
        if let Some(subaddress) = &self.subaddress {
            if subaddress.address.len() >= MAX_SUBADDRESS_CONTENT_OCTETS
                || !matches!(subaddress.kind, NSAP_SUBADDRESS | USER_SPECIFIED_SUBADDRESS)
            {
                return Err(TrafficMessageError::Field("capability_subaddress"));
            }
            put(bits, (subaddress.address.len() + 1) as u64, OCTET_BITS);
            put(bits, 0, SUBADDRESS_RESERVED_BITS);
            put(bits, u64::from(subaddress.odd), FLAG_BITS);
            put(bits, u64::from(subaddress.kind), SUBADDRESS_KIND_BITS);
            put(bits, 0, FLAG_BITS);
            for octet in &subaddress.address {
                put(bits, u64::from(*octet), OCTET_BITS);
            }
        }
        for flag in [
            self.user_group,
            self.half_rate_dtc,
            self.double_rate_dtc,
            self.triple_rate_dtc,
            self.stu_iii,
        ] {
            put(bits, u64::from(flag), FLAG_BITS);
        }
        self.characters.encode(bits);
        Ok(())
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, TrafficMessageError> {
        let mut extension = Self {
            scm: reader.take(SCM_BITS)? as u8,
            software_vintage: reader.take(VINTAGE_BITS)? as u8,
            firmware_vintage: reader.take(VINTAGE_BITS)? as u8,
            model: reader.take(OCTET_BITS)? as u8,
            manufacturer: reader.take(OCTET_BITS)? as u8,
            max_supported_pfc: reader.take(PFC_BITS)? as u8,
            async_data: flag(reader)?,
            g3_fax: flag(reader)?,
            dads: flag(reader)?,
            ..Self::default()
        };
        if flag(reader)? {
            let length = reader.take(OCTET_BITS)? as usize;
            if !(1..=MAX_SUBADDRESS_CONTENT_OCTETS).contains(&length) {
                return Err(TrafficMessageError::Field("capability_subaddress_length"));
            }
            reader.take(SUBADDRESS_RESERVED_BITS)?;
            let odd = flag(reader)?;
            let kind = reader.take(SUBADDRESS_KIND_BITS)? as u8;
            reader.take(FLAG_BITS)?;
            let address = (1..length)
                .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
                .collect::<Result<_, _>>()?;
            extension.subaddress = Some(Subaddress { odd, kind, address });
        }
        extension.user_group = flag(reader)?;
        extension.half_rate_dtc = flag(reader)?;
        extension.double_rate_dtc = flag(reader)?;
        extension.triple_rate_dtc = flag(reader)?;
        extension.stu_iii = flag(reader)?;
        extension.characters = CharacterSupport::decode(reader)?;
        Ok(extension)
    }
}

fn flag(reader: &mut Reader<'_>) -> Result<bool, TrafficMessageError> {
    Ok(reader.take(FLAG_BITS)? != 0)
}

pub(super) fn decode(
    kind: u8,
    direction: Direction,
    reader: &mut Reader<'_>,
) -> Result<Option<TrafficMessage>, TrafficMessageError> {
    if !matches!(kind, CAPABILITY_REQUEST | CAPABILITY_RESPONSE) {
        return Ok(None);
    }
    let version = ProtocolVersion::decode(reader.take(PV_BITS)? as u8)
        .map_err(|_| TrafficMessageError::Field("protocol_version"))?;
    if kind == CAPABILITY_REQUEST {
        return Ok(Some(TrafficMessage::CapabilityUpdateRequest(version)));
    }
    let mut common = CapabilityFields {
        protocol_version: version,
        voice_privacy_map: reader.take(PRIVACY_BITS)? as u8,
        voice_coder_map: reader.take(CODER_BITS)? as u8,
        encryption_key_map: reader.take(KEY_BITS)? as u8,
        facch_sacch_arq: flag(reader)?,
        soc: reader.take(SOC_BITS)? as u16,
        bsmc: reader.take(OCTET_BITS)? as u8,
        sms_map: reader.take(SMS_BITS)? as u8,
        ira: flag(reader)?,
        encryption_algorithms: [None; OCTET_BITS],
    };
    let mut mobile = match direction {
        Direction::Forward => None,
        Direction::Reverse => Some((reader.take(OCTET_BITS)? as u8, flag(reader)?)),
    };
    let domains = reader.take(OCTET_BITS)? as u8;
    // TIA-136-123-E §6.125 scans domain bits from most to least significant.
    for domain in (0..OCTET_BITS).rev() {
        if domains & (1 << domain) != 0 {
            common.encryption_algorithms[domain] = Some(reader.take(ALGORITHM_BITS)? as u8);
        }
    }
    let response = if let Some((supported_bands, analog_800_speech)) = mobile.take() {
        let pv1 = at_least(version, ProtocolVersion::Pv1)
            .then(|| MobilePv1::decode(reader))
            .transpose()?;
        let advice = at_least(version, ProtocolVersion::Pv4)
            .then(|| flag(reader))
            .transpose()?;
        let pv5 = at_least(version, ProtocolVersion::Pv5)
            .then(|| {
                Ok(MobilePv5 {
                    positioning_map: reader.take(OCTET_BITS)? as u8,
                    extended_bands: reader.take(OCTET_BITS)? as u8,
                    modulation_map: reader.take(OCTET_BITS)? as u8,
                    interleaving_map: reader.take(OCTET_BITS)? as u8,
                })
            })
            .transpose()?;
        CapabilityResponse::Mobile(MobileCapabilities {
            common,
            supported_bands,
            analog_800_speech,
            pv1,
            advice,
            pv5,
            extended_characters: decode_characters(version, reader)?,
        })
    } else {
        let pv1 = at_least(version, ProtocolVersion::Pv1)
            .then(|| {
                Ok(StationPv1 {
                    async_data: flag(reader)?,
                    g3_fax: flag(reader)?,
                    dads: flag(reader)?,
                    user_group: flag(reader)?,
                    oats: flag(reader)?,
                    characters: CharacterSupport::decode(reader)?,
                })
            })
            .transpose()?;
        let extended_bands = at_least(version, ProtocolVersion::Pv5)
            .then(|| reader.take(OCTET_BITS).map(|value| value as u8))
            .transpose()?;
        CapabilityResponse::BaseStation(BaseStationCapabilities {
            common,
            pv1,
            extended_bands,
            extended_characters: decode_characters(version, reader)?,
        })
    };
    Ok(Some(TrafficMessage::CapabilityUpdateResponse(response)))
}

fn decode_characters(
    version: ProtocolVersion,
    reader: &mut Reader<'_>,
) -> Result<Option<[bool; EXTENDED_CHARACTER_FLAGS]>, TrafficMessageError> {
    if !at_least(version, ProtocolVersion::Pv6) {
        return Ok(None);
    }
    let mut flags = [false; EXTENDED_CHARACTER_FLAGS];
    for value in &mut flags {
        *value = flag(reader)?;
    }
    Ok(Some(flags))
}

#[cfg(test)]
mod tests;
