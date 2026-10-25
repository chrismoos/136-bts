use super::broadcast::ProtocolVersion;
use std::{error::Error, fmt};

const PROTOCOL_BITS: usize = 2;
const TYPE_BITS: usize = 6;
const VERSION_BITS: usize = 4;
const FLAG_BITS: usize = 1;
const SCM_BITS: usize = 5;
const SERVICE_BITS: usize = 4;
const OCTET_BITS: usize = 8;
const PAGE_RESPONSE: u64 = 8;
const IS136_PROTOCOL: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ServiceCode {
    AnalogSpeech = 0,
    DigitalSpeech = 1,
    AnalogPreferred = 2,
    DigitalPreferred = 3,
    AsyncData = 4,
    G3Fax = 5,
    ServiceRejected = 6,
    StuIii = 7,
    DirectAsyncData = 8,
    Wort = 9,
}
impl ServiceCode {
    pub(crate) fn decode(code: usize) -> Result<Self, PageResponseError> {
        const SERVICES: [ServiceCode; 10] = [
            ServiceCode::AnalogSpeech,
            ServiceCode::DigitalSpeech,
            ServiceCode::AnalogPreferred,
            ServiceCode::DigitalPreferred,
            ServiceCode::AsyncData,
            ServiceCode::G3Fax,
            ServiceCode::ServiceRejected,
            ServiceCode::StuIii,
            ServiceCode::DirectAsyncData,
            ServiceCode::Wort,
        ];
        SERVICES
            .get(code)
            .copied()
            .ok_or(PageResponseError::Field("service_code"))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageResponse {
    pub protocol_version: ProtocolVersion,
    pub last_try: bool,
    pub scm: u8,
    pub service_code: ServiceCode,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPageResponse {
    pub message: PageResponse,
    pub consumed_bits: usize,
    /// Optional information elements and final octet padding are retained without interpretation.
    pub remaining_bits: Vec<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageResponseError {
    Truncated { needed_bits: usize },
    Protocol,
    MessageType,
    Field(&'static str),
    RequiredDataOptions,
}
impl fmt::Display for PageResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid IS-136 Page Response: {self:?}")
    }
}
impl Error for PageResponseError {}
impl PageResponse {
    // IS-136.1-A §§6.4.4.8/6.4.1.1. Absent Voice Mode defaults to VSELP without privacy.
    pub fn encode(self) -> Result<Vec<u8>, PageResponseError> {
        if matches!(
            self.service_code,
            ServiceCode::AsyncData | ServiceCode::G3Fax
        ) {
            return Err(PageResponseError::RequiredDataOptions);
        }
        self.encode_mandatory()
    }
    pub(crate) fn encode_mandatory(self) -> Result<Vec<u8>, PageResponseError> {
        if self.scm >= 1 << SCM_BITS {
            return Err(PageResponseError::Field("scm"));
        }
        let mut bits = Vec::new();
        for (value, width) in [
            (IS136_PROTOCOL, PROTOCOL_BITS),
            (PAGE_RESPONSE, TYPE_BITS),
            (u64::from(self.protocol_version.code()), VERSION_BITS),
            (u64::from(self.last_try), FLAG_BITS),
            (u64::from(self.scm), SCM_BITS),
            (self.service_code as u64, SERVICE_BITS),
        ] {
            bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
        }
        bits.resize(bits.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
        Ok(bits
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|octet| {
                octet
                    .iter()
                    .fold(0, |value, bit| value * 2 + u8::from(*bit))
            })
            .collect())
    }
}

// IS-136.1-A §6.4.4.8. Mandatory field prefix is 22 bits.
pub fn decode_prefix(octets: &[u8]) -> Result<DecodedPageResponse, PageResponseError> {
    let bits: Vec<bool> = octets
        .iter()
        .flat_map(|octet| {
            (0..OCTET_BITS)
                .rev()
                .map(move |bit| octet & (1 << bit) != 0)
        })
        .collect();
    let mut offset = 0;
    let mut take = |width: usize| {
        let end = offset + width;
        let field = bits
            .get(offset..end)
            .ok_or(PageResponseError::Truncated { needed_bits: end })?;
        offset = end;
        Ok::<u64, PageResponseError>(
            field
                .iter()
                .fold(0, |value, bit| value * 2 + u64::from(*bit)),
        )
    };
    if take(PROTOCOL_BITS)? != IS136_PROTOCOL {
        return Err(PageResponseError::Protocol);
    }
    if take(TYPE_BITS)? != PAGE_RESPONSE {
        return Err(PageResponseError::MessageType);
    }
    let message = PageResponse {
        protocol_version: ProtocolVersion::decode(take(VERSION_BITS)? as u8)
            .map_err(|_| PageResponseError::Field("protocol_version"))?,
        last_try: take(FLAG_BITS)? != 0,
        scm: take(SCM_BITS)? as u8,
        service_code: ServiceCode::decode(take(SERVICE_BITS)? as usize)?,
    };
    Ok(DecodedPageResponse {
        message,
        consumed_bits: offset,
        remaining_bits: bits[offset..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn required_fields_match_fixed_vector_and_optional_suffix_is_preserved() {
        let message = PageResponse {
            protocol_version: ProtocolVersion::Is136RevA,
            last_try: true,
            scm: 0b10101,
            service_code: ServiceCode::DigitalSpeech,
        };
        assert_eq!(message.encode().unwrap(), [0x08, 0x4d, 0x44]);
        let decoded = decode_prefix(&[0x08, 0x4d, 0x44]).unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.consumed_bits, 22);
        assert_eq!(decoded.remaining_bits, [false; 2]);
        assert_eq!(
            decode_prefix(&[0x08, 0x4d, 0x47, 0xff])
                .unwrap()
                .remaining_bits,
            [true; 10]
        );
        for code in 0..10 {
            assert_eq!(
                decode_prefix(&[0x08, 0x4d, 0x40 | (code << 2)])
                    .unwrap()
                    .message
                    .service_code as u8,
                code
            );
        }
    }
    #[test]
    fn reserved_truncated_and_required_optional_cases_are_rejected() {
        assert_eq!(
            decode_prefix(&[0x48, 0, 0]),
            Err(PageResponseError::Protocol)
        );
        assert_eq!(
            decode_prefix(&[0x0c, 0, 0]),
            Err(PageResponseError::MessageType)
        );
        assert_eq!(
            decode_prefix(&[0x08, 0x30, 0]),
            Err(PageResponseError::Field("protocol_version"))
        );
        assert_eq!(
            decode_prefix(&[0x08, 0x4d, 0x7c]),
            Err(PageResponseError::Field("service_code"))
        );
        assert!(matches!(
            decode_prefix(&[0x08, 0x4d]),
            Err(PageResponseError::Truncated { .. })
        ));
        let message = PageResponse {
            protocol_version: ProtocolVersion::Is136RevA,
            last_try: false,
            scm: 32,
            service_code: ServiceCode::DigitalSpeech,
        };
        assert_eq!(message.encode(), Err(PageResponseError::Field("scm")));
        for service_code in [ServiceCode::AsyncData, ServiceCode::G3Fax] {
            assert_eq!(
                PageResponse {
                    scm: 0,
                    service_code,
                    ..message
                }
                .encode(),
                Err(PageResponseError::RequiredDataOptions)
            );
        }
    }
}
