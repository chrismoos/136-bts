use super::{
    CodebookExcitation, SUBFRAME_SAMPLES, SubframePredictor,
    gains::{CombinedExcitation, GainError, GainParameters, PitchPrediction},
    synthesis::SynthesisError,
    weighting::{WeightedCodebookExcitation, WeightedSynthesisFilter},
};
use std::{error::Error, fmt};

const MIN_GAIN_CODE: u8 = u8::MIN;
const MAX_GAIN_CODE: u8 = u8::MAX;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    Gain(GainError),
    Filter(SynthesisError),
    NonFiniteTarget { sample: usize },
    ErrorOverflow,
}
impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP gain search failed: {self:?}")
    }
}
impl Error for SearchError {}
#[derive(Debug, Clone, PartialEq)]
pub struct GainSelection {
    pub gsp0: u8,
    pub excitation: CombinedExcitation,
    pub weighted: [f64; SUBFRAME_SAMPLES],
    pub error_energy: f64,
}

/// IS-54-B §2.1.3.3.2.6.5 minimizes weighted error against the target minus zero-input response.
pub fn search(
    predictor: &SubframePredictor,
    pitch: PitchPrediction,
    adaptive: &[f64; SUBFRAME_SAMPLES],
    codebooks: &CodebookExcitation,
    target: &[f64; SUBFRAME_SAMPLES],
) -> Result<GainSelection, SearchError> {
    if let Some(sample) = target.iter().position(|sample| !sample.is_finite()) {
        return Err(SearchError::NonFiniteTarget { sample });
    }
    let first = GainParameters::from_code(MIN_GAIN_CODE)
        .reconstruct(predictor, pitch, adaptive, codebooks)
        .map_err(SearchError::Gain)?;
    let evaluation = Evaluation {
        target,
        adaptive: WeightedSynthesisFilter::zero_state_response(&predictor.coefficients, adaptive)
            .map_err(SearchError::Filter)?,
        codebooks: WeightedCodebookExcitation {
            codebook_one: WeightedSynthesisFilter::zero_state_response(
                &predictor.coefficients,
                &codebooks.codebook_one,
            )
            .map_err(SearchError::Filter)?,
            codebook_two: WeightedSynthesisFilter::zero_state_response(
                &predictor.coefficients,
                &codebooks.codebook_two,
            )
            .map_err(SearchError::Filter)?,
        },
    };
    let mut best = evaluation.evaluate(MIN_GAIN_CODE, first)?;
    for code in MIN_GAIN_CODE + 1..=MAX_GAIN_CODE {
        let excitation = GainParameters::from_code(code)
            .reconstruct(predictor, pitch, adaptive, codebooks)
            .map_err(SearchError::Gain)?;
        let candidate = evaluation.evaluate(code, excitation)?;
        if candidate.error_energy < best.error_energy {
            best = candidate;
        }
    }
    Ok(best)
}
struct Evaluation<'a> {
    target: &'a [f64; SUBFRAME_SAMPLES],
    adaptive: [f64; SUBFRAME_SAMPLES],
    codebooks: WeightedCodebookExcitation,
}
impl Evaluation<'_> {
    // IS-54-B §2.1.3.3.2.6.5-1 uses the original weighted vectors, not their search projections.
    fn evaluate(
        &self,
        gsp0: u8,
        excitation: CombinedExcitation,
    ) -> Result<GainSelection, SearchError> {
        let gains = excitation.gains;
        let weighted = std::array::from_fn(|sample| {
            gains.beta * self.adaptive[sample]
                + gains.gamma_one * self.codebooks.codebook_one[sample]
                + gains.gamma_two * self.codebooks.codebook_two[sample]
        });
        if weighted.iter().any(|sample| !sample.is_finite()) {
            return Err(SearchError::ErrorOverflow);
        }
        let error_energy = weighted
            .iter()
            .zip(self.target)
            .map(|(weighted, target)| (target - weighted).powi(2))
            .sum::<f64>();
        if !error_energy.is_finite() {
            return Err(SearchError::ErrorOverflow);
        }
        Ok(GainSelection {
            gsp0,
            excitation,
            weighted,
            error_energy,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vselp::{LPC_COEFFICIENTS, PredictorOrigin, VselpSubframe};
    fn predictor(energy: f64) -> SubframePredictor {
        SubframePredictor {
            coefficients: [0.0; LPC_COEFFICIENTS],
            energy_full_scale_squared: energy,
            origin: PredictorOrigin::CurrentFrame,
        }
    }
    fn books() -> CodebookExcitation {
        VselpSubframe::try_new(0, 37, 54, 0)
            .unwrap()
            .codebook_excitation()
    }
    #[test]
    fn silence_ties_select_the_lowest_code_with_zero_excitation_and_known_target_error() {
        let result = search(
            &predictor(0.0),
            PitchPrediction::Enabled,
            &[0.0; SUBFRAME_SAMPLES],
            &books(),
            &[1.0; SUBFRAME_SAMPLES],
        )
        .unwrap();
        assert_eq!(result.gsp0, MIN_GAIN_CODE);
        assert_eq!(result.error_energy, SUBFRAME_SAMPLES as f64);
        assert_eq!(result.excitation.samples, [0.0; SUBFRAME_SAMPLES]);
        assert_eq!(result.weighted, [0.0; SUBFRAME_SAMPLES]);
    }
    #[test]
    fn disabled_pitch_and_empty_history_have_identical_gain_search_results() {
        let predictor = predictor(0.1);
        let books = books();
        let target = [0.25; SUBFRAME_SAMPLES];
        let disabled = search(
            &predictor,
            PitchPrediction::Disabled,
            &[1.0; SUBFRAME_SAMPLES],
            &books,
            &target,
        )
        .unwrap();
        let empty = search(
            &predictor,
            PitchPrediction::Enabled,
            &[0.0; SUBFRAME_SAMPLES],
            &books,
            &target,
        )
        .unwrap();
        assert_eq!(disabled, empty);
        assert_eq!(empty.excitation.gains.beta, 0.0);
    }
    #[test]
    fn malformed_inputs_denominators_predictors_and_error_overflow_are_rejected() {
        let predictor = predictor(0.1);
        let books = books();
        let zero = [0.0; SUBFRAME_SAMPLES];
        let mut target = zero;
        target[3] = f64::NAN;
        assert_eq!(
            search(
                &predictor,
                PitchPrediction::Disabled,
                &zero,
                &books,
                &target
            ),
            Err(SearchError::NonFiniteTarget { sample: 3 })
        );
        let mut invalid = predictor;
        invalid.coefficients[0] = 1.0;
        assert_eq!(
            search(&invalid, PitchPrediction::Disabled, &zero, &books, &zero),
            Err(SearchError::Gain(GainError::Predictor))
        );
        let mut invalid_books = books.clone();
        invalid_books.codebook_one.fill(0.0);
        assert_eq!(
            search(
                &predictor,
                PitchPrediction::Disabled,
                &zero,
                &invalid_books,
                &zero
            ),
            Err(SearchError::Gain(GainError::ZeroVectorEnergy(
                super::super::gains::ExcitationVector::CodebookOne
            )))
        );
        assert_eq!(
            search(
                &predictor,
                PitchPrediction::Disabled,
                &zero,
                &books,
                &[f64::MAX; SUBFRAME_SAMPLES]
            ),
            Err(SearchError::ErrorOverflow)
        );
    }
}
