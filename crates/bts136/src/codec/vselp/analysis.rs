use super::{LPC_COEFFICIENTS, quantization};
use std::{error::Error, fmt};

pub const ANALYSIS_SAMPLES: usize = 170;
const COVARIANCE_ORDER: usize = LPC_COEFFICIENTS + 1;
type Matrix = [[f64; LPC_COEFFICIENTS]; LPC_COEFFICIENTS];
// IS-54-B §2.1.3.3.2.4.2, 80 Hz binomial bandwidth-expansion window.
const COVARIANCE_WINDOW: [f64; COVARIANCE_ORDER] = [
    1.0, 0.999644, 0.998577, 0.996802, 0.994321, 0.991141, 0.987268, 0.982710, 0.977478, 0.971581,
    0.965032,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisError {
    NonFiniteSample { sample: usize },
    Recursion { coefficient: usize },
}
impl fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP predictor analysis failed: {self:?}")
    }
}
impl Error for AnalysisError {}

/// IS-54-B §2.1.3.3.2.5.1 uses samples normalized to maximum PCM amplitude, centered on subframe four.
pub fn analyze_energy(samples: &[f64; ANALYSIS_SAMPLES]) -> Result<u8, AnalysisError> {
    if let Some(sample) = samples.iter().position(|sample| !sample.is_finite()) {
        return Err(AnalysisError::NonFiniteSample { sample });
    }
    let scale = samples
        .iter()
        .fold(0.0f64, |maximum, value| maximum.max(value.abs()));
    if scale == 0.0 {
        return Ok(quantization::SILENCE_ENERGY_CODE);
    }
    // The two covariance diagonals overlap, giving half weight to each ten-sample edge.
    let normalized_power = samples[..ANALYSIS_SAMPLES - LPC_COEFFICIENTS]
        .iter()
        .chain(&samples[LPC_COEFFICIENTS..])
        .map(|sample| (sample / scale).powi(2))
        .sum::<f64>()
        / (2 * (ANALYSIS_SAMPLES - LPC_COEFFICIENTS)) as f64;
    const AMPLITUDE_DB_MULTIPLIER: f64 = 20.0;
    const POWER_DB_MULTIPLIER: f64 = 10.0;
    let energy_db =
        AMPLITUDE_DB_MULTIPLIER * scale.log10() + POWER_DB_MULTIPLIER * normalized_power.log10();
    Ok(quantization::encode_energy_db(energy_db))
}

/// IS-54-B §2.1.3.3.2.4 requires the 170-sample interval centered on the fourth subframe.
pub fn analyze_predictor(
    samples: &[f64; ANALYSIS_SAMPLES],
) -> Result<[u8; LPC_COEFFICIENTS], AnalysisError> {
    if let Some(sample) = samples.iter().position(|sample| !sample.is_finite()) {
        return Err(AnalysisError::NonFiniteSample { sample });
    }
    // FLAT is scale-invariant, so normalization prevents overflow and underflow in the covariance sum.
    let scale = samples
        .iter()
        .fold(0.0f64, |maximum, value| maximum.max(value.abs()));
    let normalized = if scale == 0.0 {
        [0.0; ANALYSIS_SAMPLES]
    } else {
        samples.map(|sample| sample / scale)
    };
    let covariance = covariance(&normalized);
    let mut forward: Matrix = std::array::from_fn(|i| std::array::from_fn(|k| covariance[i][k]));
    let mut backward: Matrix =
        std::array::from_fn(|i| std::array::from_fn(|k| covariance[i + 1][k + 1]));
    let mut cross: Matrix = std::array::from_fn(|i| std::array::from_fn(|k| covariance[i][k + 1]));
    let mut codes = [0; LPC_COEFFICIENTS];
    // IS-54-B §2.1.3.3.2.4.1 steps 4–7 quantize each reflection before advancing the lattice.
    for (coefficient, code) in codes.iter_mut().enumerate() {
        let last = LPC_COEFFICIENTS - coefficient - 1;
        let denominator =
            forward[0][0] + backward[0][0] + forward[last][last] + backward[last][last];
        // The rounded published covariance window is not positive semidefinite for every input.
        if !denominator.is_finite() {
            return Err(AnalysisError::Recursion { coefficient });
        }
        let reflection = if denominator == 0.0 {
            0.0
        } else {
            -2.0 * (cross[0][0] + cross[last][last]) / denominator
        };
        if !reflection.is_finite() {
            return Err(AnalysisError::Recursion { coefficient });
        }
        *code = quantization::nearest_reflection(coefficient, reflection);
        let quantized = quantization::reflection(coefficient, *code);
        let squared = quantized * quantized;
        let old_forward = forward;
        let old_backward = backward;
        let old_cross = cross;
        for i in 0..last {
            for k in 0..last {
                forward[i][k] = old_forward[i][k]
                    + quantized * (old_cross[i][k] + old_cross[k][i])
                    + squared * old_backward[i][k];
                backward[i][k] = old_backward[i + 1][k + 1]
                    + quantized * (old_cross[i + 1][k + 1] + old_cross[k + 1][i + 1])
                    + squared * old_forward[i + 1][k + 1];
                cross[i][k] = old_cross[i][k + 1]
                    + quantized * (old_backward[i][k + 1] + old_forward[i][k + 1])
                    + squared * old_cross[k + 1][i];
            }
        }
    }
    Ok(codes)
}
// IS-54-B §2.1.3.3.2.4.1-8 sums samples Np through NA-1 for every covariance pair.
fn covariance(samples: &[f64; ANALYSIS_SAMPLES]) -> [[f64; COVARIANCE_ORDER]; COVARIANCE_ORDER] {
    std::array::from_fn(|i| {
        std::array::from_fn(|k| {
            (LPC_COEFFICIENTS..ANALYSIS_SAMPLES)
                .map(|n| samples[n - i] * samples[n - k])
                .sum::<f64>()
                * COVARIANCE_WINDOW[i.abs_diff(k)]
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn covariance_bounds_and_bandwidth_window_match_closed_form_ramp_sums() {
        let samples = std::array::from_fn(|sample| sample as f64);
        let result = covariance(&samples);
        const SUM_N: f64 = 14320.0;
        const SUM_N_SQUARED: f64 = 1622960.0;
        const COUNT: f64 = 160.0;
        for (i, row) in result.iter().enumerate() {
            for (k, value) in row.iter().enumerate() {
                let expected = (SUM_N_SQUARED - (i + k) as f64 * SUM_N + (i * k) as f64 * COUNT)
                    * COVARIANCE_WINDOW[i.abs_diff(k)];
                assert_eq!(*value, expected);
                assert_eq!(*value, result[k][i]);
            }
        }
    }
    #[test]
    fn silence_scale_invariance_and_nonfinite_inputs_have_defined_results() {
        let silent = analyze_predictor(&[0.0; ANALYSIS_SAMPLES]).unwrap();
        assert!(
            quantization::reflections(&silent)
                .iter()
                .all(|value| value.abs() < 1.0)
        );
        let samples = std::array::from_fn(|sample| ((sample * 37 % 101) as f64 - 50.0) / 64.0);
        let expected = analyze_predictor(&samples).unwrap();
        for scale in [1e-200, 1e200, -1.0] {
            assert_eq!(
                analyze_predictor(&samples.map(|x| x * scale)).unwrap(),
                expected
            );
        }
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut input = samples;
            input[7] = invalid;
            assert_eq!(
                analyze_predictor(&input),
                Err(AnalysisError::NonFiniteSample { sample: 7 })
            );
        }
    }
}
#[test]
fn energy_analysis_uses_both_covariance_diagonals_and_does_not_normalize_away_amplitude() {
    let mut edge = [0.0; ANALYSIS_SAMPLES];
    edge[0] = 1.0;
    let mut middle = [0.0; ANALYSIS_SAMPLES];
    middle[10] = 1.0;
    assert_eq!(analyze_energy(&edge).unwrap(), 20);
    assert_eq!(analyze_energy(&middle).unwrap(), 22);
    edge[0] = 0.5;
    assert_eq!(analyze_energy(&edge).unwrap(), 17);
    let mut last = [0.0; ANALYSIS_SAMPLES];
    last[169] = 1.0;
    assert_eq!(analyze_energy(&last).unwrap(), 20);
}
#[test]
fn energy_analysis_handles_silence_tiny_huge_and_nonfinite_samples() {
    assert_eq!(analyze_energy(&[0.0; ANALYSIS_SAMPLES]).unwrap(), 0);
    assert_eq!(analyze_energy(&[1e-200; ANALYSIS_SAMPLES]).unwrap(), 0);
    assert_eq!(analyze_energy(&[f64::MAX; ANALYSIS_SAMPLES]).unwrap(), 31);
    assert_eq!(analyze_energy(&[0.001; ANALYSIS_SAMPLES]).unwrap(), 3);
    assert_eq!(analyze_energy(&[0.0001; ANALYSIS_SAMPLES]).unwrap(), 0);
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut input = [0.0; ANALYSIS_SAMPLES];
        input[3] = invalid;
        assert_eq!(
            analyze_energy(&input),
            Err(AnalysisError::NonFiniteSample { sample: 3 })
        );
    }
}
