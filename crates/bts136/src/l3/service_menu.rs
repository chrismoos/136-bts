use super::broadcast::ProtocolVersion;
use std::{error::Error as StdError, fmt};
pub const MESSAGE_TYPE: u8 = 8;
const OCTET_BITS: usize = 8;
const PRIVACY_BITS: usize = 4;
const VOICE_CODER_BITS: usize = 6;
const DOMAIN_BITS: usize = 8;
const ALGORITHM_BITS: usize = 4;
const KEY_BITS: usize = 4;
const MENU_BITS: usize = 10;
const FLAG_BITS: usize = 1;
const SMS_BITS: usize = 2;
const EXTENDED_CHARACTER_FLAGS: usize = 13;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceMenuPv1 {
    pub latin1: bool,
    pub bmp: bool,
    pub latin_hebrew: bool,
    pub async_data: bool,
    pub g3_fax: bool,
    pub dads: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceMenu {
    pub voice_privacy_map: u8,
    pub data_privacy_map: u8,
    pub voice_coder_map: u8,
    pub encryption_algorithms: [Option<u8>; DOMAIN_BITS],
    pub encryption_key_map: u8,
    pub menu_map: u16,
    pub facch_sacch_arq: bool,
    pub user_group: bool,
    pub sms_map: u8,
    pub ira: bool,
    pub oats: Option<bool>,
    pub pv1: Option<ServiceMenuPv1>,
    pub sms_broadcast: Option<bool>,
    pub extended_characters: Option<[bool; EXTENDED_CHARACTER_FLAGS]>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub message: ServiceMenu,
    pub remaining_bits: Vec<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Header,
    Revision,
    Field,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid Service Menu: {self:?}")
    }
}
impl StdError for Error {}
impl ServiceMenu {
    // TIA-136-123-E §5.1.1.2.6 adds mandatory flags at PV0, PV1, PV2 and PV6.
    pub fn encode(&self, revision: ProtocolVersion) -> Result<Vec<u8>, Error> {
        if revision.code() < ProtocolVersion::Is136Rev0.code()
            || self.oats.is_some() != (revision.code() >= ProtocolVersion::Is136RevA.code())
            || self.pv1.is_some() != (revision.code() >= ProtocolVersion::Pv1.code())
            || self.sms_broadcast.is_some() != (revision.code() >= ProtocolVersion::Pv2.code())
            || self.extended_characters.is_some()
                != (revision.code() >= ProtocolVersion::Pv6.code())
        {
            return Err(Error::Revision);
        }
        let mut bits = Vec::new();
        let mut put = |value: u16, width: usize| -> Result<(), Error> {
            if value >= 1 << width {
                return Err(Error::Field);
            }
            bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
            Ok(())
        };
        put(u16::from(MESSAGE_TYPE), OCTET_BITS)?;
        put(u16::from(self.voice_privacy_map), PRIVACY_BITS)?;
        put(u16::from(self.data_privacy_map), PRIVACY_BITS)?;
        put(u16::from(self.voice_coder_map), VOICE_CODER_BITS)?;
        let domains = self
            .encryption_algorithms
            .iter()
            .enumerate()
            .fold(0, |map, (domain, algorithm)| {
                map | if algorithm.is_some() { 1 << domain } else { 0 }
            });
        put(domains, DOMAIN_BITS)?;
        for algorithm in self.encryption_algorithms.iter().rev().flatten() {
            put(u16::from(*algorithm), ALGORITHM_BITS)?;
        }
        put(u16::from(self.encryption_key_map), KEY_BITS)?;
        put(self.menu_map, MENU_BITS)?;
        put(u16::from(self.facch_sacch_arq), FLAG_BITS)?;
        put(u16::from(self.user_group), FLAG_BITS)?;
        put(u16::from(self.sms_map), SMS_BITS)?;
        put(u16::from(self.ira), FLAG_BITS)?;
        if let Some(oats) = self.oats {
            put(u16::from(oats), FLAG_BITS)?;
        }
        if let Some(pv1) = &self.pv1 {
            for flag in [
                pv1.latin1,
                pv1.bmp,
                pv1.latin_hebrew,
                pv1.async_data,
                pv1.g3_fax,
                pv1.dads,
            ] {
                put(u16::from(flag), FLAG_BITS)?;
            }
        }
        if let Some(supported) = self.sms_broadcast {
            put(u16::from(supported), FLAG_BITS)?;
        }
        if let Some(characters) = self.extended_characters {
            for supported in characters {
                put(u16::from(supported), FLAG_BITS)?;
            }
        }
        bits.resize(bits.len().next_multiple_of(OCTET_BITS), false);
        Ok(bits
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|bits| bits.iter().fold(0, |byte, bit| byte << 1 | u8::from(*bit)))
            .collect())
    }
}
// IS-136.1-A Message Encryption Algorithm Map orders instances by domain bits from left to right.
pub fn decode_prefix(bytes: &[u8], revision: ProtocolVersion) -> Result<Decoded, Error> {
    if bytes.first().copied() != Some(MESSAGE_TYPE) {
        return Err(if bytes.is_empty() {
            Error::Truncated
        } else {
            Error::Header
        });
    }
    if revision.code() < ProtocolVersion::Is136Rev0.code() {
        return Err(Error::Revision);
    }
    let with_oats = revision.code() >= ProtocolVersion::Is136RevA.code();
    let bits: Vec<_> = bytes
        .iter()
        .flat_map(|byte| (0..OCTET_BITS).rev().map(move |bit| byte & (1 << bit) != 0))
        .collect();
    let mut position = OCTET_BITS;
    let mut take = |width: usize| -> Result<u16, Error> {
        let field = bits
            .get(position..position + width)
            .ok_or(Error::Truncated)?;
        position += width;
        Ok(field
            .iter()
            .fold(0, |value, bit| value << 1 | u16::from(*bit)))
    };
    let voice_privacy_map = take(PRIVACY_BITS)? as u8;
    let data_privacy_map = take(PRIVACY_BITS)? as u8;
    let voice_coder_map = take(VOICE_CODER_BITS)? as u8;
    let domains = take(DOMAIN_BITS)?;
    let mut encryption_algorithms = [None; DOMAIN_BITS];
    for domain in (0..DOMAIN_BITS).rev() {
        if domains & (1 << domain) != 0 {
            encryption_algorithms[domain] = Some(take(ALGORITHM_BITS)? as u8);
        }
    }
    let message = ServiceMenu {
        voice_privacy_map,
        data_privacy_map,
        voice_coder_map,
        encryption_algorithms,
        encryption_key_map: take(KEY_BITS)? as u8,
        menu_map: take(MENU_BITS)?,
        facch_sacch_arq: take(FLAG_BITS)? != 0,
        user_group: take(FLAG_BITS)? != 0,
        sms_map: take(SMS_BITS)? as u8,
        ira: take(FLAG_BITS)? != 0,
        oats: if with_oats {
            Some(take(FLAG_BITS)? != 0)
        } else {
            None
        },
        pv1: if revision.code() >= ProtocolVersion::Pv1.code() {
            Some(ServiceMenuPv1 {
                latin1: take(FLAG_BITS)? != 0,
                bmp: take(FLAG_BITS)? != 0,
                latin_hebrew: take(FLAG_BITS)? != 0,
                async_data: take(FLAG_BITS)? != 0,
                g3_fax: take(FLAG_BITS)? != 0,
                dads: take(FLAG_BITS)? != 0,
            })
        } else {
            None
        },
        sms_broadcast: if revision.code() >= ProtocolVersion::Pv2.code() {
            Some(take(FLAG_BITS)? != 0)
        } else {
            None
        },
        extended_characters: if revision.code() >= ProtocolVersion::Pv6.code() {
            let mut flags = [false; EXTENDED_CHARACTER_FLAGS];
            for flag in &mut flags {
                *flag = take(FLAG_BITS)? != 0;
            }
            Some(flags)
        } else {
            None
        },
    };
    Ok(Decoded {
        message,
        remaining_bits: bits[position..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encryption_instances_follow_domain_order_and_revisions_keep_distinct_lengths() {
        let bytes = [0x08, 0xa5, 0x86, 0x06, 0xf3, 0x6a, 0xab, 0x80];
        let decoded = decode_prefix(&bytes, ProtocolVersion::Is136RevA).unwrap();
        let message = &decoded.message;
        assert_eq!(
            (
                message.voice_privacy_map,
                message.data_privacy_map,
                message.voice_coder_map
            ),
            (10, 5, 33)
        );
        assert_eq!(message.encryption_algorithms[7], Some(11));
        assert_eq!(message.encryption_algorithms[0], Some(12));
        assert!(
            message.encryption_algorithms[1..7]
                .iter()
                .all(Option::is_none)
        );
        assert_eq!((message.encryption_key_map, message.menu_map), (13, 682));
        assert!(message.facch_sacch_arq && !message.user_group && message.ira);
        assert_eq!(message.sms_map, 3);
        assert_eq!(message.oats, Some(false));
        assert_eq!(decoded.remaining_bits, vec![false; 6]);
        assert_eq!(message.encode(ProtocolVersion::Is136RevA).unwrap(), bytes);
        assert_eq!(
            message.encode(ProtocolVersion::Is136Rev0),
            Err(Error::Revision)
        );
        let original = decode_prefix(&bytes, ProtocolVersion::Is136Rev0).unwrap();
        assert_eq!(original.message.oats, None);
        assert_eq!(original.remaining_bits.len(), 7);
        for length in 0..bytes.len() {
            assert_eq!(
                decode_prefix(&bytes[..length], ProtocolVersion::Is136RevA),
                Err(Error::Truncated)
            );
        }
    }
    #[test]
    fn later_service_menu_versions_match_fixed_vectors_and_mandatory_flag_order() {
        let base = [0x08, 0xa5, 0x86, 0x06, 0xf3, 0x6a, 0xab, 0x80];
        for version in [
            ProtocolVersion::Pv1,
            ProtocolVersion::Pv2,
            ProtocolVersion::Pv3,
            ProtocolVersion::Pv4,
            ProtocolVersion::Pv5,
            ProtocolVersion::Pv6,
            ProtocolVersion::Pv7,
        ] {
            let mut menu = decode_prefix(&base, ProtocolVersion::Is136RevA)
                .unwrap()
                .message;
            menu.pv1 = Some(ServiceMenuPv1 {
                latin1: true,
                bmp: false,
                latin_hebrew: true,
                async_data: false,
                g3_fax: true,
                dads: false,
            });
            let mut expected = vec![0x08, 0xa5, 0x86, 0x06, 0xf3, 0x6a, 0xab, 0xaa];
            if version.code() >= ProtocolVersion::Pv2.code() {
                menu.sms_broadcast = Some(true);
                expected.push(0x80);
            }
            if version.code() >= ProtocolVersion::Pv6.code() {
                menu.extended_characters =
                    Some(std::array::from_fn(|index| index.is_multiple_of(2)));
                expected[8] = 0xd5;
                expected.push(0x54);
            }
            assert_eq!(menu.encode(version).unwrap(), expected, "{version:?}");
            let decoded = decode_prefix(&expected, version).unwrap();
            assert_eq!(decoded.message, menu);
            assert!(decoded.remaining_bits.iter().all(|bit| !bit));
            for length in 0..expected.len() {
                assert_eq!(
                    decode_prefix(&expected[..length], version),
                    Err(Error::Truncated)
                );
            }
        }
    }
    #[test]
    fn service_menu_requires_exact_revision_extensions_and_rejects_premature_fields() {
        let base = [0x08, 0xa5, 0x86, 0x06, 0xf3, 0x6a, 0xab, 0x80];
        let mut menu = decode_prefix(&base, ProtocolVersion::Is136RevA)
            .unwrap()
            .message;
        assert_eq!(menu.encode(ProtocolVersion::Pv1), Err(Error::Revision));
        menu.pv1 = Some(ServiceMenuPv1::default());
        assert_eq!(
            menu.encode(ProtocolVersion::Is136RevA),
            Err(Error::Revision)
        );
        assert!(menu.encode(ProtocolVersion::Pv1).is_ok());
        assert_eq!(menu.encode(ProtocolVersion::Pv2), Err(Error::Revision));
        menu.sms_broadcast = Some(false);
        assert_eq!(menu.encode(ProtocolVersion::Pv1), Err(Error::Revision));
        assert!(menu.encode(ProtocolVersion::Pv5).is_ok());
        assert_eq!(menu.encode(ProtocolVersion::Pv6), Err(Error::Revision));
        menu.extended_characters = Some([false; EXTENDED_CHARACTER_FLAGS]);
        assert_eq!(menu.encode(ProtocolVersion::Pv5), Err(Error::Revision));
        assert!(menu.encode(ProtocolVersion::Pv7).is_ok());
        for version in [ProtocolVersion::Eia553OrIs54A, ProtocolVersion::Tia627] {
            assert_eq!(menu.encode(version), Err(Error::Revision));
            assert_eq!(decode_prefix(&base, version), Err(Error::Revision));
        }
    }
}
