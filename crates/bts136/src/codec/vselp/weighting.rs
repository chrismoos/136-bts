use super::{
    LPC_COEFFICIENTS, SUBFRAME_SAMPLES, interpolation,
    synthesis::{SynthesisError, SynthesisFilter},
};

// IS-54-B §2.1.3.3.2.6.1 permits lambda = 0.8 for perceptual noise weighting.
const NOISE_WEIGHTING: f64 = 0.8;
/// Zero-state weighted codebook responses from IS-54-B §2.1.3.3.2.6.4.1.
#[derive(Debug, Clone, PartialEq)]
pub struct WeightedCodebookExcitation {
    pub codebook_one: [f64; SUBFRAME_SAMPLES],
    pub codebook_two: [f64; SUBFRAME_SAMPLES],
}
fn weighted_coefficients(
    predictor: &[f64; LPC_COEFFICIENTS],
) -> Result<[f64; LPC_COEFFICIENTS], SynthesisError> {
    if !interpolation::is_stable(predictor) {
        return Err(SynthesisError::UnstablePredictor);
    }
    let mut weight = 1.0;
    Ok(predictor.map(|coefficient| {
        weight *= NOISE_WEIGHTING;
        coefficient * weight
    }))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerceptualFilter {
    input_history: [f64; LPC_COEFFICIENTS],
    synthesis: SynthesisFilter,
}
impl PerceptualFilter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
    // IS-54-B §2.1.3.3.2.6.1 preserves separate all-zero and all-pole direct-form states.
    pub fn process(
        &mut self,
        predictor: &[f64; LPC_COEFFICIENTS],
        speech: &[f64; SUBFRAME_SAMPLES],
    ) -> Result<[f64; SUBFRAME_SAMPLES], SynthesisError> {
        let weighted = weighted_coefficients(predictor)?;
        if let Some(sample) = speech.iter().position(|sample| !sample.is_finite()) {
            return Err(SynthesisError::NonFiniteExcitation { sample });
        }
        let mut input_history = self.input_history;
        let mut inverse = [0.0; SUBFRAME_SAMPLES];
        for (sample, input) in speech.iter().copied().enumerate() {
            let value = input
                - predictor
                    .iter()
                    .zip(input_history)
                    .map(|(a, x)| a * x)
                    .sum::<f64>();
            if !value.is_finite() {
                return Err(SynthesisError::NonFiniteOutput { sample });
            }
            inverse[sample] = value;
            input_history.copy_within(..LPC_COEFFICIENTS - 1, 1);
            input_history[0] = input;
        }
        let output = self.synthesis.process(&weighted, &inverse)?;
        self.input_history = input_history;
        Ok(output)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WeightedSynthesisFilter {
    synthesis: SynthesisFilter,
}
impl WeightedSynthesisFilter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        self.synthesis.reset();
    }
    // IS-54-B §2.1.3.3.2.6.2: H(z) = 1 / (1 - sum(alpha_i lambda^i z^-i)).
    pub fn process(
        &mut self,
        predictor: &[f64; LPC_COEFFICIENTS],
        excitation: &[f64; SUBFRAME_SAMPLES],
    ) -> Result<[f64; SUBFRAME_SAMPLES], SynthesisError> {
        self.synthesis
            .process(&weighted_coefficients(predictor)?, excitation)
    }
    /// IS-54-B §2.1.3.3.2.6.2 subtracts this response without advancing the synthesis state.
    pub fn zero_input_response(
        &self,
        predictor: &[f64; LPC_COEFFICIENTS],
    ) -> Result<[f64; SUBFRAME_SAMPLES], SynthesisError> {
        self.clone().process(predictor, &[0.0; SUBFRAME_SAMPLES])
    }
    pub fn zero_state_response(
        predictor: &[f64; LPC_COEFFICIENTS],
        excitation: &[f64; SUBFRAME_SAMPLES],
    ) -> Result<[f64; SUBFRAME_SAMPLES], SynthesisError> {
        Self::new().process(predictor, excitation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn predictor(first: f64, second: f64) -> [f64; LPC_COEFFICIENTS] {
        let mut result = [0.0; LPC_COEFFICIENTS];
        result[0] = first;
        result[1] = second;
        result
    }
    #[test]
    fn first_order_perceptual_impulse_matches_analytic_transfer_function_across_subframes() {
        let mut filter = PerceptualFilter::new();
        let coefficients = predictor(0.5, 0.0);
        let mut impulse = [0.0; SUBFRAME_SAMPLES];
        impulse[0] = 1.0;
        let output: Vec<_> = filter
            .process(&coefficients, &impulse)
            .unwrap()
            .into_iter()
            .chain(
                filter
                    .process(&coefficients, &[0.0; SUBFRAME_SAMPLES])
                    .unwrap(),
            )
            .collect();
        assert_eq!(output[0], 1.0);
        for (sample, value) in output.into_iter().enumerate().skip(1) {
            let expected = -0.1 * 0.4f64.powi((sample - 1) as i32);
            assert!((value - expected).abs() < 1e-15);
        }
    }
    #[test]
    fn coefficient_changes_preserve_both_cascade_histories_against_independent_scipy_values() {
        let input: [f64; SUBFRAME_SAMPLES * 2] =
            std::array::from_fn(|sample| ((sample * 37 % 101) as f64 - 50.0) / 64.0);
        let frames = input.as_chunks::<SUBFRAME_SAMPLES>().0;
        let mut filter = PerceptualFilter::new();
        let output: Vec<_> = filter
            .process(&predictor(0.6, -0.08), &frames[0])
            .unwrap()
            .into_iter()
            .chain(filter.process(&predictor(-0.4, -0.03), &frames[1]).unwrap())
            .collect();
        for (sample, expected) in [
            (0, -0.78125),
            (1, -0.109375),
            (2, 0.421875),
            (3, -0.65815),
            (9, -0.39557908659630087),
            (20, -0.35618868421268174),
            (38, 0.6711039888300268),
            (39, -0.40872014432352255),
            (40, 0.25681149959799077),
            (41, -0.7341760031003455),
            (42, -0.23436945980017088),
            (43, 0.40409440639558136),
            (49, 0.7381656162524842),
            (60, 0.7882189455819582),
            (78, 0.08107489147176158),
            (79, 0.7214813410424933),
        ] {
            assert!((output[sample] - expected).abs() < 1e-14, "sample {sample}");
        }
    }
    #[test]
    fn weighted_synthesis_impulse_and_read_only_zero_input_response_obey_superposition() {
        let coefficients = predictor(0.5, 0.0);
        let mut impulse = [0.0; SUBFRAME_SAMPLES];
        impulse[0] = 1.0;
        let response =
            WeightedSynthesisFilter::zero_state_response(&coefficients, &impulse).unwrap();
        for (sample, value) in response.into_iter().enumerate() {
            assert!((value - 0.4f64.powi(sample as i32)).abs() < 1e-15);
        }
        let mut filter = WeightedSynthesisFilter::new();
        filter
            .process(&coefficients, &[0.25; SUBFRAME_SAMPLES])
            .unwrap();
        let before = filter.clone();
        let changed = predictor(-0.4, -0.03);
        let zero_input = filter.zero_input_response(&changed).unwrap();
        assert_eq!(filter, before);
        let input = std::array::from_fn(|sample| sample as f64 / 64.0);
        let zero_state = WeightedSynthesisFilter::zero_state_response(&changed, &input).unwrap();
        let stateful = filter.process(&changed, &input).unwrap();
        for sample in 0..SUBFRAME_SAMPLES {
            assert!((stateful[sample] - zero_input[sample] - zero_state[sample]).abs() < 1e-14);
        }
    }
    #[test]
    fn reset_restores_identity_and_invalid_processing_preserves_histories() {
        let coefficients = predictor(0.5, 0.0);
        let mut perceptual = PerceptualFilter::new();
        let mut weighted = WeightedSynthesisFilter::new();
        let input = [0.25; SUBFRAME_SAMPLES];
        perceptual.process(&coefficients, &input).unwrap();
        weighted.process(&coefficients, &input).unwrap();
        let before_perceptual = perceptual.clone();
        let before_weighted = weighted.clone();
        let unstable = predictor(1.1, 0.0);
        assert_eq!(
            perceptual.process(&unstable, &input),
            Err(SynthesisError::UnstablePredictor)
        );
        assert_eq!(
            weighted.process(&unstable, &input),
            Err(SynthesisError::UnstablePredictor)
        );
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut input = input;
            input[3] = invalid;
            assert!(perceptual.process(&coefficients, &input).is_err());
            assert!(weighted.process(&coefficients, &input).is_err());
        }
        let alternating =
            std::array::from_fn(|sample| if sample % 2 == 0 { f64::MAX } else { -f64::MAX });
        assert!(perceptual.process(&coefficients, &alternating).is_err());
        assert!(
            weighted
                .process(&coefficients, &[f64::MAX; SUBFRAME_SAMPLES])
                .is_err()
        );
        assert_eq!(perceptual, before_perceptual);
        assert_eq!(weighted, before_weighted);
        perceptual.reset();
        weighted.reset();
        assert_eq!(
            perceptual
                .process(&[0.0; LPC_COEFFICIENTS], &input)
                .unwrap(),
            input
        );
        assert_eq!(
            weighted.process(&[0.0; LPC_COEFFICIENTS], &input).unwrap(),
            input
        );
    }
}
