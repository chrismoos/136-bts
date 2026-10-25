use super::{
    SUBFRAME_SAMPLES, lpc::COEFFICIENTS, postfilter::weight, spectral::LPC_ORDER,
    synthesis::SynthesisFilter,
};
const RESIDUAL_SHIFT: u32 = 3;
const WORD_SHIFT: u32 = 16;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
const GAMMA_ONE_Q15: [i16; LPC_ORDER] = [
    30802, 28954, 27217, 25584, 24049, 22606, 21250, 19975, 18777, 17650,
];
const GAMMA_TWO_Q15: [i16; LPC_ORDER] = [19661, 11797, 7078, 4247, 2548, 1529, 917, 550, 330, 198];
pub const INPUT_SAMPLES: usize = LPC_ORDER + SUBFRAME_SAMPLES;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSubframe {
    pub residual_q0: [i16; SUBFRAME_SAMPLES],
    pub impulse_q12: [i16; SUBFRAME_SAMPLES],
    pub target_q0: [i16; SUBFRAME_SAMPLES],
    pub error_q0: [i16; SUBFRAME_SAMPLES],
}
/// TS 26.073 subframePreProc for MR74. Error and weighting histories remain unchanged until quantized excitation is committed.
pub fn prepare(
    speech_q0: &[i16; INPUT_SAMPLES],
    coefficients_q12: &[i16; COEFFICIENTS],
    quantized_q12: &[i16; COEFFICIENTS],
    error_history_q0: &[i16; LPC_ORDER],
    weighting_history_q0: &[i16; LPC_ORDER],
) -> PreparedSubframe {
    let numerator = weight(coefficients_q12, &GAMMA_ONE_Q15);
    let denominator = weight(coefficients_q12, &GAMMA_TWO_Q15);
    let mut impulse_input = [0; SUBFRAME_SAMPLES];
    impulse_input[..COEFFICIENTS].copy_from_slice(&numerator);
    let unweighted_impulse = SynthesisFilter::default()
        .preview(quantized_q12, &impulse_input)
        .samples_q0;
    let impulse_q12 = SynthesisFilter::default()
        .preview(&denominator, &unweighted_impulse)
        .samples_q0;
    let residual_q0 = residual(quantized_q12, speech_q0);
    let error_q0 = SynthesisFilter::with_history(*error_history_q0)
        .preview(quantized_q12, &residual_q0)
        .samples_q0;
    let mut error = [0; INPUT_SAMPLES];
    error[..LPC_ORDER].copy_from_slice(error_history_q0);
    error[LPC_ORDER..].copy_from_slice(&error_q0);
    let weighted = residual(&numerator, &error);
    let target_q0 = SynthesisFilter::with_history(*weighting_history_q0)
        .preview(&denominator, &weighted)
        .samples_q0;
    PreparedSubframe {
        residual_q0,
        impulse_q12,
        target_q0,
        error_q0,
    }
}
/// TS 26.073 Residu. The input begins with the ten preceding speech samples, oldest first.
pub fn residual(
    coefficients_q12: &[i16; COEFFICIENTS],
    samples: &[i16; INPUT_SAMPLES],
) -> [i16; SUBFRAME_SAMPLES] {
    std::array::from_fn(|i| {
        let mut sum = 0i32;
        for (tap, &coefficient) in coefficients_q12.iter().enumerate() {
            let value = samples[LPC_ORDER + i - tap];
            sum = sum.saturating_add((i32::from(value) * i32::from(coefficient)).saturating_mul(2));
        }
        let scaled = (i64::from(sum) << RESIDUAL_SHIFT)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        (scaled.saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16
    })
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WeightedSpeechFilter {
    synthesis: SynthesisFilter,
}
impl WeightedSpeechFilter {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 pre_big applies A(z/0.94)/A(z/0.6), preserving synthesis history across subframes.
    pub fn filter(
        &mut self,
        speech_q0: &[i16; INPUT_SAMPLES],
        coefficients_q12: &[i16; COEFFICIENTS],
    ) -> [i16; SUBFRAME_SAMPLES] {
        let numerator = weight(coefficients_q12, &GAMMA_ONE_Q15);
        let denominator = weight(coefficients_q12, &GAMMA_TWO_Q15);
        self.synthesis
            .filter(&denominator, &residual(&numerator, speech_q0))
            .samples_q0
    }
}
#[cfg(test)]
mod tests;
