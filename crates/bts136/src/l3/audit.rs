pub use super::control_fields::Error as AuditError;
use super::{
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_subaddress, write_subaddress,
    },
    legacy_traffic::Subaddress,
};
pub const ORDER_TYPE: u8 = 18;
pub const CONFIRMATION_TYPE: u8 = 1;
const PROTOCOL_BITS: usize = 2;
const IS136_PROTOCOL: u64 = 0;
const ALLOCATED_GROUP_STATUS: u64 = 0;
const TYPE_BITS: usize = 6;
const PFC_BITS: usize = 3;
const STATUS_BITS: usize = 2;
const GROUP_TYPE_BITS: usize = 2;
const GROUP_WIDTHS: [usize; 4] = [20, 24, 34, 50];
const SELECTED_SYSTEM: u8 = 1;
const USER_GROUP: u8 = 2;
const SUBADDRESS: u8 = 3;
const ORDER_SUBADDRESS: u8 = 1;
const DISPLAY: u8 = 8;
const MULTILINGUAL: u8 = 9;
const ENCODING_BITS: usize = 5;
const RESERVED_BITS: usize = 3;
const BMP: u8 = 4;
const BMP_CHARACTER_OCTETS: usize = 2;
const FIRST_SURROGATE: u16 = 0xd800;
const LAST_SURROGATE: u16 = 0xdfff;
const MIN_ENCODING: u8 = 2;
const MAX_ENCODING: u8 = 18;
const MAX_CHARACTERS: usize = 82;
pub(super) const MULTILINGUAL_HEADER_OCTETS: usize = 1;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditOrder {
    pub force_registration: bool,
    pub debug_display_allowed: bool,
    pub subaddress: Option<Subaddress>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserGroupId {
    pub kind: u8,
    pub value: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultilingualDisplay {
    pub encoding: u8,
    pub bytes: Vec<u8>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditConfirmation {
    pub pfc_minus_one: u8,
    pub selected_system: Option<u8>,
    pub group: Option<UserGroupId>,
    pub subaddress: Option<Subaddress>,
    pub display: Option<String>,
    pub multilingual_display: Option<MultilingualDisplay>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
fn header(bits: &mut Vec<bool>, kind: u8) -> Result<(), AuditError> {
    put(bits, IS136_PROTOCOL, PROTOCOL_BITS)?;
    put(bits, u64::from(kind), TYPE_BITS)
}
fn prefix(reader: &mut Reader<'_>, kind: u8) -> Result<(), AuditError> {
    if reader.take(PROTOCOL_BITS)? != IS136_PROTOCOL {
        return Err(AuditError::Protocol);
    }
    if reader.take(TYPE_BITS)? as u8 != kind {
        return Err(AuditError::MessageType);
    }
    Ok(())
}
fn parameter(bits: &mut Vec<bool>, code: u8) -> Result<(), AuditError> {
    put(bits, u64::from(code), PARAMETER_BITS)
}
pub(super) fn text(bits: &mut Vec<bool>, bytes: &[u8]) -> Result<(), AuditError> {
    if bytes.len() > MAX_CHARACTERS {
        return Err(AuditError::Field("display_length"));
    }
    put(bits, bytes.len() as u64, OCTET_BITS)?;
    for byte in bytes {
        put(bits, u64::from(*byte), OCTET_BITS)?;
    }
    Ok(())
}
fn next(reader: &mut Reader<'_>) -> Result<Option<(usize, u8)>, AuditError> {
    let remaining = &reader.bits[reader.offset..];
    if remaining.len() < OCTET_BITS && remaining.iter().all(|bit| !*bit) {
        reader.offset = reader.bits.len();
        return Ok(None);
    }
    if remaining.len() < PARAMETER_BITS {
        return Err(AuditError::Field("padding"));
    }
    let start = reader.offset;
    Ok(Some((start, reader.take(PARAMETER_BITS)? as u8)))
}
impl AuditOrder {
    // TIA-136-123-E §5.3.2 orders the forced-registration and debug-display flags before optional IEs.
    pub fn encode(&self) -> Result<Vec<u8>, AuditError> {
        let mut bits = Vec::new();
        header(&mut bits, ORDER_TYPE)?;
        bits.extend([self.force_registration, self.debug_display_allowed]);
        if let Some(address) = &self.subaddress {
            write_subaddress(&mut bits, ORDER_SUBADDRESS, address)?;
        }
        bits.extend(&self.ignored_bits);
        bits.extend(&self.unparsed_bits);
        pack(bits)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, AuditError> {
        let bits = control_fields::bits(bytes)?;
        let mut r = Reader {
            bits: &bits,
            offset: 0,
        };
        prefix(&mut r, ORDER_TYPE)?;
        let mut order = Self {
            force_registration: r.flag()?,
            debug_display_allowed: r.flag()?,
            ..Default::default()
        };
        let mut previous = 0;
        while let Some((start, code)) = next(&mut r)? {
            if code != ORDER_SUBADDRESS {
                if r.skip_noncritical(code)? {
                    order
                        .ignored_bits
                        .extend_from_slice(&r.bits[start..r.offset]);
                    continue;
                }
                order.unparsed_bits = r.bits[start..].to_vec();
                break;
            }
            if code <= previous {
                return Err(AuditError::Field("parameter_order"));
            }
            previous = code;
            order.subaddress = Some(read_subaddress(&mut r)?);
        }
        Ok(order)
    }
}
impl AuditConfirmation {
    // TIA-136-123-E §§5.4.1,6.165 use PFC Minus One, not the maximum-supported PFC capability.
    pub fn encode(&self) -> Result<Vec<u8>, AuditError> {
        let mut bits = Vec::new();
        header(&mut bits, CONFIRMATION_TYPE)?;
        put(&mut bits, u64::from(self.pfc_minus_one), PFC_BITS)?;
        if let Some(system) = self.selected_system {
            parameter(&mut bits, SELECTED_SYSTEM)?;
            put(&mut bits, u64::from(system), PARAMETER_BITS)?;
        }
        if let Some(group) = &self.group {
            parameter(&mut bits, USER_GROUP)?;
            put(&mut bits, ALLOCATED_GROUP_STATUS, STATUS_BITS)?;
            let width = *GROUP_WIDTHS
                .get(usize::from(group.kind))
                .ok_or(AuditError::Field("user_group_kind"))?;
            put(&mut bits, u64::from(group.kind), GROUP_TYPE_BITS)?;
            put(&mut bits, group.value, width)?;
        }
        if let Some(address) = &self.subaddress {
            write_subaddress(&mut bits, SUBADDRESS, address)?;
        }
        if let Some(display) = &self.display {
            if !display.is_ascii() {
                return Err(AuditError::Field("display_encoding"));
            }
            parameter(&mut bits, DISPLAY)?;
            text(&mut bits, display.as_bytes())?;
        }
        if let Some(display) = &self.multilingual_display {
            validate_multilingual(display)?;
            parameter(&mut bits, MULTILINGUAL)?;
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
    pub fn decode(bytes: &[u8]) -> Result<Self, AuditError> {
        let bits = control_fields::bits(bytes)?;
        let mut r = Reader {
            bits: &bits,
            offset: 0,
        };
        prefix(&mut r, CONFIRMATION_TYPE)?;
        let mut result = Self {
            pfc_minus_one: r.take(PFC_BITS)? as u8,
            ..Default::default()
        };
        let mut previous = 0;
        while let Some((start, code)) = next(&mut r)? {
            if !matches!(
                code,
                SELECTED_SYSTEM | USER_GROUP | SUBADDRESS | DISPLAY | MULTILINGUAL
            ) {
                if r.skip_noncritical(code)? {
                    result
                        .ignored_bits
                        .extend_from_slice(&r.bits[start..r.offset]);
                    continue;
                }
                result.unparsed_bits = r.bits[start..].to_vec();
                break;
            }
            if code <= previous {
                return Err(AuditError::Field("parameter_order"));
            }
            previous = code;
            match code {
                SELECTED_SYSTEM => result.selected_system = Some(r.take(PARAMETER_BITS)? as u8),
                USER_GROUP => {
                    if r.take(STATUS_BITS)? != ALLOCATED_GROUP_STATUS {
                        return Err(AuditError::Field("user_group_status"));
                    }
                    let kind = r.take(GROUP_TYPE_BITS)? as u8;
                    let value = r.take(GROUP_WIDTHS[usize::from(kind)])?;
                    result.group = Some(UserGroupId { kind, value });
                }
                SUBADDRESS => result.subaddress = Some(read_subaddress(&mut r)?),
                DISPLAY | MULTILINGUAL => {
                    let length = r.take(OCTET_BITS)? as usize;
                    let encoding = if code == MULTILINGUAL {
                        let encoding = r.take(ENCODING_BITS)? as u8;
                        if r.take(RESERVED_BITS)? != 0 {
                            return Err(AuditError::Field("display_reserved"));
                        }
                        Some(encoding)
                    } else {
                        None
                    };
                    let length = display_octets(length, encoding)?;
                    let bytes = (0..length)
                        .map(|_| r.take(OCTET_BITS).map(|value| value as u8))
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Some(encoding) = encoding {
                        let display = MultilingualDisplay { encoding, bytes };
                        validate_multilingual(&display)?;
                        result.multilingual_display = Some(display);
                    } else {
                        if !bytes.is_ascii() {
                            return Err(AuditError::Field("display_encoding"));
                        }
                        result.display = Some(
                            String::from_utf8(bytes)
                                .map_err(|_| AuditError::Field("display_encoding"))?,
                        );
                    }
                }
                _ => unreachable!(),
            }
        }
        Ok(result)
    }
}
// TIA-136-123-E §5 counts every remaining octet, including the encoding/reserved octet.
pub(super) fn display_octets(length: usize, encoding: Option<u8>) -> Result<usize, AuditError> {
    let header = if encoding.is_some() {
        MULTILINGUAL_HEADER_OCTETS
    } else {
        0
    };
    let octets = length
        .checked_sub(header)
        .ok_or(AuditError::Field("display_length"))?;
    if octets > MAX_CHARACTERS {
        return Err(AuditError::Field("display_length"));
    }
    Ok(octets)
}
pub(super) fn validate_multilingual(display: &MultilingualDisplay) -> Result<(), AuditError> {
    if !(MIN_ENCODING..=MAX_ENCODING).contains(&display.encoding)
        || display.bytes.len() > MAX_CHARACTERS
        || (display.encoding == BMP
            && (!display.bytes.len().is_multiple_of(BMP_CHARACTER_OCTETS)
                || display
                    .bytes
                    .as_chunks::<BMP_CHARACTER_OCTETS>()
                    .0
                    .iter()
                    .any(|pair| {
                        (FIRST_SURROGATE..=LAST_SURROGATE).contains(&u16::from_be_bytes(*pair))
                    })))
    {
        return Err(AuditError::Field("multilingual_display"));
    }
    Ok(())
}
#[cfg(test)]
mod tests;
