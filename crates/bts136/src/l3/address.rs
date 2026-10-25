use std::{error::Error, fmt};

const HEADER_OCTETS: usize = 1;
const TON_SHIFT: u8 = 5;
const PLAN_SHIFT: u8 = 1;
const PLAN_MASK: u8 = 0xf;
const ENCODING_MASK: u8 = 1;
const NIBBLE_BITS: u8 = 4;
const NIBBLE_MASK: u8 = 0xf;
const FILLER: u8 = 0;
const ZERO_DIGIT: u8 = 10;
const STAR_DIGIT: u8 = 11;
const HASH_DIGIT: u8 = 12;
const DPC_SSN_PLAN: u8 = 12;
const BINARY_INTERNET_PLAN: u8 = 13;
const IPV4_OCTETS: usize = 4;
const IPV6_OCTETS: usize = 16;
const INTERNET_OCTETS: [usize; 2] = [IPV4_OCTETS, IPV6_OCTETS];
const VALID_NUMBER_TYPES: [u8; 6] = [0, 1, 2, 3, 4, 6];
const VALID_PLANS: [u8; 9] = [0, 1, 3, 4, 6, 9, 12, 13, 14];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressValue {
    Tbcd(String),
    Ira(Vec<u8>),
    DpcSsn(Vec<u8>),
    BinaryInternet(std::net::IpAddr),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressInfo {
    pub number_type: u8,
    pub numbering_plan: u8,
    pub address: AddressValue,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedAddress {
    pub address: AddressInfo,
    pub consumed_octets: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressError {
    Truncated,
    Length,
    NumberType,
    NumberingPlan,
    Encoding,
    Digit,
    Filler,
}
impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid IS-136 Address Info: {self:?}")
    }
}
impl Error for AddressError {}
impl AddressInfo {
    // IS-136.1-A §6.5 Address Info. First digit precedes second in the transmitted octet.
    pub fn encode(&self) -> Result<Vec<u8>, AddressError> {
        validate(self.number_type, self.numbering_plan)?;
        let (encoding, data) = match &self.address {
            AddressValue::Tbcd(digits) => {
                if matches!(self.numbering_plan, DPC_SSN_PLAN | BINARY_INTERNET_PLAN) {
                    return Err(AddressError::Encoding);
                }
                let digits: Vec<u8> = digits.bytes().map(encode_digit).collect::<Result<_, _>>()?;
                let data = digits
                    .chunks(2)
                    .map(|pair| (pair[0] << NIBBLE_BITS) | pair.get(1).copied().unwrap_or(FILLER))
                    .collect();
                (ENCODING_MASK, data)
            }
            AddressValue::Ira(data) => {
                if matches!(self.numbering_plan, DPC_SSN_PLAN | BINARY_INTERNET_PLAN)
                    || data.iter().any(|byte| !byte.is_ascii())
                {
                    return Err(AddressError::Encoding);
                }
                (0, data.clone())
            }
            AddressValue::BinaryInternet(address) => {
                if self.numbering_plan != BINARY_INTERNET_PLAN {
                    return Err(AddressError::Encoding);
                }
                (
                    0,
                    match address {
                        std::net::IpAddr::V4(value) => value.octets().to_vec(),
                        std::net::IpAddr::V6(value) => value.octets().to_vec(),
                    },
                )
            }
            AddressValue::DpcSsn(data) => {
                if self.numbering_plan != DPC_SSN_PLAN {
                    return Err(AddressError::Encoding);
                }
                (0, data.clone())
            }
        };
        let length = u8::try_from(HEADER_OCTETS + data.len()).map_err(|_| AddressError::Length)?;
        let mut bytes = vec![
            length,
            (self.number_type << TON_SHIFT) | (self.numbering_plan << PLAN_SHIFT) | encoding,
        ];
        bytes.extend(data);
        Ok(bytes)
    }
}
pub fn decode_prefix(bytes: &[u8]) -> Result<DecodedAddress, AddressError> {
    let length = usize::from(*bytes.first().ok_or(AddressError::Truncated)?);
    if length < HEADER_OCTETS {
        return Err(AddressError::Length);
    }
    let content = bytes.get(1..length + 1).ok_or(AddressError::Truncated)?;
    let header = content[0];
    let number_type = header >> TON_SHIFT;
    let numbering_plan = (header >> PLAN_SHIFT) & PLAN_MASK;
    validate(number_type, numbering_plan)?;
    let data = &content[HEADER_OCTETS..];
    // TIA-136-123-E §6.5 encodes plan thirteen as four or sixteen binary IP octets.
    let address = if numbering_plan == BINARY_INTERNET_PLAN {
        if header & ENCODING_MASK != 0 || !INTERNET_OCTETS.contains(&data.len()) {
            return Err(AddressError::Encoding);
        }
        let value = match data.len() {
            IPV4_OCTETS => std::net::IpAddr::V4(std::net::Ipv4Addr::from(
                <[u8; IPV4_OCTETS]>::try_from(data).expect("IPv4 length checked"),
            )),
            IPV6_OCTETS => std::net::IpAddr::V6(std::net::Ipv6Addr::from(
                <[u8; IPV6_OCTETS]>::try_from(data).expect("IPv6 length checked"),
            )),
            _ => unreachable!(),
        };
        AddressValue::BinaryInternet(value)
    } else if numbering_plan == DPC_SSN_PLAN {
        if header & ENCODING_MASK != 0 {
            return Err(AddressError::Encoding);
        }
        AddressValue::DpcSsn(data.to_vec())
    } else if header & ENCODING_MASK == 0 {
        if data.iter().any(|byte| !byte.is_ascii()) {
            return Err(AddressError::Encoding);
        }
        AddressValue::Ira(data.to_vec())
    } else {
        let mut digits = String::new();
        for (index, byte) in data.iter().enumerate() {
            digits.push(decode_digit(byte >> NIBBLE_BITS)?);
            let second = byte & NIBBLE_MASK;
            if second == FILLER {
                if index + 1 != data.len() {
                    return Err(AddressError::Filler);
                }
            } else {
                digits.push(decode_digit(second)?);
            }
        }
        AddressValue::Tbcd(digits)
    };
    Ok(DecodedAddress {
        address: AddressInfo {
            number_type,
            numbering_plan,
            address,
        },
        consumed_octets: length + 1,
    })
}
fn validate(number_type: u8, plan: u8) -> Result<(), AddressError> {
    if !VALID_NUMBER_TYPES.contains(&number_type) {
        return Err(AddressError::NumberType);
    }
    if !VALID_PLANS.contains(&plan) {
        return Err(AddressError::NumberingPlan);
    }
    Ok(())
}
fn encode_digit(digit: u8) -> Result<u8, AddressError> {
    match digit {
        b'1'..=b'9' => Ok(digit - b'0'),
        b'0' => Ok(ZERO_DIGIT),
        b'*' => Ok(STAR_DIGIT),
        b'#' => Ok(HASH_DIGIT),
        _ => Err(AddressError::Digit),
    }
}
fn decode_digit(digit: u8) -> Result<char, AddressError> {
    match digit {
        1..=9 => Ok(char::from(b'0' + digit)),
        ZERO_DIGIT => Ok('0'),
        STAR_DIGIT => Ok('*'),
        HASH_DIGIT => Ok('#'),
        FILLER => Err(AddressError::Filler),
        _ => Err(AddressError::Digit),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_tbcd_vector_covers_zero_star_hash_odd_filler_and_prefix_length() {
        let address = AddressInfo {
            number_type: 0,
            numbering_plan: 1,
            address: AddressValue::Tbcd("120*#".into()),
        };
        assert_eq!(address.encode().unwrap(), [4, 3, 0x12, 0xab, 0xc0]);
        assert_eq!(
            decode_prefix(&[4, 3, 0x12, 0xab, 0xc0, 0xff]).unwrap(),
            DecodedAddress {
                address,
                consumed_octets: 5
            }
        );
        assert_eq!(
            decode_prefix(&[3, 3, 0x10, 0x12]),
            Err(AddressError::Filler)
        );
        assert_eq!(decode_prefix(&[2, 3, 0x01]), Err(AddressError::Filler));
        assert_eq!(decode_prefix(&[2, 3, 0xfd]), Err(AddressError::Digit));
    }
    #[test]
    fn ira_binary_bounds_and_reserved_fields_are_checked() {
        for address in [
            AddressValue::Ira(b"5551234".to_vec()),
            AddressValue::DpcSsn(vec![0x80, 0xff]),
        ] {
            let info = AddressInfo {
                number_type: 0,
                numbering_plan: if matches!(address, AddressValue::DpcSsn(_)) {
                    DPC_SSN_PLAN
                } else {
                    1
                },
                address,
            };
            let bytes = info.encode().unwrap();
            assert_eq!(decode_prefix(&bytes).unwrap().address, info);
        }
        assert_eq!(decode_prefix(&[0]), Err(AddressError::Length));
        assert_eq!(decode_prefix(&[2, 3]), Err(AddressError::Truncated));
        assert_eq!(decode_prefix(&[1, 0xa0]), Err(AddressError::NumberType));
        assert_eq!(decode_prefix(&[1, 4]), Err(AddressError::NumberingPlan));
        assert_eq!(decode_prefix(&[2, 2, 0x80]), Err(AddressError::Encoding));
        assert_eq!(decode_prefix(&[1, 25]), Err(AddressError::Encoding));
        let too_long = AddressInfo {
            number_type: 0,
            numbering_plan: 1,
            address: AddressValue::Ira(vec![b'1'; 255]),
        };
        assert_eq!(too_long.encode(), Err(AddressError::Length));
    }
}

#[cfg(test)]
mod internet_tests {
    use super::*;
    #[test]
    fn binary_ipv4_and_ipv6_match_fixed_network_order_vectors() {
        for (address, wire) in [
            ("192.0.2.1", vec![5, 0x1a, 192, 0, 2, 1]),
            (
                "2001:db8::1",
                vec![
                    17, 0x1a, 0x20, 1, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
                ],
            ),
        ] {
            let value = AddressInfo {
                number_type: 0,
                numbering_plan: BINARY_INTERNET_PLAN,
                address: AddressValue::BinaryInternet(address.parse().unwrap()),
            };
            assert_eq!(value.encode().unwrap(), wire);
            assert_eq!(decode_prefix(&wire).unwrap().address, value);
            for length in 0..wire.len() {
                assert!(decode_prefix(&wire[..length]).is_err());
            }
            let mut extended = wire.clone();
            extended.push(255);
            assert_eq!(
                decode_prefix(&extended).unwrap().consumed_octets,
                wire.len()
            );
        }
    }
    #[test]
    fn binary_internet_accepts_only_four_or_sixteen_octets_with_clear_encoding() {
        for count in 0..=254 {
            for encoding in 0..2 {
                let mut wire = vec![
                    (count + 1) as u8,
                    (BINARY_INTERNET_PLAN << PLAN_SHIFT) | encoding,
                ];
                wire.extend(vec![0; count]);
                assert_eq!(
                    decode_prefix(&wire).is_ok(),
                    encoding == 0 && INTERNET_OCTETS.contains(&count),
                    "{count}/{encoding}"
                );
            }
        }
        for address in [
            AddressValue::Tbcd("1234".into()),
            AddressValue::Ira(b"1.2.3.4".to_vec()),
            AddressValue::DpcSsn(vec![0; 4]),
        ] {
            assert_eq!(
                AddressInfo {
                    number_type: 0,
                    numbering_plan: BINARY_INTERNET_PLAN,
                    address
                }
                .encode(),
                Err(AddressError::Encoding)
            );
        }
        assert_eq!(
            AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::BinaryInternet("::1".parse().unwrap())
            }
            .encode(),
            Err(AddressError::Encoding)
        );
    }
}
