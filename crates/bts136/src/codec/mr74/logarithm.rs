use std::{error::Error, fmt};
const TABLE_ENTRIES: usize = 33;
const MAX_EXPONENT: u8 = 30;
const MIN_ROUNDED_EXPONENT: i16 = -1;
const FRACTION_BITS: u32 = 15;
const FRACTION_MAX: u16 = (1 << FRACTION_BITS) - 1;
const TABLE_INDEX_SHIFT: u32 = 10;
const NORMALIZED_INDEX_SHIFT: u32 = 25;
const TABLE_BASE: usize = 32;
const WORD_SHIFT: u32 = 16;
const POW2_INTERPOLATION_SHIFT: u32 = 5;
// TS 26.073 Log2 and Pow2 use these Q15 table values and integer interpolation.
const LOG2_TABLE: [i32; TABLE_ENTRIES] = [
    0, 1455, 2866, 4236, 5568, 6863, 8124, 9352, 10549, 11716, 12855, 13967, 15054, 16117, 17156,
    18172, 19167, 20142, 21097, 22033, 22951, 23852, 24735, 25603, 26455, 27291, 28113, 28922,
    29716, 30497, 31266, 32023, 32767,
];
const POW2_TABLE: [i32; TABLE_ENTRIES] = [
    16384, 16743, 17109, 17484, 17867, 18258, 18658, 19066, 19484, 19911, 20347, 20792, 21247,
    21713, 22188, 22674, 23170, 23678, 24196, 24726, 25268, 25821, 26386, 26964, 27554, 28158,
    28774, 29405, 30048, 30706, 31379, 32066, 32767,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Logarithm {
    pub exponent: u8,
    pub fraction_q15: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerDomainError {
    pub exponent: u8,
    pub fraction_q15: u16,
}
impl fmt::Display for PowerDomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fixed-point power exponent {} or Q15 fraction {} is outside its domain",
            self.exponent, self.fraction_q15
        )
    }
}
impl Error for PowerDomainError {}
pub fn log2(value: i32) -> Logarithm {
    if value <= 0 {
        return Logarithm {
            exponent: 0,
            fraction_q15: 0,
        };
    }
    let normalization = value.leading_zeros() - 1;
    let normalized = value << normalization;
    let index = (normalized >> NORMALIZED_INDEX_SHIFT) as usize - TABLE_BASE;
    let interpolation = (normalized >> TABLE_INDEX_SHIFT) & i32::from(FRACTION_MAX);
    let fraction = (LOG2_TABLE[index] << WORD_SHIFT)
        - 2 * (LOG2_TABLE[index] - LOG2_TABLE[index + 1]) * interpolation;
    Logarithm {
        exponent: MAX_EXPONENT - normalization as u8,
        fraction_q15: (fraction >> WORD_SHIFT) as u16,
    }
}
pub fn pow2(exponent: u8, fraction_q15: u16) -> Result<i32, PowerDomainError> {
    if exponent > MAX_EXPONENT || fraction_q15 > FRACTION_MAX {
        return Err(PowerDomainError {
            exponent,
            fraction_q15,
        });
    }
    let index = usize::from(fraction_q15 >> TABLE_INDEX_SHIFT);
    let interpolation =
        (i32::from(fraction_q15) << POW2_INTERPOLATION_SHIFT) & i32::from(FRACTION_MAX);
    let value = (POW2_TABLE[index] << WORD_SHIFT)
        - 2 * (POW2_TABLE[index] - POW2_TABLE[index + 1]) * interpolation;
    let shift = u32::from(MAX_EXPONENT - exponent);
    if shift == 0 {
        return Ok(value);
    }
    Ok((value >> shift) + i32::from(value & (1 << (shift - 1)) != 0))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedPowerDomainError {
    pub exponent: i16,
    pub fraction_q15: u16,
}
impl fmt::Display for SignedPowerDomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fixed-point power exponent {} has invalid Q15 fraction {}",
            self.exponent, self.fraction_q15
        )
    }
}
impl Error for SignedPowerDomainError {}
/// TS 26.073 Pow2 rounds exponent -1 to one, underflows smaller exponents and saturates above thirty.
pub fn pow2_signed(exponent: i16, fraction_q15: u16) -> Result<i32, SignedPowerDomainError> {
    if fraction_q15 > FRACTION_MAX {
        return Err(SignedPowerDomainError {
            exponent,
            fraction_q15,
        });
    }
    if exponent < MIN_ROUNDED_EXPONENT {
        return Ok(0);
    }
    if exponent == MIN_ROUNDED_EXPONENT {
        return Ok(1);
    }
    if exponent > i16::from(MAX_EXPONENT) {
        return Ok(i32::MAX);
    }
    Ok(pow2(exponent as u8, fraction_q15)
        .expect("signed exponent and Q15 fraction satisfy the bounded power domain"))
}
#[cfg(test)]
mod tests {
    use super::*;
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    fn include(hash: &mut u64, bytes: impl IntoIterator<Item = u8>) {
        for byte in bytes {
            *hash = (*hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
        }
    }
    #[test]
    fn logarithm_matches_reference_across_normalization_and_interpolation_boundaries() {
        let mut hash = FNV_OFFSET;
        let mut check = |value| {
            let result = log2(value);
            include(&mut hash, u16::from(result.exponent).to_le_bytes());
            include(&mut hash, result.fraction_q15.to_le_bytes());
        };
        for exponent in 0..=MAX_EXPONENT {
            for fraction in 0..=FRACTION_MAX {
                let value =
                    (1u64 << exponent) + ((u64::from(fraction) << exponent) >> FRACTION_BITS);
                check(value as i32);
            }
        }
        for value in [0, -1, i32::MIN, i32::MAX] {
            check(value);
        }
        assert_eq!(hash, 0x76a99d893245fd44);
    }
    #[test]
    fn power_matches_reference_for_every_valid_exponent_and_fraction() {
        let mut hash = FNV_OFFSET;
        for exponent in 0..=MAX_EXPONENT {
            for fraction in 0..=FRACTION_MAX {
                include(&mut hash, pow2(exponent, fraction).unwrap().to_le_bytes());
            }
        }
        assert_eq!(hash, 0xd65612a34e8e9cfe);
        assert!(pow2(MAX_EXPONENT + 1, 0).is_err());
        assert!(pow2(0, FRACTION_MAX + 1).is_err());
        assert_eq!(pow2(0, 0), Ok(1));
        assert_eq!(pow2(MAX_EXPONENT, 0), Ok(1 << MAX_EXPONENT));
    }
    #[test]
    fn signed_power_matches_reference_across_fraction_domain_and_exponent_extremes() {
        let mut hash = FNV_OFFSET;
        for exponent in (-35i16..=35).chain([i16::MIN, i16::MAX]) {
            for fraction in 0..=FRACTION_MAX {
                include(
                    &mut hash,
                    pow2_signed(exponent, fraction).unwrap().to_le_bytes(),
                );
            }
        }
        assert_eq!(hash, 0x77e7097843269cfe);
        assert!(pow2_signed(0, FRACTION_MAX + 1).is_err());
    }
}
