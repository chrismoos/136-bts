// TIA/EIA-136-132 §1.1 reserves zero for traffic-channel DVCC assignments.
pub const RESERVED_TRAFFIC_DVCC: u8 = 0;
const PARITY_BITS: u32 = 4;
const PARITY_MASK: u16 = (1 << PARITY_BITS) - 1;
const CODE_BITS: u32 = 12;
const CODE_MASK: u16 = (1 << CODE_BITS) - 1;
const LOCATOR_BITS: u32 = 7;
const LOCATOR_MASK: u8 = (1 << LOCATOR_BITS) - 1;
const GENERATOR: u16 = 0b1_0011;

// IS-136.2-A §1.2.5, systematic shortened Hamming code with X^4 + X + 1.
pub fn encode_dvcc(dvcc: u8) -> u16 {
    let systematic = u16::from(dvcc) << PARITY_BITS;
    let mut remainder = systematic;
    for shift in (PARITY_BITS..CODE_BITS).rev() {
        if remainder & (1 << shift) != 0 {
            remainder ^= GENERATOR << (shift - PARITY_BITS);
        }
    }
    systematic | remainder
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedDvcc {
    pub dvcc: u8,
    pub corrected_bits: u32,
}

pub fn decode_dvcc(code: u16) -> Option<DecodedDvcc> {
    if code & !CODE_MASK != 0 {
        return None;
    }
    for dvcc in u8::MIN..=u8::MAX {
        let corrected_bits = (code ^ encode_dvcc(dvcc)).count_ones();
        if corrected_bits <= 1 {
            return Some(DecodedDvcc {
                dvcc,
                corrected_bits,
            });
        }
    }
    None
}

// IS-136.2-A §1.2.6, omit d7 and complement the four parity bits.
pub fn encode_locator(locator: u8) -> Option<u16> {
    if locator & !LOCATOR_MASK != 0 {
        return None;
    }
    Some(encode_dvcc(locator) ^ PARITY_MASK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polynomial_basis_vectors() {
        let expected = [0x013, 0x026, 0x04c, 0x08b, 0x105, 0x20a, 0x407, 0x80e];
        for (bit, expected) in expected.into_iter().enumerate() {
            assert_eq!(encode_dvcc(1 << bit), expected);
        }
        assert_eq!(encode_dvcc(0), 0);
        assert_eq!(encode_locator(0), Some(0x00f));
        assert_eq!(encode_locator(0x80), None);
    }

    #[test]
    fn corrects_every_single_bit_error() {
        for dvcc in u8::MIN..=u8::MAX {
            let code = encode_dvcc(dvcc);
            assert_eq!(
                decode_dvcc(code),
                Some(DecodedDvcc {
                    dvcc,
                    corrected_bits: 0
                })
            );
            for bit in 0..CODE_BITS {
                assert_eq!(
                    decode_dvcc(code ^ (1 << bit)),
                    Some(DecodedDvcc {
                        dvcc,
                        corrected_bits: 1
                    })
                );
            }
        }
        assert_eq!(decode_dvcc(1 << CODE_BITS), None);
    }
}
