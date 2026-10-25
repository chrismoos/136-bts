use super::SUBFRAME_SAMPLES;
const INITIAL_SEED: u32 = 0x70816958;
const FEEDBACK_SOURCE_BIT: u32 = 28;
const FEEDBACK_TARGET_BIT: u32 = 30;
const PULSES: usize = 10;
const POSITION_BITS: u8 = 2;
const SIGN_BITS: u8 = 1;
const SPECTRAL_BITS: u8 = 3;
const PULSE_AMPLITUDE: i16 = 4096;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfortNoiseExcitation {
    seed: u32,
}
impl Default for ComfortNoiseExcitation {
    fn default() -> Self {
        Self { seed: INITIAL_SEED }
    }
}
impl ComfortNoiseExcitation {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 dtx_dec draws a three-bit spectral variability index before four comfort-noise subframes.
    pub fn spectral_index(&mut self) -> u8 {
        self.draw_bits(SPECTRAL_BITS)
    }
    /// TS 26.073 build_CN_code generates ten signed pulses of amplitude 4096, one on each track with four possible positions.
    pub fn generate(&mut self) -> [i16; SUBFRAME_SAMPLES] {
        let mut code = [0; SUBFRAME_SAMPLES];
        for track in 0..PULSES {
            let position = usize::from(self.draw_bits(POSITION_BITS)) * PULSES + track;
            code[position] = if self.draw_bits(SIGN_BITS) != 0 {
                PULSE_AMPLITUDE
            } else {
                -PULSE_AMPLITUDE
            };
        }
        code
    }
    fn draw_bits(&mut self, count: u8) -> u8 {
        let mut bits = 0;
        for _ in 0..count {
            let bit = (self.seed & 1) as u8;
            let feedback = bit ^ ((self.seed >> FEEDBACK_SOURCE_BIT) & 1) as u8;
            bits = (bits << 1) | bit;
            self.seed = (self.seed >> 1) | (u32::from(feedback) << FEEDBACK_TARGET_BIT);
        }
        bits
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::SUBFRAMES;
    #[test]
    fn spectral_indices_pulses_and_seed_match_reference_across_seed_basis_and_long_sequences() {
        let mut hash = 0xcbf29ce484222325u64;
        let mut include = |bytes: &[u8]| {
            for &byte in bytes {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        };
        for seed in (0..=FEEDBACK_TARGET_BIT)
            .map(|bit| 1u32 << bit)
            .chain([INITIAL_SEED, 0])
        {
            let mut generator = ComfortNoiseExcitation { seed };
            for _ in 0..1024 {
                include(&u16::from(generator.spectral_index()).to_le_bytes());
                for _ in 0..SUBFRAMES {
                    let code = generator.generate();
                    assert_eq!(code.iter().filter(|sample| **sample != 0).count(), PULSES);
                    for sample in code {
                        include(&sample.to_le_bytes());
                    }
                }
                include(&generator.seed.to_le_bytes());
            }
        }
        assert_eq!(hash, 0xbfe2a30811a91bc6);
    }
    #[test]
    fn reset_restores_spectral_and_excitation_sequence() {
        let mut generator = ComfortNoiseExcitation::default();
        let index = generator.spectral_index();
        let code = generator.generate();
        generator.generate();
        generator.reset();
        assert_eq!(generator.spectral_index(), index);
        assert_eq!(generator.generate(), code);
    }
}
