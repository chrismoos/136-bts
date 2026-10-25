use super::{AccessMessage, RegistrationError, RegistrationType, decode_prefix};
pub use crate::l3::control_fields::Error as OptionsError;
use crate::l3::{
    address::{self, AddressInfo},
    broadcast::ProtocolVersion,
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_subaddress, write_subaddress,
    },
    legacy_traffic::Subaddress,
};
const FIXED_BITS: usize = 21;
const C_NUMBER: u8 = 1;
const PFC: u8 = 2;
const ENCRYPTION: u8 = 3;
const SELECTED_SYSTEM: u8 = 4;
const GROUP: u8 = 5;
const SUBADDRESS: u8 = 6;
const EXTENSION: u8 = 7;
const SID: u8 = 1;
const NUMBER_REQUEST: u8 = 2;
const TARGET_SOC: u8 = 3;
const MULTILINGUAL_NUMBER: u8 = 4;
const SYSTEM_VALUE: u8 = 5;
const PFC_BITS: usize = 3;
const ENCRYPTION_BITS: usize = 3;
const SID_BITS: usize = 15;
const SOC_BITS: usize = 12;
const SYSTEM_VALUE_BITS: usize = 17;
const GROUP_STATUS_BITS: usize = 2;
const GROUP_KIND_BITS: usize = 2;
const GROUP_WIDTHS: [usize; 4] = [20, 24, 34, 50];
const ACCEPT: u64 = 29;
const ASSIGN_PFC: u64 = 1;
const DIRECTORY: u64 = 9;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncryptionMode {
    pub algorithm: u8,
    pub domain: u8,
    pub key: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupRequest {
    Allocated { kind: u8, value: u64 },
    Unspecified,
    Deallocate,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultilingualNumber {
    pub number_type: u8,
    pub numbering_plan: u8,
    pub encoding: u8,
    pub bytes: Vec<u8>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistrationOptions {
    pub c_number: Option<AddressInfo>,
    pub pfc: Option<u8>,
    pub encryption: Option<EncryptionMode>,
    pub selected_system: Option<u8>,
    pub group: Option<GroupRequest>,
    pub subaddress: Option<Subaddress>,
    pub sid_report: Option<u16>,
    pub number_request: Option<u8>,
    pub target_soc: Option<u16>,
    pub multilingual_number: Option<MultilingualNumber>,
    pub system_value: Option<u32>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationRequest {
    pub registration_type: RegistrationType,
    pub scm: u8,
    pub protocol_version: ProtocolVersion,
    pub options: RegistrationOptions,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcceptOptions {
    pub pfc: Option<u8>,
    pub directory_address: Option<AddressInfo>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
fn parameter(bits: &mut Vec<bool>, code: u8) -> Result<(), OptionsError> {
    put(bits, u64::from(code), PARAMETER_BITS)
}
fn auxiliary(bits: &mut Vec<bool>, code: u8) -> Result<(), OptionsError> {
    parameter(bits, EXTENSION)?;
    parameter(bits, code)
}
fn read_address(r: &mut Reader<'_>) -> Result<AddressInfo, OptionsError> {
    let count = r.take(OCTET_BITS)? as usize;
    let mut bytes = vec![count as u8];
    for _ in 0..count {
        bytes.push(r.take(OCTET_BITS)? as u8);
    }
    address::decode_prefix(&bytes)
        .map(|value| value.address)
        .map_err(|_| OptionsError::Field("address_info"))
}
fn write_address(bits: &mut Vec<bool>, address: &AddressInfo) -> Result<(), OptionsError> {
    for byte in address
        .encode()
        .map_err(|_| OptionsError::Field("address_info"))?
    {
        put(bits, u64::from(byte), OCTET_BITS)?;
    }
    Ok(())
}
fn next(r: &mut Reader<'_>) -> Result<Option<(usize, u8)>, OptionsError> {
    let remaining = &r.bits[r.offset..];
    if remaining.len() < OCTET_BITS && remaining.iter().all(|bit| !*bit) {
        r.offset = r.bits.len();
        return Ok(None);
    }
    let start = r.offset;
    Ok(Some((start, r.take(PARAMETER_BITS)? as u8)))
}
pub(super) fn mandatory_error(error: RegistrationError) -> OptionsError {
    match error {
        RegistrationError::Truncated { needed_bits } => OptionsError::Truncated { needed_bits },
        RegistrationError::Protocol => OptionsError::Protocol,
        RegistrationError::Field(name) => OptionsError::Field(name),
        RegistrationError::UnsupportedType(_) => OptionsError::MessageType,
    }
}
impl RegistrationOptions {
    fn validate(&self, version: ProtocolVersion) -> Result<(), OptionsError> {
        if self.c_number.is_some() && self.multilingual_number.is_some() {
            return Err(OptionsError::Field("c_number_exclusive"));
        }
        if self.selected_system.is_some() && self.system_value.is_some() {
            return Err(OptionsError::Field("selected_system_exclusive"));
        }
        if self.target_soc.is_some() && self.selected_system.is_none() {
            return Err(OptionsError::Field("target_soc_context"));
        }
        if self.sid_report.is_some() && version.code() < ProtocolVersion::Is136RevA.code() {
            return Err(OptionsError::Field("sid_report_version"));
        }
        if (self.number_request.is_some()
            || self.target_soc.is_some()
            || self.multilingual_number.is_some())
            && version.code() < ProtocolVersion::Pv1.code()
        {
            return Err(OptionsError::Field("registration_option_version"));
        }
        if self.system_value.is_some() && version.code() < ProtocolVersion::Pv4.code() {
            return Err(OptionsError::Field("system_value_version"));
        }
        if self.number_request.is_some_and(|value| value > 1) {
            return Err(OptionsError::Field("number_request_reserved"));
        }
        Ok(())
    }
    fn write(&self, bits: &mut Vec<bool>, version: ProtocolVersion) -> Result<(), OptionsError> {
        self.validate(version)?;
        if let Some(value) = &self.c_number {
            parameter(bits, C_NUMBER)?;
            write_address(bits, value)?;
        }
        if let Some(value) = self.pfc {
            parameter(bits, PFC)?;
            put(bits, u64::from(value), PFC_BITS)?;
        }
        if let Some(value) = self.encryption {
            parameter(bits, ENCRYPTION)?;
            for value in [value.algorithm, value.domain, value.key] {
                put(bits, u64::from(value), ENCRYPTION_BITS)?;
            }
        }
        if let Some(value) = self.selected_system {
            parameter(bits, SELECTED_SYSTEM)?;
            put(bits, u64::from(value), PARAMETER_BITS)?;
        }
        if let Some(value) = &self.group {
            parameter(bits, GROUP)?;
            match value {
                GroupRequest::Allocated { kind, value } => {
                    put(bits, 0, GROUP_STATUS_BITS)?;
                    let width = *GROUP_WIDTHS
                        .get(usize::from(*kind))
                        .ok_or(OptionsError::Field("group_kind"))?;
                    put(bits, u64::from(*kind), GROUP_KIND_BITS)?;
                    put(bits, *value, width)?;
                }
                GroupRequest::Unspecified => put(bits, 1, GROUP_STATUS_BITS)?,
                GroupRequest::Deallocate => put(bits, 2, GROUP_STATUS_BITS)?,
            }
        }
        if let Some(value) = &self.subaddress {
            write_subaddress(bits, SUBADDRESS, value)?;
        }
        if let Some(value) = self.sid_report {
            auxiliary(bits, SID)?;
            put(bits, u64::from(value), SID_BITS)?;
        }
        if let Some(value) = self.number_request {
            auxiliary(bits, NUMBER_REQUEST)?;
            put(bits, u64::from(value), PARAMETER_BITS)?;
        }
        if let Some(value) = self.target_soc {
            auxiliary(bits, TARGET_SOC)?;
            put(bits, u64::from(value), SOC_BITS)?;
        }
        if let Some(value) = &self.multilingual_number {
            auxiliary(bits, MULTILINGUAL_NUMBER)?;
            let length = u8::try_from(value.bytes.len() + 2).map_err(|_| OptionsError::Length)?;
            put(bits, u64::from(length), OCTET_BITS)?;
            put(bits, u64::from(value.number_type), 3)?;
            put(bits, u64::from(value.numbering_plan), PARAMETER_BITS)?;
            put(bits, 0, 1)?;
            put(bits, u64::from(value.encoding), 5)?;
            put(bits, 0, 3)?;
            for byte in &value.bytes {
                put(bits, u64::from(*byte), OCTET_BITS)?;
            }
        }
        if let Some(value) = self.system_value {
            auxiliary(bits, SYSTEM_VALUE)?;
            put(bits, u64::from(value), SYSTEM_VALUE_BITS)?;
        }
        bits.extend(&self.ignored_bits);
        bits.extend(&self.unparsed_bits);
        Ok(())
    }
}
impl RegistrationRequest {
    // TIA-136-123-E §5.4.12 begins optional IEs immediately after the 21 mandatory bits.
    pub fn encode(&self) -> Result<Vec<u8>, OptionsError> {
        let bytes = AccessMessage::Registration {
            registration_type: self.registration_type,
            scm: self.scm,
            protocol_version: self.protocol_version,
        }
        .encode()
        .map_err(mandatory_error)?;
        let mut bits = control_fields::bits(&bytes)?;
        bits.truncate(FIXED_BITS);
        self.options.write(&mut bits, self.protocol_version)?;
        pack(bits)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, OptionsError> {
        let bits = control_fields::bits(bytes)?;
        let prefix = decode_prefix(bytes).map_err(mandatory_error)?;
        let AccessMessage::Registration {
            registration_type,
            scm,
            protocol_version,
        } = prefix.message
        else {
            return Err(OptionsError::MessageType);
        };
        let mut r = Reader {
            bits: &bits,
            offset: prefix.consumed_bits,
        };
        let mut options = RegistrationOptions::default();
        let mut previous = 0;
        while let Some((start, code)) = next(&mut r)? {
            let aux = if code == EXTENSION {
                Some(r.take(PARAMETER_BITS)? as u8)
            } else {
                None
            };
            if !(C_NUMBER..=EXTENSION).contains(&code)
                || aux.is_some_and(|value| !(SID..=SYSTEM_VALUE).contains(&value))
            {
                if r.skip_noncritical(code)? {
                    options
                        .ignored_bits
                        .extend_from_slice(&r.bits[start..r.offset]);
                    continue;
                }
                options.unparsed_bits = r.bits[start..].to_vec();
                break;
            }
            let order = if let Some(aux) = aux {
                u16::from(EXTENSION) * (1 << PARAMETER_BITS) + u16::from(aux)
            } else {
                u16::from(code) * (1 << PARAMETER_BITS)
            };
            if order <= previous {
                return Err(OptionsError::Field("parameter_order"));
            }
            previous = order;
            match (code, aux) {
                (C_NUMBER, _) => options.c_number = Some(read_address(&mut r)?),
                (PFC, _) => options.pfc = Some(r.take(PFC_BITS)? as u8),
                (ENCRYPTION, _) => {
                    options.encryption = Some(EncryptionMode {
                        algorithm: r.take(ENCRYPTION_BITS)? as u8,
                        domain: r.take(ENCRYPTION_BITS)? as u8,
                        key: r.take(ENCRYPTION_BITS)? as u8,
                    })
                }
                (SELECTED_SYSTEM, _) => {
                    options.selected_system = Some(r.take(PARAMETER_BITS)? as u8)
                }
                (GROUP, _) => {
                    options.group = Some(match r.take(GROUP_STATUS_BITS)? {
                        0 => {
                            let kind = r.take(GROUP_KIND_BITS)? as u8;
                            let value = r.take(GROUP_WIDTHS[usize::from(kind)])?;
                            GroupRequest::Allocated { kind, value }
                        }
                        1 => GroupRequest::Unspecified,
                        2 => GroupRequest::Deallocate,
                        _ => return Err(OptionsError::Field("group_status")),
                    })
                }
                (SUBADDRESS, _) => options.subaddress = Some(read_subaddress(&mut r)?),
                (EXTENSION, Some(SID)) => options.sid_report = Some(r.take(SID_BITS)? as u16),
                (EXTENSION, Some(NUMBER_REQUEST)) => {
                    options.number_request = Some(r.take(PARAMETER_BITS)? as u8)
                }
                (EXTENSION, Some(TARGET_SOC)) => {
                    options.target_soc = Some(r.take(SOC_BITS)? as u16)
                }
                (EXTENSION, Some(MULTILINGUAL_NUMBER)) => {
                    let count = r.take(OCTET_BITS)? as usize;
                    if count < 2 {
                        return Err(OptionsError::Field("multilingual_address_length"));
                    }
                    let number_type = r.take(3)? as u8;
                    let numbering_plan = r.take(PARAMETER_BITS)? as u8;
                    if r.flag()? {
                        return Err(OptionsError::Field("multilingual_address_reserved"));
                    }
                    let encoding = r.take(5)? as u8;
                    if r.take(3)? != 0 {
                        return Err(OptionsError::Field("multilingual_address_reserved"));
                    }
                    let bytes = (2..count)
                        .map(|_| r.take(OCTET_BITS).map(|value| value as u8))
                        .collect::<Result<Vec<_>, _>>()?;
                    options.multilingual_number = Some(MultilingualNumber {
                        number_type,
                        numbering_plan,
                        encoding,
                        bytes,
                    });
                }
                (EXTENSION, Some(SYSTEM_VALUE)) => {
                    options.system_value = Some(r.take(SYSTEM_VALUE_BITS)? as u32)
                }
                _ => unreachable!(),
            }
        }
        options.validate(protocol_version)?;
        Ok(Self {
            registration_type,
            scm,
            protocol_version,
            options,
        })
    }
}
impl AcceptOptions {
    pub fn encode(&self) -> Result<Vec<u8>, OptionsError> {
        let mut bits = Vec::new();
        put(&mut bits, ACCEPT, OCTET_BITS)?;
        if let Some(pfc) = self.pfc {
            put(&mut bits, ASSIGN_PFC, PARAMETER_BITS)?;
            put(&mut bits, u64::from(pfc), PFC_BITS)?;
        }
        if let Some(address) = &self.directory_address {
            put(&mut bits, DIRECTORY, PARAMETER_BITS)?;
            write_address(&mut bits, address)?;
        }
        bits.extend(&self.ignored_bits);
        bits.extend(&self.unparsed_bits);
        pack(bits)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, OptionsError> {
        let bits = control_fields::bits(bytes)?;
        let mut r = Reader {
            bits: &bits,
            offset: 0,
        };
        if r.take(OCTET_BITS)? != ACCEPT {
            return Err(OptionsError::MessageType);
        }
        let mut options = Self::default();
        let mut previous = 0;
        while let Some((start, code)) = next(&mut r)? {
            if !matches!(u64::from(code), ASSIGN_PFC | DIRECTORY) {
                if r.skip_noncritical(code)? {
                    options
                        .ignored_bits
                        .extend_from_slice(&r.bits[start..r.offset]);
                    continue;
                }
                options.unparsed_bits = r.bits[start..].to_vec();
                break;
            }
            if code <= previous {
                return Err(OptionsError::Field("parameter_order"));
            }
            previous = code;
            if u64::from(code) == ASSIGN_PFC {
                options.pfc = Some(r.take(PFC_BITS)? as u8);
            } else {
                options.directory_address = Some(read_address(&mut r)?);
            }
        }
        Ok(options)
    }
}
#[cfg(test)]
mod tests;
