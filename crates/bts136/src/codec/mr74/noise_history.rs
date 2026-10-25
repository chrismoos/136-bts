use super::{
    decoder::FRAME_SAMPLES,
    logarithm,
    spectral::{self, ConversionError, LPC_ORDER, Spectrum},
};
pub const HISTORY_FRAMES: usize = 8;
const INITIAL_LOG_ENERGY_Q11: i16 = 3500;
const HISTORY_AVERAGE_SHIFT: u32 = 3;
const LOG_EXPONENT_SHIFT: u32 = 10;
const LOG_FRACTION_SHIFT: u32 = 5;
const FRAME_ENERGY_OFFSET: i16 = 8521;
const MODE_ENERGY_ADJUST_Q11: i16 = -440;
const Q15_SHIFT: u32 = 15;
const SOFT_DEVIATION_LIMIT: i16 = 655;
const HARD_DEVIATION_LIMIT: i16 = 1310;
const SOFT_LIMIT_SHIFT: u32 = 2;
// TS 26.073 dtx_dec attenuates upper-frequency history deviations before soft and hard limiting.
const DEVIATION_SCALE_Q15: [i16; LPC_ORDER] =
    [20000, 20000, 20000, 20000, 20000, 18000, 16384, 8192, 0, 0];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackwardNoise {
    pub spectrum: Spectrum,
    pub log_energy_q11: i16,
    pub deviations_q15: [[i16; LPC_ORDER]; HISTORY_FRAMES],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoiseHistory {
    frequencies: [[i16; LPC_ORDER]; HISTORY_FRAMES],
    energies: [i16; HISTORY_FRAMES],
    cursor: usize,
}
impl Default for NoiseHistory {
    fn default() -> Self {
        Self {
            frequencies: [spectral::INITIAL_LSF_Q15; HISTORY_FRAMES],
            energies: [INITIAL_LOG_ENERGY_Q11; HISTORY_FRAMES],
            cursor: 0,
        }
    }
}
impl NoiseHistory {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 dtx_dec_activity_update records normalized frequencies and pre-postfilter speech energy.
    pub fn record(
        &mut self,
        frequencies: &[i16; LPC_ORDER],
        samples: &[i16; FRAME_SAMPLES],
    ) -> Result<(), ConversionError> {
        let cosines = spectral::lsf_to_lsp(frequencies)?;
        spectral::lsp_to_lsf(&cosines)?;
        let energy = samples.iter().fold(0i32, |sum, &sample| {
            sum.saturating_add((i32::from(sample) * i32::from(sample)).saturating_mul(2))
        });
        let log = logarithm::log2(energy);
        let log_energy = ((i32::from(log.exponent) << LOG_EXPONENT_SHIFT) as i16)
            .saturating_add((log.fraction_q15 >> LOG_FRACTION_SHIFT) as i16)
            .saturating_sub(FRAME_ENERGY_OFFSET);
        self.cursor = (self.cursor + 1) % HISTORY_FRAMES;
        self.frequencies[self.cursor] = *frequencies;
        self.energies[self.cursor] = log_energy;
        Ok(())
    }
    /// TS 26.073 dtx_dec duplicates the latest frame into the next slot before backward hangover analysis.
    pub fn analyze(&mut self) -> BackwardNoise {
        let next = (self.cursor + 1) % HISTORY_FRAMES;
        self.frequencies[next] = self.frequencies[self.cursor];
        self.energies[next] = self.energies[self.cursor];
        let mean = std::array::from_fn(|index| {
            (self
                .frequencies
                .iter()
                .map(|frame| i32::from(frame[index]))
                .sum::<i32>()
                >> HISTORY_AVERAGE_SHIFT) as i16
        });
        let lsp_q15 = spectral::lsf_to_lsp(&mean)
            .expect("averages of validated normalized frequencies stay in the conversion domain");
        let log_energy_q11 = self
            .energies
            .iter()
            .fold(0i16, |sum, &energy| {
                sum.saturating_add(energy >> HISTORY_AVERAGE_SHIFT)
            })
            .saturating_sub(MODE_ENERGY_ADJUST_Q11);
        let deviations_q15 = std::array::from_fn(|frame| {
            std::array::from_fn(|index| {
                let difference = self.frequencies[frame][index].saturating_sub(mean[index]);
                let scaled = ((i32::from(difference) * i32::from(DEVIATION_SCALE_Q15[index]))
                    >> Q15_SHIFT) as i16;
                let mut magnitude = scaled.saturating_abs();
                if magnitude > SOFT_DEVIATION_LIMIT {
                    magnitude = SOFT_DEVIATION_LIMIT
                        .saturating_add((magnitude - SOFT_DEVIATION_LIMIT) >> SOFT_LIMIT_SHIFT);
                }
                magnitude = magnitude.min(HARD_DEVIATION_LIMIT);
                if scaled < 0 { -magnitude } else { magnitude }
            })
        });
        BackwardNoise {
            spectrum: Spectrum {
                lsf_q15: mean,
                lsp_q15,
            },
            log_energy_q11,
            deviations_q15,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::{LSF_INDICES, PARAMETER_COUNT, Parameters, spectral::SpectralDecoder};
    #[test]
    fn activity_history_backward_descriptor_and_deviations_match_native_dtx_decoder() {
        let mut hash = 0xcbf29ce484222325u64;
        let mut include = |value: i16| {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        };
        for pattern in 0..4 {
            let mut history = NoiseHistory::default();
            let mut spectral = SpectralDecoder::default();
            for step in 0..512 {
                let mut values = [0; PARAMETER_COUNT];
                values[..LSF_INDICES].copy_from_slice(&[
                    (step % 256) as u16,
                    ((step * 73 + step / 512) % 512) as u16,
                    ((step * 197 + step / 256) % 512) as u16,
                ]);
                let spectrum = spectral.decode(&Parameters::try_new(values).unwrap());
                let samples = std::array::from_fn(|index| match pattern {
                    0 => 0,
                    1 => {
                        if index == step % FRAME_SAMPLES {
                            30000
                        } else {
                            0
                        }
                    }
                    2 => {
                        if index.is_multiple_of(2) {
                            i16::MAX
                        } else {
                            i16::MIN
                        }
                    }
                    _ => (((index * 347 + step * 997) % 65536) as i32 - 32768) as i16,
                });
                history.record(&spectrum.lsf_q15, &samples).unwrap();
                if step % 3 == 1 {
                    let output = history.analyze();
                    for value in output
                        .spectrum
                        .lsf_q15
                        .into_iter()
                        .chain(output.spectrum.lsp_q15)
                        .chain([output.log_energy_q11])
                        .chain(output.deviations_q15.into_iter().flatten())
                    {
                        include(value);
                    }
                }
                for value in history
                    .frequencies
                    .into_iter()
                    .flatten()
                    .chain(history.energies)
                    .chain([(history.cursor * LPC_ORDER) as i16, history.cursor as i16])
                {
                    include(value);
                }
            }
            history.reset();
            assert_eq!(history, NoiseHistory::default());
        }
        assert_eq!(hash, 0xf90c0da097d0c7c2);
    }
    #[test]
    fn invalid_history_input_preserves_cursor_and_all_frames() {
        let mut history = NoiseHistory::default();
        let before = history.clone();
        assert!(
            history
                .record(&[-1; LPC_ORDER], &[0; FRAME_SAMPLES])
                .is_err()
        );
        assert_eq!(history, before);
        let mut frequencies = spectral::INITIAL_LSF_Q15;
        frequencies.swap(0, 1);
        assert!(history.record(&frequencies, &[0; FRAME_SAMPLES]).is_err());
        assert_eq!(history, before);
    }
}
