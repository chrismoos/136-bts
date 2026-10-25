use super::{MAX_USER_OCTETS, SmsError};
const IRA_ENCODING: u8 = 1;
const USER_ENCODING: u8 = 2;
const ENCODING_SHIFT: u8 = 3;
const BMP_OCTETS: usize = 2;
const MAX_LATIN1: u32 = 0xff;
const MAX_BMP: u32 = 0xffff;
const SURROGATE_START: u16 = 0xd800;
const SURROGATE_END: u16 = 0xdfff;
// TIA/EIA-136-710-C §4.27 assigns these five-bit Encoding Identifier values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InternationalEncoding {
    Latin1 = 3,
    Bmp = 4,
    Iso8859_8 = 5,
    Iso8859_2 = 6,
    Iso8859_3 = 7,
    Iso8859_4 = 8,
    Iso8859_5 = 9,
    Iso8859_6 = 10,
    Iso8859_7 = 11,
    Iso8859_9 = 12,
    Iso8859_10 = 13,
    Iso8859_11 = 14,
    Iso8859_12 = 15,
    Iso8859_13 = 16,
    Iso8859_14 = 17,
    Iso8859_15 = 18,
}
impl TryFrom<u8> for InternationalEncoding {
    type Error = SmsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            3 => Ok(Self::Latin1),
            4 => Ok(Self::Bmp),
            5 => Ok(Self::Iso8859_8),
            6 => Ok(Self::Iso8859_2),
            7 => Ok(Self::Iso8859_3),
            8 => Ok(Self::Iso8859_4),
            9 => Ok(Self::Iso8859_5),
            10 => Ok(Self::Iso8859_6),
            11 => Ok(Self::Iso8859_7),
            12 => Ok(Self::Iso8859_9),
            13 => Ok(Self::Iso8859_10),
            14 => Ok(Self::Iso8859_11),
            15 => Ok(Self::Iso8859_12),
            16 => Ok(Self::Iso8859_13),
            17 => Ok(Self::Iso8859_14),
            18 => Ok(Self::Iso8859_15),
            _ => Err(SmsError::Encoding),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserData {
    Ira(Vec<u8>),
    UserSpecific(Vec<u8>),
    International {
        encoding: InternationalEncoding,
        data: Vec<u8>,
    },
}
impl UserData {
    pub fn from_text(text: &str) -> Result<Self, SmsError> {
        if text.is_ascii() {
            if text.len() > MAX_USER_OCTETS {
                return Err(SmsError::Length);
            }
            return Ok(Self::Ira(text.as_bytes().to_vec()));
        }
        if text.chars().any(|character| u32::from(character) > MAX_BMP) {
            return Err(SmsError::Encoding);
        }
        let characters = text.chars().count();
        let latin1 = text
            .chars()
            .all(|character| u32::from(character) <= MAX_LATIN1);
        let octets = characters
            .checked_mul(if latin1 { 1 } else { BMP_OCTETS })
            .ok_or(SmsError::Length)?;
        if octets > MAX_USER_OCTETS {
            return Err(SmsError::Length);
        }
        Ok(if latin1 {
            Self::International {
                encoding: InternationalEncoding::Latin1,
                data: text.chars().map(|character| character as u8).collect(),
            }
        } else {
            Self::International {
                encoding: InternationalEncoding::Bmp,
                data: text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            }
        })
    }
    pub fn text(&self) -> Option<String> {
        self.octet_len().ok()?;
        match self {
            Self::Ira(data) => String::from_utf8(data.clone()).ok(),
            Self::International {
                encoding: InternationalEncoding::Latin1,
                data,
            } => Some(data.iter().map(|octet| char::from(*octet)).collect()),
            Self::International {
                encoding: InternationalEncoding::Bmp,
                data,
            } => String::from_utf16(
                &data
                    .as_chunks::<BMP_OCTETS>()
                    .0
                    .iter()
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>(),
            )
            .ok(),
            Self::UserSpecific(_) | Self::International { .. } => None,
        }
    }
    pub fn encoding_name(&self) -> &'static str {
        match self {
            Self::Ira(_) => "IRA",
            Self::UserSpecific(_) => "User specific",
            Self::International { encoding, .. } => match encoding {
                InternationalEncoding::Latin1 => "ISO-8859-1",
                InternationalEncoding::Bmp => "ISO-10646 BMP",
                InternationalEncoding::Iso8859_8 => "ISO-8859-8",
                InternationalEncoding::Iso8859_2 => "ISO-8859-2",
                InternationalEncoding::Iso8859_3 => "ISO-8859-3",
                InternationalEncoding::Iso8859_4 => "ISO-8859-4",
                InternationalEncoding::Iso8859_5 => "ISO-8859-5",
                InternationalEncoding::Iso8859_6 => "ISO-8859-6",
                InternationalEncoding::Iso8859_7 => "ISO-8859-7",
                InternationalEncoding::Iso8859_9 => "ISO-8859-9",
                InternationalEncoding::Iso8859_10 => "ISO-8859-10",
                InternationalEncoding::Iso8859_11 => "ISO-8859-11",
                InternationalEncoding::Iso8859_12 => "ISO-8859-12",
                InternationalEncoding::Iso8859_13 => "ISO-8859-13",
                InternationalEncoding::Iso8859_14 => "ISO-8859-14",
                InternationalEncoding::Iso8859_15 => "ISO-8859-15",
            },
        }
    }
    pub fn octet_len(&self) -> Result<usize, SmsError> {
        let data = match self {
            Self::Ira(data) if !data.iter().all(u8::is_ascii) => return Err(SmsError::Encoding),
            Self::Ira(data) | Self::UserSpecific(data) => data,
            Self::International {
                encoding: InternationalEncoding::Bmp,
                data,
            } => {
                if !data.len().is_multiple_of(BMP_OCTETS)
                    || data.as_chunks::<BMP_OCTETS>().0.iter().any(|pair| {
                        (SURROGATE_START..=SURROGATE_END)
                            .contains(&u16::from_be_bytes([pair[0], pair[1]]))
                    })
                {
                    return Err(SmsError::Encoding);
                }
                data
            }
            Self::International { data, .. } => data,
        };
        if data.len() > MAX_USER_OCTETS {
            return Err(SmsError::Length);
        }
        Ok(data.len())
    }
    pub(super) fn encode(&self) -> Result<Vec<u8>, SmsError> {
        let length = self.octet_len()?;
        let (encoding, data) = match self {
            Self::Ira(data) => (IRA_ENCODING, data),
            Self::UserSpecific(data) => (USER_ENCODING, data),
            Self::International { encoding, data } => (*encoding as u8, data),
        };
        let mut bytes = vec![(length + 1) as u8, encoding << ENCODING_SHIFT];
        bytes.extend(data);
        Ok(bytes)
    }
    pub(super) fn from_wire(encoding: u8, data: &[u8]) -> Result<Self, SmsError> {
        let result = match encoding {
            IRA_ENCODING => Self::Ira(data.to_vec()),
            USER_ENCODING => Self::UserSpecific(data.to_vec()),
            _ => Self::International {
                encoding: InternationalEncoding::try_from(encoding)?,
                data: data.to_vec(),
            },
        };
        result.octet_len()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latin1_and_bmp_match_independent_data_unit_vectors() {
        let cases = [
            ("café", vec![5, 0x18, 0x63, 0x61, 0x66, 0xe9]),
            ("A中", vec![5, 0x20, 0x00, 0x41, 0x4e, 0x2d]),
            ("€", vec![3, 0x20, 0x20, 0xac]),
            ("\u{80}", vec![2, 0x18, 0x80]),
        ];
        for (text, wire) in cases {
            let value = UserData::from_text(text).unwrap();
            assert_eq!(value.encode().unwrap(), wire);
            let decoded = UserData::from_wire(wire[1] >> ENCODING_SHIFT, &wire[2..]).unwrap();
            assert_eq!(decoded.text().as_deref(), Some(text));
            assert_eq!(decoded, value);
        }
        assert_eq!(
            UserData::from_text("ASCII").unwrap(),
            UserData::Ira(b"ASCII".to_vec())
        );
    }
    #[test]
    fn every_assigned_identifier_preserves_wire_bytes_and_reserved_identifiers_fail() {
        for identifier in 0..32 {
            let result = UserData::from_wire(identifier, &[0, 0x41]);
            if (1..=18).contains(&identifier) {
                let value = result.unwrap();
                assert_eq!(
                    value.encode().unwrap(),
                    vec![3, identifier << ENCODING_SHIFT, 0, 0x41]
                );
                if identifier >= 5 {
                    assert!(value.text().is_none());
                }
            } else {
                assert_eq!(result, Err(SmsError::Encoding));
            }
        }
    }
    #[test]
    fn bmp_rejects_odd_lengths_and_all_surrogate_units_including_supplementary_pairs() {
        for data in [
            vec![0],
            vec![0xd8, 0],
            vec![0xdc, 0],
            vec![0xd8, 0, 0xdc, 0],
        ] {
            assert_eq!(
                UserData::from_wire(InternationalEncoding::Bmp as u8, &data),
                Err(SmsError::Encoding)
            );
            let value = UserData::International {
                encoding: InternationalEncoding::Bmp,
                data,
            };
            assert_eq!(value.encode(), Err(SmsError::Encoding));
            assert!(value.text().is_none());
        }
        assert_eq!(UserData::from_text("😀"), Err(SmsError::Encoding));
        assert_eq!(UserData::Ira(vec![0x80]).encode(), Err(SmsError::Encoding));
    }
    #[test]
    fn data_unit_limit_counts_encoded_octets_without_truncation() {
        for text in ["é".repeat(254), "中".repeat(127)] {
            let value = UserData::from_text(&text).unwrap();
            assert_eq!(value.octet_len(), Ok(254));
            assert_eq!(value.encode().unwrap()[0], 255);
            assert_eq!(value.text().as_deref(), Some(text.as_str()));
        }
        assert_eq!(UserData::from_text(&"é".repeat(255)), Err(SmsError::Length));
        assert_eq!(
            UserData::from_text(&"中".repeat(128)),
            Err(SmsError::Length)
        );
    }
}
