use super::{LPC_COEFFICIENTS, SUBFRAME_SAMPLES, interpolation};
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthesisError {
    UnstablePredictor,
    NonFiniteExcitation { sample: usize },
    NonFiniteOutput { sample: usize },
}
impl fmt::Display for SynthesisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "short-term synthesis failed: {self:?}")
    }
}
impl Error for SynthesisError {}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SynthesisFilter {
    memory: [f64; LPC_COEFFICIENTS],
}
impl SynthesisFilter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        self.memory.fill(0.0);
    }
    // IS-54-B §2.2.2.2.4.2: A(z) = 1 / (1 - sum(alpha_i z^-i)).
    pub fn process(
        &mut self,
        coefficients: &[f64; LPC_COEFFICIENTS],
        excitation: &[f64; SUBFRAME_SAMPLES],
    ) -> Result<[f64; SUBFRAME_SAMPLES], SynthesisError> {
        if !interpolation::is_stable(coefficients) {
            return Err(SynthesisError::UnstablePredictor);
        }
        if let Some(sample) = excitation.iter().position(|sample| !sample.is_finite()) {
            return Err(SynthesisError::NonFiniteExcitation { sample });
        }
        let mut memory = self.memory;
        let mut output = [0.0; SUBFRAME_SAMPLES];
        for (sample, input) in excitation.iter().copied().enumerate() {
            let value = input
                + coefficients
                    .iter()
                    .zip(memory)
                    .map(|(coefficient, previous)| coefficient * previous)
                    .sum::<f64>();
            if !value.is_finite() {
                return Err(SynthesisError::NonFiniteOutput { sample });
            }
            output[sample] = value;
            memory.copy_within(..LPC_COEFFICIENTS - 1, 1);
            memory[0] = value;
        }
        self.memory = memory;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn analytic_impulse_response_preserves_history_across_subframes_and_reset() {
        let mut filter = SynthesisFilter::new();
        let mut coefficients = [0.0; LPC_COEFFICIENTS];
        coefficients[0] = 0.5;
        let mut excitation = [0.0; SUBFRAME_SAMPLES];
        excitation[0] = 1.0;
        let first = filter.process(&coefficients, &excitation).unwrap();
        let second = filter
            .process(&coefficients, &[0.0; SUBFRAME_SAMPLES])
            .unwrap();
        for (sample, value) in first.into_iter().chain(second).enumerate() {
            assert_eq!(value, 0.5f64.powi(sample as i32));
        }
        filter.reset();
        assert_eq!(
            filter
                .process(&coefficients, &[0.0; SUBFRAME_SAMPLES])
                .unwrap(),
            [0.0; SUBFRAME_SAMPLES]
        );
    }
    #[test]
    fn changing_coefficients_preserves_sample_history_and_zero_predictor_is_identity() {
        let mut filter = SynthesisFilter::new();
        let mut coefficients = [0.0; LPC_COEFFICIENTS];
        coefficients[0] = 0.5;
        let input = [1.0; SUBFRAME_SAMPLES];
        let first = filter.process(&coefficients, &input).unwrap();
        coefficients[0] = -0.25;
        let next = filter
            .process(&coefficients, &[0.0; SUBFRAME_SAMPLES])
            .unwrap();
        assert_eq!(next[0], -0.25 * first[SUBFRAME_SAMPLES - 1]);
        let identity = filter.process(&[0.0; LPC_COEFFICIENTS], &input).unwrap();
        assert_eq!(identity, input);
    }
    #[test]
    fn predictor_input_and_output_failures_leave_history_unchanged() {
        let mut filter = SynthesisFilter::new();
        let mut coefficients = [0.0; LPC_COEFFICIENTS];
        coefficients[0] = 0.5;
        filter
            .process(&coefficients, &[1.0; SUBFRAME_SAMPLES])
            .unwrap();
        let before = filter.clone();
        let mut unstable = coefficients;
        unstable[0] = 1.0;
        assert_eq!(
            filter.process(&unstable, &[0.0; SUBFRAME_SAMPLES]),
            Err(SynthesisError::UnstablePredictor)
        );
        assert_eq!(filter, before);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut input = [0.0; SUBFRAME_SAMPLES];
            input[3] = bad;
            assert_eq!(
                filter.process(&coefficients, &input),
                Err(SynthesisError::NonFiniteExcitation { sample: 3 })
            );
            assert_eq!(filter, before);
        }
        assert!(matches!(
            filter.process(&coefficients, &[f64::MAX; SUBFRAME_SAMPLES]),
            Err(SynthesisError::NonFiniteOutput { .. })
        ));
        assert_eq!(filter, before);
    }
}
