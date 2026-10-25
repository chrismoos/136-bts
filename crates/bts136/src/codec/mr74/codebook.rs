pub mod search;
use super::SUBFRAME_SAMPLES;
use std::{error::Error as StdError, fmt};
const PULSES: usize = 4;
const TRACKS: usize = 5;
const GRAY_BITS: usize = 3;
const GRAY_MASK: u16 = (1 << GRAY_BITS) - 1;
const LAST_TRACK_SHIFT: usize = (PULSES - 1) * GRAY_BITS;
const LAST_POSITION_SHIFT: usize = LAST_TRACK_SHIFT + 1;
const MAX_POSITIONS: u16 = (1 << (LAST_POSITION_SHIFT + GRAY_BITS)) - 1;
const MAX_SIGNS: u16 = (1 << PULSES) - 1;
const INVERSE_GRAY: [usize; 8] = [0, 1, 3, 2, 5, 6, 4, 7];
const POSITIVE_Q13: i16 = 8191;
const NEGATIVE_Q13: i16 = -8192;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Positions(u16),
    Signs(u16),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MR74 fixed codebook failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Codebook {
    samples_q13: [i16; SUBFRAME_SAMPLES],
    positions: [usize; PULSES],
}
impl Codebook {
    // TS 26.073 decode_4i40_17bits and dgray. Positive pulses use 8191 and negative pulses use -8192 in Q13.
    pub fn try_new(pulse_positions: u16, pulse_signs: u16) -> Result<Self, Error> {
        if pulse_positions > MAX_POSITIONS {
            return Err(Error::Positions(pulse_positions));
        }
        if pulse_signs > MAX_SIGNS {
            return Err(Error::Signs(pulse_signs));
        }
        let positions = std::array::from_fn(|pulse| {
            let (shift, track) = if pulse == PULSES - 1 {
                (
                    LAST_POSITION_SHIFT,
                    pulse + usize::from((pulse_positions >> LAST_TRACK_SHIFT) & 1),
                )
            } else {
                (pulse * GRAY_BITS, pulse)
            };
            INVERSE_GRAY[usize::from((pulse_positions >> shift) & GRAY_MASK)] * TRACKS + track
        });
        let mut samples_q13 = [0; SUBFRAME_SAMPLES];
        for (pulse, position) in positions.iter().enumerate() {
            samples_q13[*position] = if pulse_signs & (1 << pulse) != 0 {
                POSITIVE_Q13
            } else {
                NEGATIVE_Q13
            };
        }
        Ok(Self {
            samples_q13,
            positions,
        })
    }
    pub fn samples_q13(&self) -> &[i16; SUBFRAME_SAMPLES] {
        &self.samples_q13
    }
    pub fn positions(&self) -> &[usize; PULSES] {
        &self.positions
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_position_and_sign_combination_matches_fixed_codebook_fingerprint() {
        const EXPECTED: u64 = 0xd1a32314d9459e25;
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;
        let mut hash = FNV_OFFSET;
        for position in 0..=MAX_POSITIONS {
            for signs in 0..=MAX_SIGNS {
                let codebook = Codebook::try_new(position, signs).unwrap();
                assert_eq!(
                    codebook
                        .samples_q13
                        .iter()
                        .filter(|sample| **sample != 0)
                        .count(),
                    PULSES
                );
                for byte in codebook
                    .samples_q13
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes())
                {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
                }
            }
        }
        assert_eq!(hash, EXPECTED);
    }
    #[test]
    fn gray_positions_sign_order_and_last_track_match_exact_vectors() {
        for (position, signs, pulses) in [
            (0, 0, [(0, -8192), (1, -8192), (2, -8192), (3, -8192)]),
            (4, 5, [(25, 8191), (1, -8192), (2, 8191), (3, -8192)]),
            (511, 1, [(35, 8191), (36, -8192), (37, -8192), (3, -8192)]),
            (8191, 10, [(35, -8192), (36, 8191), (37, -8192), (39, 8191)]),
        ] {
            let mut expected = [0; SUBFRAME_SAMPLES];
            for (position, sample) in pulses {
                expected[position] = sample;
            }
            assert_eq!(
                Codebook::try_new(position, signs).unwrap().samples_q13(),
                &expected
            );
        }
        assert_eq!(
            Codebook::try_new(MAX_POSITIONS + 1, 0),
            Err(Error::Positions(MAX_POSITIONS + 1))
        );
        assert_eq!(
            Codebook::try_new(0, MAX_SIGNS + 1),
            Err(Error::Signs(MAX_SIGNS + 1))
        );
    }
}
