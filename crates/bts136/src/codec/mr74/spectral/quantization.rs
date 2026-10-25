use super::{
    ConversionError, FIRST_SPLIT_SAMPLES, LPC_ORDER, MAX_FREQUENCY_Q15, SpectralDecoder, Spectrum,
    THIRD_SPLIT_START, lsp_to_lsf, multiply_q15, tables,
};
use crate::codec::mr74::{LSF_INDICES, PARAMETER_COUNT, Parameters};

const WEIGHT_BREAKPOINT: i16 = 1843;
const WEIGHT_INTERCEPT: i16 = 3427;
const LOW_WEIGHT_SLOPE_Q15: i16 = 28160;
const HIGH_WEIGHT_SLOPE_Q15: i16 = 6242;
const WEIGHT_SHIFT: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantizedSpectrum {
    pub indices: [u16; LSF_INDICES],
    pub spectrum: Spectrum,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quantizer {
    decoder: SpectralDecoder,
}
impl Quantizer {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 Q_plsf_3 for MR74. Predictor state advances only after valid ordered Q15 LSP input.
    pub fn quantize(
        &mut self,
        lsp_q15: &[i16; LPC_ORDER],
    ) -> Result<QuantizedSpectrum, ConversionError> {
        let frequencies = lsp_to_lsf(lsp_q15)?;
        let weights = weights(&frequencies);
        let residual: [i16; LPC_ORDER] = std::array::from_fn(|i| {
            let prediction = tables::MEAN[i].saturating_add(multiply_q15(
                self.decoder.residual_q15[i],
                tables::PREDICTOR[i],
            ));
            frequencies[i].saturating_sub(prediction)
        });
        let indices = [
            search(
                &residual[..FIRST_SPLIT_SAMPLES],
                &weights[..FIRST_SPLIT_SAMPLES],
                &tables::FIRST,
            ),
            search(
                &residual[FIRST_SPLIT_SAMPLES..THIRD_SPLIT_START],
                &weights[FIRST_SPLIT_SAMPLES..THIRD_SPLIT_START],
                &tables::SECOND,
            ),
            search(
                &residual[THIRD_SPLIT_START..],
                &weights[THIRD_SPLIT_START..],
                &tables::THIRD,
            ),
        ];
        let mut values = [0; PARAMETER_COUNT];
        values[..LSF_INDICES].copy_from_slice(&indices);
        let parameters =
            Parameters::try_new(values).expect("codebook indices fit their parameter widths");
        Ok(QuantizedSpectrum {
            indices,
            spectrum: self.decoder.decode(&parameters),
        })
    }
}
// TS 26.073 Lsf_wt uses saturated Q15 products and shifts its piecewise-linear weights to Q13.
fn weights(frequencies: &[i16; LPC_ORDER]) -> [i16; LPC_ORDER] {
    std::array::from_fn(|i| {
        let distance = match i {
            0 => frequencies[1],
            i if i == LPC_ORDER - 1 => MAX_FREQUENCY_Q15.saturating_sub(frequencies[i - 1]),
            i => frequencies[i + 1].saturating_sub(frequencies[i - 1]),
        };
        let above = distance.saturating_sub(WEIGHT_BREAKPOINT);
        let weight = if above < 0 {
            WEIGHT_INTERCEPT.saturating_sub(multiply_q15(distance, LOW_WEIGHT_SLOPE_Q15))
        } else {
            WEIGHT_BREAKPOINT.saturating_sub(multiply_q15(above, HIGH_WEIGHT_SLOPE_Q15))
        };
        (i32::from(weight) << WEIGHT_SHIFT).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    })
}
fn search<const N: usize>(residual: &[i16], weights: &[i16], table: &[[i16; N]]) -> u16 {
    let mut best = 0;
    let mut minimum = i32::MAX;
    for (index, vector) in table.iter().enumerate() {
        let distance = residual.iter().zip(weights).zip(vector).fold(
            0i32,
            |sum, ((&target, &weight), &value)| {
                let error = multiply_q15(weight, target.saturating_sub(value));
                sum.saturating_add(
                    i32::from(error)
                        .saturating_mul(i32::from(error))
                        .saturating_mul(2),
                )
            },
        );
        if distance < minimum {
            minimum = distance;
            best = index as u16;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::spectral::{INITIAL_LSF_Q15, lsf_to_lsp};
    #[test]
    fn rejected_input_preserves_predictor_and_reset_restores_initial_state() {
        let mut quantizer = Quantizer::default();
        let initial = lsf_to_lsp(&INITIAL_LSF_Q15).unwrap();
        quantizer.quantize(&initial).unwrap();
        let saved = quantizer.clone();
        let mut unordered = initial;
        unordered.swap(0, 1);
        assert!(quantizer.quantize(&unordered).is_err());
        assert_eq!(quantizer, saved);
        quantizer.reset();
        assert_eq!(quantizer, Quantizer::default());
    }
    #[test]
    fn equal_distance_selects_first_entry() {
        assert_eq!(search(&[10], &[i16::MAX], &[[9], [9]]), 0);
    }
    #[test]
    fn consecutive_quantizations_match_fixed_point_reference_indices_cosines_and_state() {
        let mut quantizer = Quantizer::default();
        let mut hash = 0xcbf29ce484222325u64;
        for step in 0..4096 {
            let frequencies = std::array::from_fn(|i| {
                ((i + 1) * 1400 + (step * 73 + i * 197) % 601) as i16 - 300
            });
            let result = quantizer
                .quantize(&lsf_to_lsp(&frequencies).unwrap())
                .unwrap();
            for value in result
                .indices
                .map(|i| i as i16)
                .into_iter()
                .chain(result.spectrum.lsp_q15)
                .chain(quantizer.decoder.residual_q15)
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        assert_eq!(hash, 0xfe694ff8ee1c676b);
    }
}
