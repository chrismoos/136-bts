pub mod open_loop;
pub mod search;
use super::{Parameters, SUBFRAMES};
use std::{error::Error as StdError, fmt};
const INITIAL_LAG_SAMPLES: u16 = 40;
const FRACTIONAL_INDEX_LIMIT: u8 = 197;
const DIRECT_LAG_OFFSET: u16 = 112;
const FRACTIONAL_LAG_BASE: u16 = 19;
const INDEX_BIAS: u16 = 2;
const FRACTION_DENOMINATOR: u16 = 3;
const MIN_SEARCH_LAG_SAMPLES: u16 = 20;
const MAX_SEARCH_LAG_SAMPLES: u16 = 143;
const SEARCH_RADIUS_SAMPLES: u16 = 5;
const SEARCH_SPAN_SAMPLES: u16 = 9;
const DELTA_INDEX_BITS: u32 = 5;
const MAX_DELTA_INDEX: u8 = (1 << DELTA_INDEX_BITS) - 1;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fraction {
    MinusThird,
    Zero,
    PlusThird,
}
impl Fraction {
    pub fn thirds(self) -> i16 {
        match self {
            Self::MinusThird => -1,
            Self::Zero => 0,
            Self::PlusThird => 1,
        }
    }
}
const FRACTIONS: [Fraction; FRACTION_DENOMINATOR as usize] =
    [Fraction::PlusThird, Fraction::MinusThird, Fraction::Zero];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaIndexError(pub u8);
impl fmt::Display for DeltaIndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MR74 differential pitch index {} exceeds {}",
            self.0, MAX_DELTA_INDEX
        )
    }
}
impl StdError for DeltaIndexError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PitchLag {
    samples: u16,
    fraction: Fraction,
}
impl PitchLag {
    // TS 26.073 Dec_lag3 uses one-third resolution below index 197 and integer lags above it.
    pub fn from_absolute(index: u8) -> Self {
        if index < FRACTIONAL_INDEX_LIMIT {
            Self {
                samples: (u16::from(index) + INDEX_BIAS) / FRACTION_DENOMINATOR
                    + FRACTIONAL_LAG_BASE,
                fraction: FRACTIONS[usize::from(index) % FRACTIONS.len()],
            }
        } else {
            Self {
                samples: u16::from(index) - DIRECT_LAG_OFFSET,
                fraction: Fraction::Zero,
            }
        }
    }
    pub fn from_relative(index: u8, previous: Self) -> Result<Self, DeltaIndexError> {
        if index > MAX_DELTA_INDEX {
            return Err(DeltaIndexError(index));
        }
        Ok(Self::relative(index, previous))
    }
    fn relative(index: u8, previous: Self) -> Self {
        let minimum = previous
            .samples
            .saturating_sub(SEARCH_RADIUS_SAMPLES)
            .clamp(
                MIN_SEARCH_LAG_SAMPLES,
                MAX_SEARCH_LAG_SAMPLES - SEARCH_SPAN_SAMPLES,
            );
        Self {
            samples: minimum + (u16::from(index) + INDEX_BIAS) / FRACTION_DENOMINATOR - 1,
            fraction: FRACTIONS[usize::from(index) % FRACTIONS.len()],
        }
    }
    /// Decoder endpoints include integer lags 19 and 144 even though the encoder search bounds are 20 and 143.
    pub fn samples(self) -> u16 {
        self.samples
    }
    pub fn fraction(self) -> Fraction {
        self.fraction
    }
}
pub fn decode_frame(parameters: &Parameters) -> [PitchLag; SUBFRAMES] {
    let subframes = parameters.subframes();
    let first = PitchLag::from_absolute(subframes[0].pitch_index as u8);
    let second = PitchLag::relative(subframes[1].pitch_index as u8, first);
    let third = PitchLag::from_absolute(subframes[2].pitch_index as u8);
    let fourth = PitchLag::relative(subframes[3].pitch_index as u8, third);
    [first, second, third, fourth]
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PitchDecoder {
    previous: PitchLag,
}
impl Default for PitchDecoder {
    fn default() -> Self {
        Self {
            previous: PitchLag {
                samples: INITIAL_LAG_SAMPLES,
                fraction: Fraction::Zero,
            },
        }
    }
}
impl PitchDecoder {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn decode(&mut self, parameters: &Parameters) -> [PitchLag; SUBFRAMES] {
        let lags = decode_frame(parameters);
        self.previous = lags[SUBFRAMES - 1];
        lags
    }
    /// TS 26.073 Decoder_amr increments the preceding integer lag below 143 on each erased MR74 subframe and clears its fraction.
    pub fn conceal(&mut self) -> [PitchLag; SUBFRAMES] {
        std::array::from_fn(|_| {
            if self.previous.samples < MAX_SEARCH_LAG_SAMPLES {
                self.previous.samples += 1;
            }
            self.previous.fraction = Fraction::Zero;
            self.previous
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_and_all_differential_ranges_match_pitch_fingerprint() {
        const EXPECTED: u64 = 0x43c6a07e72e7d16b;
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;
        let mut hash = FNV_OFFSET;
        let mut include = |lag: PitchLag| {
            for byte in (lag.samples as i16)
                .to_le_bytes()
                .into_iter()
                .chain(lag.fraction.thirds().to_le_bytes())
            {
                hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
            }
        };
        for index in u8::MIN..=u8::MAX {
            include(PitchLag::from_absolute(index));
        }
        for samples in 19..=144 {
            let previous = PitchLag {
                samples,
                fraction: Fraction::Zero,
            };
            for index in 0..=MAX_DELTA_INDEX {
                include(PitchLag::from_relative(index, previous).unwrap());
            }
        }
        assert_eq!(hash, EXPECTED);
    }
    #[test]
    fn frame_uses_absolute_first_and_third_then_relative_second_and_fourth() {
        let mut values = [0; super::super::PARAMETER_COUNT];
        values[3] = 196;
        values[7] = 31;
        values[11] = 255;
        values[15] = 0;
        let parameters = Parameters::try_new(values).unwrap();
        let lags = decode_frame(&parameters);
        assert_eq!(lags.map(PitchLag::samples), [85, 90, 143, 133]);
        assert_eq!(lags.map(|lag| lag.fraction.thirds()), [-1, -1, 0, 1]);
        assert_eq!(PitchLag::from_absolute(197).samples(), 85);
        assert_eq!(PitchLag::from_absolute(197).fraction(), Fraction::Zero);
        assert_eq!(
            PitchLag::from_relative(32, lags[0]),
            Err(DeltaIndexError(32))
        );
    }
    #[test]
    fn pitch_erasures_match_reference_after_every_absolute_and_relative_index() {
        let mut hash = 0xcbf29ce484222325u64;
        let mut include = |lag: PitchLag| {
            for value in [lag.samples as i16, lag.fraction.thirds()] {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        };
        for absolute in u8::MIN..=u8::MAX {
            for delta in 0..=MAX_DELTA_INDEX {
                let mut values = [0; super::super::PARAMETER_COUNT];
                for subframe in 0..SUBFRAMES {
                    values[super::super::LSF_INDICES + subframe * 4] =
                        u16::from(if subframe % 2 == 0 { absolute } else { delta });
                }
                let parameters = Parameters::try_new(values).unwrap();
                let mut decoder = PitchDecoder::default();
                for lag in decoder.decode(&parameters) {
                    include(lag);
                }
                for _ in 0..40 {
                    for lag in decoder.conceal() {
                        include(lag);
                    }
                }
                assert_eq!(decoder.decode(&parameters), decode_frame(&parameters));
                decoder.reset();
                assert_eq!(decoder, PitchDecoder::default());
            }
        }
        assert_eq!(hash, 0x3cf7511ca6d29052);
    }
    #[test]
    fn initial_erasure_and_above_search_endpoint_preserve_reference_behavior() {
        let mut decoder = PitchDecoder::default();
        assert_eq!(decoder.conceal().map(PitchLag::samples), [41, 42, 43, 44]);
        decoder.previous =
            PitchLag::from_relative(MAX_DELTA_INDEX, PitchLag::from_absolute(u8::MAX)).unwrap();
        assert_eq!(decoder.previous.samples, 144);
        for lag in decoder.conceal() {
            assert_eq!(lag.samples, 144);
            assert_eq!(lag.fraction, Fraction::Zero);
        }
    }
}
