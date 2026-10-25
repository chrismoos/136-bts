use super::{
    LAG_CODE_OFFSET, LPC_COEFFICIENTS, MAX_LAG_SAMPLES, SUBFRAME_SAMPLES, VselpSubframe,
    excitation::LongTermHistory, synthesis::SynthesisError, weighting::WeightedSynthesisFilter,
};
use std::{error::Error, fmt};

const DISABLED_LAG_CODE: u8 = 0;
const MIN_ACTIVE_LAG_CODE: u8 = 1;
const MAX_ACTIVE_LAG_CODE: u8 = (MAX_LAG_SAMPLES - LAG_CODE_OFFSET) as u8;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PitchError {
    Filter(SynthesisError),
    NonFiniteTarget { sample: usize },
    Arithmetic,
}
impl fmt::Display for PitchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP pitch search failed: {self:?}")
    }
}
impl Error for PitchError {}
#[derive(Debug, Clone, PartialEq)]
pub struct PitchSelection {
    pub lag_code: u8,
    pub adaptive: [f64; SUBFRAME_SAMPLES],
    pub weighted: [f64; SUBFRAME_SAMPLES],
    pub optimal_gain: f64,
}

/// IS-54-B §2.1.3.3.2.6.3 searches the weighted speech target after subtracting zero-input response.
pub fn search(
    history: &LongTermHistory,
    predictor: &[f64; LPC_COEFFICIENTS],
    target: &[f64; SUBFRAME_SAMPLES],
) -> Result<PitchSelection, PitchError> {
    if let Some(sample) = target.iter().position(|sample| !sample.is_finite()) {
        return Err(PitchError::NonFiniteTarget { sample });
    }
    let mut best = PitchSelection {
        lag_code: DISABLED_LAG_CODE,
        adaptive: [0.0; SUBFRAME_SAMPLES],
        weighted: [0.0; SUBFRAME_SAMPLES],
        optimal_gain: 0.0,
    };
    let mut best_metric = 0.0;
    // IS-54-B §2.1.3.3.2.6.3.1 uses C_L²/G_L only for positive correlations.
    for lag_code in MIN_ACTIVE_LAG_CODE..=MAX_ACTIVE_LAG_CODE {
        let subframe = VselpSubframe {
            lag: lag_code,
            ..VselpSubframe::default()
        };
        let adaptive = history.excitation(&subframe);
        let weighted = WeightedSynthesisFilter::zero_state_response(predictor, &adaptive)
            .map_err(PitchError::Filter)?;
        let energy = weighted.iter().map(|sample| sample * sample).sum::<f64>();
        let correlation = weighted
            .iter()
            .zip(target)
            .map(|(sample, target)| sample * target)
            .sum::<f64>();
        if !energy.is_finite() || !correlation.is_finite() {
            return Err(PitchError::Arithmetic);
        }
        if energy == 0.0 || correlation <= 0.0 {
            continue;
        }
        // Positive C_L/sqrt(G_L) has the same ordering without squaring small correlations.
        let metric = correlation / energy.sqrt();
        let optimal_gain = correlation / energy;
        if !metric.is_finite() || !optimal_gain.is_finite() {
            return Err(PitchError::Arithmetic);
        }
        if metric > best_metric {
            best_metric = metric;
            best = PitchSelection {
                lag_code,
                adaptive,
                weighted,
                optimal_gain,
            };
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn constant_history() -> LongTermHistory {
        let mut history = LongTermHistory::new();
        for _ in 0..4 {
            history.commit(&[1.0; SUBFRAME_SAMPLES]).unwrap();
        }
        history
    }
    #[test]
    fn absent_or_nonpositively_correlated_history_disables_prediction() {
        let predictor = [0.0; LPC_COEFFICIENTS];
        for (history, target) in [
            (LongTermHistory::new(), [1.0; SUBFRAME_SAMPLES]),
            (constant_history(), [-1.0; SUBFRAME_SAMPLES]),
            (constant_history(), [0.0; SUBFRAME_SAMPLES]),
        ] {
            let result = search(&history, &predictor, &target).unwrap();
            assert_eq!(result.lag_code, DISABLED_LAG_CODE);
            assert_eq!(result.adaptive, [0.0; SUBFRAME_SAMPLES]);
            assert_eq!(result.weighted, [0.0; SUBFRAME_SAMPLES]);
            assert_eq!(result.optimal_gain, 0.0);
        }
    }
    #[test]
    fn positive_ties_select_the_lowest_lag_and_tiny_targets_do_not_lose_pitch_to_squared_underflow()
    {
        let history = constant_history();
        let before = history.clone();
        let predictor = [0.0; LPC_COEFFICIENTS];
        for amplitude in [0.5, 1e-200] {
            let result = search(&history, &predictor, &[amplitude; SUBFRAME_SAMPLES]).unwrap();
            assert_eq!(result.lag_code, MIN_ACTIVE_LAG_CODE);
            assert!((result.optimal_gain / amplitude - 1.0).abs() < 1e-14);
            assert_eq!(result.adaptive, [1.0; SUBFRAME_SAMPLES]);
            assert_eq!(result.weighted, result.adaptive);
        }
        assert_eq!(history, before);
    }
    #[test]
    fn invalid_target_predictor_and_overflow_return_errors_without_mutating_history() {
        let history = constant_history();
        let before = history.clone();
        let mut predictor = [0.0; LPC_COEFFICIENTS];
        predictor[0] = 1.1;
        assert_eq!(
            search(&history, &predictor, &[0.0; SUBFRAME_SAMPLES]),
            Err(PitchError::Filter(SynthesisError::UnstablePredictor))
        );
        let predictor = [0.0; LPC_COEFFICIENTS];
        let mut invalid = [0.0; SUBFRAME_SAMPLES];
        invalid[3] = f64::NAN;
        assert_eq!(
            search(&history, &predictor, &invalid),
            Err(PitchError::NonFiniteTarget { sample: 3 })
        );
        assert_eq!(
            search(&history, &predictor, &[f64::MAX; SUBFRAME_SAMPLES]),
            Err(PitchError::Arithmetic)
        );
        assert_eq!(history, before);
    }
}
