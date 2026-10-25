use super::FRAME_SAMPLES;
use std::{error::Error, fmt};

const FILTER_ORDER: usize = 4;
// IS-54-B §2.1.3.3.2.3, denominator is one minus the feedback polynomial.
const FEEDFORWARD: [f64; FILTER_ORDER + 1] = [
    0.898025036,
    -3.59010601,
    5.38416243,
    -3.59010601,
    0.898024917,
];
const FEEDBACK: [f64; FILTER_ORDER] = [3.78284979, -5.37379122, 3.39733505, -0.806448996];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreprocessingError {
    NonFiniteInput { sample: usize },
    NonFiniteOutput { sample: usize },
}
impl fmt::Display for PreprocessingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP high-pass preprocessing failed: {self:?}")
    }
}
impl Error for PreprocessingError {}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HighPassFilter {
    input_history: [f64; FILTER_ORDER],
    output_history: [f64; FILTER_ORDER],
}
impl HighPassFilter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
    /// IS-54-B §2.1.3.3.2.3 filter coefficients require PCM sampled at 8 kHz.
    pub fn process(
        &mut self,
        input: &[f64; FRAME_SAMPLES],
    ) -> Result<[f64; FRAME_SAMPLES], PreprocessingError> {
        if let Some(sample) = input.iter().position(|sample| !sample.is_finite()) {
            return Err(PreprocessingError::NonFiniteInput { sample });
        }
        let mut next = self.clone();
        let mut output = [0.0; FRAME_SAMPLES];
        for (sample, value) in input.iter().copied().enumerate() {
            let filtered = FEEDFORWARD[0] * value
                + FEEDFORWARD[1..]
                    .iter()
                    .zip(next.input_history)
                    .map(|(a, x)| a * x)
                    .sum::<f64>()
                + FEEDBACK
                    .iter()
                    .zip(next.output_history)
                    .map(|(b, y)| b * y)
                    .sum::<f64>();
            if !filtered.is_finite() {
                return Err(PreprocessingError::NonFiniteOutput { sample });
            }
            output[sample] = filtered;
            next.input_history.copy_within(..FILTER_ORDER - 1, 1);
            next.output_history.copy_within(..FILTER_ORDER - 1, 1);
            next.input_history[0] = value;
            next.output_history[0] = filtered;
        }
        *self = next;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vselp::PCM_SAMPLE_RATE_HZ;
    #[test]
    fn impulse_and_frame_boundary_transients_match_independent_scipy_filtering() {
        let mut filter = HighPassFilter::new();
        let mut first = [0.0; FRAME_SAMPLES];
        first[0] = 1.0;
        first[158] = 0.5;
        let mut second = [0.0; FRAME_SAMPLES];
        second[3] = -0.25;
        let output: Vec<_> = filter
            .process(&first)
            .unwrap()
            .into_iter()
            .chain(filter.process(&second).unwrap())
            .collect();
        for (sample, expected) in [
            (0, 0.898025036),
            (1, -0.19301219115265714),
            (2, -0.17177275056625338),
            (3, -0.15179737465785126),
            (4, -0.13306932176410738),
            (5, -0.11556895509820396),
            (6, -0.09927382376523153),
            (7, -0.08415875769217025),
            (8, -0.07019597724899782),
            (9, -0.057355217840352586),
            (40, 0.005443767849717387),
            (80, 0.001797872444494303),
            (120, -0.0009885936192387454),
            (158, 0.44939018082526844),
            (159, -0.0961083092539237),
            (160, -0.08547303900837491),
            (161, -0.0754742258623004),
            (162, -0.06610332214708359),
            (163, -0.28185656677535575),
            (164, -0.00095068616968306),
            (200, -0.002053773666666785),
            (240, 0.0013044295871895806),
            (280, -0.00043707916145742907),
            (319, 0.0001339619817373769),
        ] {
            assert!((output[sample] - expected).abs() < 1e-10, "sample {sample}");
        }
    }
    #[test]
    fn streamed_sinusoid_response_matches_stopband_corner_and_passband() {
        const SETTLE_FRAMES: usize = 100;
        const MEASURE_FRAMES: usize = 100;
        for (frequency_hz, expected_db) in [
            (60.0, -43.654765963182804),
            (120.0, -3.162684491262469),
            (1000.0, -4.259228612912678e-6),
        ] {
            let mut filter = HighPassFilter::new();
            let mut input_energy = 0.0;
            let mut output_energy = 0.0;
            for frame in 0..SETTLE_FRAMES + MEASURE_FRAMES {
                let input = std::array::from_fn(|sample| {
                    let time_seconds =
                        (frame * FRAME_SAMPLES + sample) as f64 / PCM_SAMPLE_RATE_HZ as f64;
                    (std::f64::consts::TAU * frequency_hz * time_seconds).sin()
                });
                let output = filter.process(&input).unwrap();
                if frame >= SETTLE_FRAMES {
                    input_energy += input.iter().map(|x| x * x).sum::<f64>();
                    output_energy += output.iter().map(|x| x * x).sum::<f64>();
                }
            }
            let measured_db = 10.0 * (output_energy / input_energy).log10();
            assert!(
                (measured_db - expected_db).abs() < 1e-6,
                "{frequency_hz} Hz: {measured_db} dB"
            );
        }
    }
    #[test]
    fn reset_and_fresh_silence_clear_both_filter_histories() {
        let mut filter = HighPassFilter::new();
        let silence = [0.0; FRAME_SAMPLES];
        assert_eq!(filter.process(&silence).unwrap(), silence);
        let mut impulse = silence;
        impulse[0] = 1.0;
        let first = filter.process(&impulse).unwrap();
        assert_ne!(filter.process(&silence).unwrap(), silence);
        filter.reset();
        assert_eq!(filter.process(&silence).unwrap(), silence);
        assert_eq!(filter.process(&impulse).unwrap(), first);
    }
    #[test]
    fn nonfinite_input_and_output_overflow_preserve_filter_state() {
        let mut filter = HighPassFilter::new();
        filter.process(&[0.25; FRAME_SAMPLES]).unwrap();
        let before = filter.clone();
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut input = [0.0; FRAME_SAMPLES];
            input[7] = invalid;
            assert_eq!(
                filter.process(&input),
                Err(PreprocessingError::NonFiniteInput { sample: 7 })
            );
            assert_eq!(filter, before);
        }
        assert!(matches!(
            filter.process(&[f64::MAX; FRAME_SAMPLES]),
            Err(PreprocessingError::NonFiniteOutput { .. })
        ));
        assert_eq!(filter, before);
    }
}
