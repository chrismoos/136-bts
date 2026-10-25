pub mod quantization;
mod tables;
use super::Parameters;
use std::{error::Error, fmt};
pub const LPC_ORDER: usize = 10;
const FIRST_SPLIT_SAMPLES: usize = 3;
const THIRD_SPLIT_START: usize = 6;
const LSF_GAP_Q15: i16 = 205;
const Q15_SHIFT: u32 = 15;
// TS 26.073 D_plsf_reset initializes past LSFs from q_plsf_5, distinct from the MR74 prediction mean.
pub(super) const INITIAL_LSF_Q15: [i16; LPC_ORDER] = [
    1384, 2077, 3420, 5108, 6742, 8122, 9863, 11092, 12714, 13701,
];
const CONCEAL_ALPHA_Q15: i16 = 29491;
const CONCEAL_MEAN_Q15: i16 = 3277;
const COSINE_INDEX_SHIFT: u32 = 8;
const COSINE_OFFSET_MASK: i16 = (1 << COSINE_INDEX_SHIFT) - 1;
const COSINE_POINTS: usize = 65;
// TS 26.073 Lsf_lsp uses this Q15 cosine table and unrounded linear interpolation.
const COSINE: [i16; COSINE_POINTS] = [
    32767, 32729, 32610, 32413, 32138, 31786, 31357, 30853, 30274, 29622, 28899, 28106, 27246,
    26320, 25330, 24279, 23170, 22006, 20788, 19520, 18205, 16846, 15447, 14010, 12540, 11039,
    9512, 7962, 6393, 4808, 3212, 1608, 0, -1608, -3212, -4808, -6393, -7962, -9512, -11039,
    -12540, -14010, -15447, -16846, -18205, -19520, -20788, -22006, -23170, -24279, -25330, -26320,
    -27246, -28106, -28899, -29622, -30274, -30853, -31357, -31786, -32138, -32413, -32610, -32729,
    -32768,
];
const MAX_FREQUENCY_Q15: i16 = 16384;
const ARCCOS_SCALE_SHIFT: u32 = 3;
const WORD_SHIFT: u32 = 16;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
// TS 26.073 Lsp_lsf uses these Q12 inverse-cosine interpolation slopes.
const ARCCOS_SLOPES: [i16; COSINE_POINTS - 1] = [
    -26887, -8812, -5323, -3813, -2979, -2444, -2081, -1811, -1608, -1450, -1322, -1219, -1132,
    -1059, -998, -946, -901, -861, -827, -797, -772, -750, -730, -713, -699, -687, -677, -668,
    -662, -657, -654, -652, -652, -654, -657, -662, -668, -677, -687, -699, -713, -730, -750, -772,
    -797, -827, -861, -901, -946, -998, -1059, -1132, -1219, -1322, -1450, -1608, -1811, -2081,
    -2444, -2979, -3813, -5323, -8812, -26887,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionError {
    Frequency { index: usize, value: i16 },
    UnorderedCosines { index: usize },
}
impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frequency { index, value } => write!(
                f,
                "normalized Q15 frequency {value} at index {index} is outside zero through {MAX_FREQUENCY_Q15}"
            ),
            Self::UnorderedCosines { index } => {
                write!(f, "Q15 cosines must be nonincreasing at index {index}")
            }
        }
    }
}
impl Error for ConversionError {}
pub fn lsf_to_lsp(frequencies: &[i16; LPC_ORDER]) -> Result<[i16; LPC_ORDER], ConversionError> {
    for (index, &value) in frequencies.iter().enumerate() {
        if !(0..=MAX_FREQUENCY_Q15).contains(&value) {
            return Err(ConversionError::Frequency { index, value });
        }
    }
    Ok(frequencies.map(frequency_to_cosine))
}
fn frequency_to_cosine(frequency: i16) -> i16 {
    if frequency == MAX_FREQUENCY_Q15 {
        return COSINE[COSINE_POINTS - 1];
    }
    let index = (frequency >> COSINE_INDEX_SHIFT) as usize;
    let offset = frequency & COSINE_OFFSET_MASK;
    let difference = COSINE[index + 1].saturating_sub(COSINE[index]);
    COSINE[index]
        .saturating_add(((i32::from(difference) * i32::from(offset)) >> COSINE_INDEX_SHIFT) as i16)
}
/// TS 26.073 Lsp_lsf searches a shared table index backward and requires nonincreasing cosine values.
pub fn lsp_to_lsf(cosines: &[i16; LPC_ORDER]) -> Result<[i16; LPC_ORDER], ConversionError> {
    for (index, pair) in cosines.windows(2).enumerate() {
        if pair[0] < pair[1] {
            return Err(ConversionError::UnorderedCosines { index: index + 1 });
        }
    }
    let mut frequencies = [0; LPC_ORDER];
    let mut index = COSINE_POINTS - 2;
    for (component, &value) in cosines.iter().enumerate().rev() {
        while COSINE[index] < value {
            index -= 1;
        }
        let difference = value.saturating_sub(COSINE[index]);
        let interpolated =
            (i32::from(difference) * i32::from(ARCCOS_SLOPES[index])).saturating_mul(2);
        let scaled = (i64::from(interpolated) << ARCCOS_SCALE_SHIFT)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        let offset = (scaled.saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16;
        frequencies[component] = offset.saturating_add((index as i16) << COSINE_INDEX_SHIFT);
    }
    Ok(frequencies)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spectrum {
    /// Frequencies are normalized to the sample rate in Q15, with minimum separation 205.
    pub lsf_q15: [i16; LPC_ORDER],
    pub lsp_q15: [i16; LPC_ORDER],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpectralDecoder {
    residual_q15: [i16; LPC_ORDER],
    previous_lsf_q15: [i16; LPC_ORDER],
}
impl Default for SpectralDecoder {
    fn default() -> Self {
        Self {
            residual_q15: [0; LPC_ORDER],
            previous_lsf_q15: INITIAL_LSF_Q15,
        }
    }
}
impl SpectralDecoder {
    pub(super) fn resume_noise_spectrum(&mut self, spectrum: Spectrum) {
        self.residual_q15 = [0; LPC_ORDER];
        self.previous_lsf_q15 = spectrum.lsf_q15;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// Decodes consecutive good MR74 speech frames. Concealment and DTX require separate predictor updates.
    pub fn decode(&mut self, parameters: &Parameters) -> Spectrum {
        let indices = parameters.lsf_indices();
        let mut residual = [0i16; LPC_ORDER];
        residual[..FIRST_SPLIT_SAMPLES].copy_from_slice(&tables::FIRST[usize::from(indices[0])]);
        residual[FIRST_SPLIT_SAMPLES..THIRD_SPLIT_START]
            .copy_from_slice(&tables::SECOND[usize::from(indices[1])]);
        residual[THIRD_SPLIT_START..].copy_from_slice(&tables::THIRD[usize::from(indices[2])]);
        let lsf_q15 = std::array::from_fn(|index| {
            let prediction = ((i32::from(self.residual_q15[index])
                * i32::from(tables::PREDICTOR[index]))
                >> Q15_SHIFT) as i16;
            residual[index].saturating_add(tables::MEAN[index].saturating_add(prediction))
        });
        self.residual_q15 = residual;
        self.finish(lsf_q15)
    }
    /// TS 26.073 dtx_dec uses temporary MRDTX residual initialization and clears prediction residuals before speech resumes.
    pub fn decode_reference_sid(&mut self, parameters: &super::sid::SidParameters) -> Spectrum {
        let indices = parameters.lsf_indices();
        let mut residual = [0i16; LPC_ORDER];
        residual[..FIRST_SPLIT_SAMPLES].copy_from_slice(&tables::FIRST[usize::from(indices[0])]);
        residual[FIRST_SPLIT_SAMPLES..THIRD_SPLIT_START]
            .copy_from_slice(&tables::SECOND[usize::from(indices[1])]);
        residual[THIRD_SPLIT_START..].copy_from_slice(&tables::THIRD[usize::from(indices[2])]);
        let initial = tables::SID_RESIDUAL[usize::from(parameters.residual_index())];
        let frequencies = std::array::from_fn(|index| {
            residual[index].saturating_add(tables::MEAN[index].saturating_add(initial[index]))
        });
        self.residual_q15 = [0; LPC_ORDER];
        self.finish(frequencies)
    }
    /// TS 26.073 D_plsf_3 shifts erased-frame frequencies toward their means and estimates the next predictor residual.
    pub fn conceal(&mut self) -> Spectrum {
        let lsf_q15 = std::array::from_fn(|index| {
            multiply_q15(self.previous_lsf_q15[index], CONCEAL_ALPHA_Q15)
                .saturating_add(multiply_q15(tables::MEAN[index], CONCEAL_MEAN_Q15))
        });
        self.residual_q15 = std::array::from_fn(|index| {
            let prediction = tables::MEAN[index].saturating_add(multiply_q15(
                self.residual_q15[index],
                tables::PREDICTOR[index],
            ));
            lsf_q15[index].saturating_sub(prediction)
        });
        self.finish(lsf_q15)
    }
    fn finish(&mut self, mut lsf_q15: [i16; LPC_ORDER]) -> Spectrum {
        reorder_frequencies(&mut lsf_q15);
        let lsp_q15 = lsf_q15.map(frequency_to_cosine);
        self.previous_lsf_q15 = lsf_q15;
        Spectrum { lsf_q15, lsp_q15 }
    }
}
pub(super) fn reorder_frequencies(frequencies: &mut [i16; LPC_ORDER]) {
    let mut minimum = LSF_GAP_Q15;
    for frequency in frequencies {
        *frequency = (*frequency).max(minimum);
        minimum = frequency.saturating_add(LSF_GAP_Q15);
    }
}
fn multiply_q15(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
#[cfg(test)]
mod tests {
    use super::*;
    fn parameters(indices: [u16; 3]) -> Parameters {
        let mut values = [0; super::super::PARAMETER_COUNT];
        values[..indices.len()].copy_from_slice(&indices);
        Parameters::try_new(values).unwrap()
    }
    #[test]
    fn split_codebooks_and_consecutive_predictions_match_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        let mut check = |decoder: &mut SpectralDecoder, indices| {
            let spectrum = decoder.decode(&parameters(indices));
            for value in spectrum
                .lsf_q15
                .into_iter()
                .chain(spectrum.lsp_q15)
                .chain(decoder.residual_q15)
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        };
        for (part, entries) in [
            tables::FIRST.len(),
            tables::SECOND.len(),
            tables::THIRD.len(),
        ]
        .into_iter()
        .enumerate()
        {
            for index in 0..entries {
                let mut indices = [0; 3];
                indices[part] = index as u16;
                check(&mut SpectralDecoder::default(), indices);
            }
        }
        let mut decoder = SpectralDecoder::default();
        for step in 0..4096 {
            check(
                &mut decoder,
                [
                    (step % 256) as u16,
                    ((step * 73 + step / 512) % 512) as u16,
                    ((step * 197 + step / 256) % 512) as u16,
                ],
            );
        }
        assert_eq!(hash, 0x746df45eeffe5fc4);
        decoder.reset();
        assert_eq!(decoder, SpectralDecoder::default());
    }
    #[test]
    fn every_codebook_combination_and_prior_residual_stays_in_cosine_table_domain() {
        let mut maximum = [i16::MIN; LPC_ORDER];
        for vector in tables::FIRST {
            for (index, value) in vector.into_iter().enumerate() {
                maximum[index] = maximum[index].max(value);
            }
        }
        for vector in tables::SECOND {
            for (index, value) in vector.into_iter().enumerate() {
                maximum[index + FIRST_SPLIT_SAMPLES] =
                    maximum[index + FIRST_SPLIT_SAMPLES].max(value);
            }
        }
        for vector in tables::THIRD {
            for (index, value) in vector.into_iter().enumerate() {
                maximum[index + THIRD_SPLIT_START] = maximum[index + THIRD_SPLIT_START].max(value);
            }
        }
        let mut upper = LSF_GAP_Q15;
        for (index, &maximum_residual) in maximum.iter().enumerate() {
            let prediction = ((i32::from(maximum_residual) * i32::from(tables::PREDICTOR[index]))
                >> Q15_SHIFT) as i16;
            upper = upper.max(maximum_residual + tables::MEAN[index] + prediction);
            assert!(upper < ((COSINE_POINTS - 1) << COSINE_INDEX_SHIFT) as i16);
            upper += LSF_GAP_Q15;
        }
    }
    #[test]
    fn erasure_bursts_and_good_frame_recovery_match_reference_spectra_and_residuals() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..4 {
            let mut decoder = SpectralDecoder::default();
            for step in 0..4096 {
                let bad = match pattern {
                    0 => step % 7 == 3,
                    1 => step % 32 >= 4,
                    2 => step % 128 < 127,
                    _ => step < 2048,
                };
                let spectrum = if bad {
                    decoder.conceal()
                } else {
                    decoder.decode(&parameters([
                        (step % 256) as u16,
                        ((step * 73 + step / 512) % 512) as u16,
                        ((step * 197 + step / 256) % 512) as u16,
                    ]))
                };
                for value in spectrum
                    .lsf_q15
                    .into_iter()
                    .chain(spectrum.lsp_q15)
                    .chain(decoder.residual_q15)
                {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            decoder.reset();
            assert_eq!(decoder, SpectralDecoder::default());
        }
        assert_eq!(hash, 0x5f16e14d96cb9cb8);
    }
    #[test]
    fn inverse_spectral_conversion_matches_every_cosine_and_ordered_reference_vectors() {
        let mut hash = 0xcbf29ce484222325u64;
        let mut include = |frequencies: [i16; LPC_ORDER]| {
            for value in frequencies {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        };
        for value in i16::MIN..=i16::MAX {
            include(lsp_to_lsf(&[value; LPC_ORDER]).unwrap());
        }
        let mut seed = 1u32;
        for _ in 0..4096 {
            let mut cosines = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                seed as i16
            });
            cosines.sort_unstable_by(|left, right| right.cmp(left));
            include(lsp_to_lsf(&cosines).unwrap());
        }
        assert_eq!(hash, 0xdf75f66f9c94d893);
    }
    #[test]
    fn forward_spectral_conversion_matches_full_reference_frequency_domain() {
        let mut hash = 0xcbf29ce484222325u64;
        for frequency in 0..MAX_FREQUENCY_Q15 {
            for value in lsf_to_lsp(&[frequency; LPC_ORDER]).unwrap() {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        assert_eq!(hash, 0x1cf25f7899a77065);
        assert_eq!(
            lsf_to_lsp(&[MAX_FREQUENCY_Q15; LPC_ORDER]),
            Ok([i16::MIN; LPC_ORDER])
        );
        assert_eq!(
            lsp_to_lsf(&[i16::MIN; LPC_ORDER]),
            Ok([MAX_FREQUENCY_Q15; LPC_ORDER])
        );
    }
    #[test]
    fn spectral_conversion_rejects_out_of_domain_and_unordered_inputs() {
        for value in [-1, MAX_FREQUENCY_Q15 + 1] {
            let mut frequencies = [0; LPC_ORDER];
            frequencies[3] = value;
            assert_eq!(
                lsf_to_lsp(&frequencies),
                Err(ConversionError::Frequency { index: 3, value })
            );
        }
        let mut cosines = [0; LPC_ORDER];
        cosines[1] = 1;
        assert_eq!(
            lsp_to_lsf(&cosines),
            Err(ConversionError::UnorderedCosines { index: 1 })
        );
    }
}
