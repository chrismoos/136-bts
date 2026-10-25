use super::{AccessMessage, OptionsError, decode_prefix};
use crate::l3::{
    broadcast::ProtocolVersion,
    control_fields::{self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put},
};
const FIXED_BITS: usize = 40;
const ESNX: u8 = 1;
const S_ESN: u8 = 2;
const ESNX_BITS: usize = 24;
const S_ESN_BITS: usize = 32;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialNumber {
    pub esn: u32,
    pub esnx: Option<u32>,
    pub s_esn: Option<u32>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
impl SerialNumber {
    // TIA-136-123-E §§5.4.13,6.69,6.214 assign ESNX 24 bits and S-ESN 32 bits.
    pub fn encode(&self) -> Result<Vec<u8>, OptionsError> {
        let bytes = AccessMessage::SerialNumber { esn: self.esn }
            .encode()
            .map_err(super::options::mandatory_error)?;
        let mut bits = control_fields::bits(&bytes)?;
        for (code, value, width) in [
            (ESNX, self.esnx, ESNX_BITS),
            (S_ESN, self.s_esn, S_ESN_BITS),
        ] {
            if let Some(value) = value {
                put(&mut bits, u64::from(code), PARAMETER_BITS)?;
                put(&mut bits, u64::from(value), width)?;
            }
        }
        bits.extend(&self.ignored_bits);
        bits.extend(&self.unparsed_bits);
        pack(bits)
    }
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), OptionsError> {
        if self.esnx.is_some() && version.code() < ProtocolVersion::Pv1.code() {
            return Err(OptionsError::Field("esnx_version"));
        }
        if self.s_esn.is_some() && version.code() < ProtocolVersion::Pv5.code() {
            return Err(OptionsError::Field("s_esn_version"));
        }
        Ok(())
    }
    pub fn has_extensions(&self) -> bool {
        self.esnx.is_some()
            || self.s_esn.is_some()
            || !self.ignored_bits.is_empty()
            || !self.unparsed_bits.is_empty()
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, OptionsError> {
        let bits = control_fields::bits(bytes)?;
        let prefix = decode_prefix(bytes).map_err(super::options::mandatory_error)?;
        let AccessMessage::SerialNumber { esn } = prefix.message else {
            return Err(OptionsError::MessageType);
        };
        let mut r = Reader {
            bits: &bits,
            offset: FIXED_BITS,
        };
        let mut result = Self {
            esn,
            esnx: None,
            s_esn: None,
            ignored_bits: Vec::new(),
            unparsed_bits: Vec::new(),
        };
        let mut previous = 0;
        while r.offset < r.bits.len() {
            if r.bits.len() - r.offset < OCTET_BITS && r.bits[r.offset..].iter().all(|bit| !*bit) {
                break;
            }
            let start = r.offset;
            let code = r.take(PARAMETER_BITS)? as u8;
            if !matches!(code, ESNX | S_ESN) {
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
                return Err(OptionsError::Field("parameter_order"));
            }
            previous = code;
            match code {
                ESNX => result.esnx = Some(r.take(ESNX_BITS)? as u32),
                S_ESN => result.s_esn = Some(r.take(S_ESN_BITS)? as u32),
                _ => unreachable!(),
            }
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serial_extensions_match_independent_fields_without_octet_padding_between_ies() {
        let value = SerialNumber {
            esn: 0x12345678,
            esnx: Some(0xabcdef),
            s_esn: Some(0x10203040),
            ignored_bits: Vec::new(),
            unparsed_bits: vec![],
        };
        // Type13/ESN32, parameter1/ESNX24, parameter2/S-ESN32.
        let wire = [
            0x0d, 0x12, 0x34, 0x56, 0x78, 0x1a, 0xbc, 0xde, 0xf2, 0x10, 0x20, 0x30, 0x40,
        ];
        assert_eq!(value.encode().unwrap(), wire);
        assert_eq!(SerialNumber::decode(&wire).unwrap(), value);
        assert!(value.validate_version(ProtocolVersion::Pv1).is_err());
        assert!(value.validate_version(ProtocolVersion::Pv5).is_ok());
        for end in 6..9 {
            assert!(SerialNumber::decode(&wire[..end]).is_err());
        }
        for end in 10..13 {
            assert!(SerialNumber::decode(&wire[..end]).is_err());
        }
    }
    #[test]
    fn serial_zero_presence_bounds_versions_and_unknown_tail_are_preserved() {
        let mut value = SerialNumber {
            esn: 0,
            esnx: Some(0),
            s_esn: None,
            ignored_bits: Vec::new(),
            unparsed_bits: vec![],
        };
        assert_eq!(
            SerialNumber::decode(&value.encode().unwrap()).unwrap(),
            value
        );
        assert!(value.validate_version(ProtocolVersion::Is136RevA).is_err());
        assert!(value.validate_version(ProtocolVersion::Pv1).is_ok());
        value.esnx = Some(1 << 24);
        assert!(value.encode().is_err());
        value.esnx = None;
        value.unparsed_bits = vec![true; 8];
        assert_eq!(
            SerialNumber::decode(&value.encode().unwrap()).unwrap(),
            value
        );
        assert_eq!(
            SerialNumber::decode(&vec![0; 256]),
            Err(OptionsError::Length)
        );
        assert!(SerialNumber::decode(&[13, 0, 0, 0]).is_err());
        assert!(SerialNumber::decode(&[13, 0, 0, 0, 0, 0x10, 0, 0, 0x01, 0, 0, 0]).is_err());
    }
}
