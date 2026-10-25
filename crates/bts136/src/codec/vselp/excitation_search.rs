use super::{
    CodebookExcitation, LPC_COEFFICIENTS, SUBFRAME_CODE_BITS, SUBFRAME_SAMPLES,
    codebooks::{self, BASIS_COUNT},
    gains::ExcitationVector,
    synthesis::SynthesisError,
    weighting::{WeightedCodebookExcitation, WeightedSynthesisFilter},
};
use std::{error::Error, fmt};

const MIN_CODE: u8 = 0;
const MAX_CODE: u8 = (1 << SUBFRAME_CODE_BITS) - 1;
const MAX_PROJECTIONS: usize = 2;
const ROUNDOFF_OPERATIONS: usize = MAX_PROJECTIONS * (3 * SUBFRAME_SAMPLES + 1) + BASIS_COUNT;
type Basis = [[f64; SUBFRAME_SAMPLES]; BASIS_COUNT];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    NonFiniteTarget { sample: usize },
    NonFinitePitch { sample: usize },
    Filter(SynthesisError),
    Arithmetic,
    Degenerate(ExcitationVector),
}
impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP excitation search failed: {self:?}")
    }
}
impl Error for SearchError {}
#[derive(Debug, Clone, PartialEq)]
pub struct CodebookSelection {
    pub code1: u8,
    pub code2: u8,
    pub excitation: CodebookExcitation,
    pub weighted: WeightedCodebookExcitation,
}
struct Selected {
    code: u8,
    orthogonalized: [f64; SUBFRAME_SAMPLES],
}

/// IS-54-B §§2.1.3.3.2.6.4.2–3 search both books against the weighted target minus zero-input response.
pub fn search(
    predictor: &[f64; LPC_COEFFICIENTS],
    target: &[f64; SUBFRAME_SAMPLES],
    pitch: &[f64; SUBFRAME_SAMPLES],
) -> Result<CodebookSelection, SearchError> {
    if let Some(sample) = target.iter().position(|sample| !sample.is_finite()) {
        return Err(SearchError::NonFiniteTarget { sample });
    }
    if let Some(sample) = pitch.iter().position(|sample| !sample.is_finite()) {
        return Err(SearchError::NonFinitePitch { sample });
    }
    let first = filter_basis(0, predictor)?;
    let second = filter_basis(1, predictor)?;
    let mut first_orthogonal = first;
    for basis in &mut first_orthogonal {
        orthogonalize(basis, pitch)?;
    }
    let selected_first = select(
        &first_orthogonal,
        &first,
        target,
        ExcitationVector::CodebookOne,
    )?;
    let mut second_orthogonal = second;
    // IS-54-B §2.1.3.3.2.6.4.2.2 projects against pitch and then the orthogonalized first vector.
    for basis in &mut second_orthogonal {
        orthogonalize(basis, pitch)?;
        orthogonalize(basis, &selected_first.orthogonalized)?;
    }
    let selected_second = select(
        &second_orthogonal,
        &second,
        target,
        ExcitationVector::CodebookTwo,
    )?;
    Ok(CodebookSelection {
        code1: selected_first.code,
        code2: selected_second.code,
        excitation: CodebookExcitation {
            codebook_one: codebooks::vector(codebooks::basis(0), selected_first.code),
            codebook_two: codebooks::vector(codebooks::basis(1), selected_second.code),
        },
        weighted: WeightedCodebookExcitation {
            codebook_one: codebooks::vector(&first, selected_first.code),
            codebook_two: codebooks::vector(&second, selected_second.code),
        },
    })
}
fn filter_basis(book: usize, predictor: &[f64; LPC_COEFFICIENTS]) -> Result<Basis, SearchError> {
    let mut output = [[0.0; SUBFRAME_SAMPLES]; BASIS_COUNT];
    for (output, input) in output.iter_mut().zip(codebooks::basis(book)) {
        *output = WeightedSynthesisFilter::zero_state_response(predictor, input)
            .map_err(SearchError::Filter)?;
    }
    Ok(output)
}
fn dot(a: &[f64; SUBFRAME_SAMPLES], b: &[f64; SUBFRAME_SAMPLES]) -> Result<f64, SearchError> {
    let result = a.iter().zip(b).map(|(a, b)| a * b).sum::<f64>();
    if !result.is_finite() {
        return Err(SearchError::Arithmetic);
    }
    Ok(result)
}
fn orthogonalize(
    vector: &mut [f64; SUBFRAME_SAMPLES],
    against: &[f64; SUBFRAME_SAMPLES],
) -> Result<(), SearchError> {
    let energy = dot(against, against)?;
    if energy == 0.0 {
        return Ok(());
    }
    let projection = dot(vector, against)? / energy;
    if !projection.is_finite() {
        return Err(SearchError::Arithmetic);
    }
    for (sample, against) in vector.iter_mut().zip(against) {
        *sample -= projection * against;
        if !sample.is_finite() {
            return Err(SearchError::Arithmetic);
        }
    }
    Ok(())
}
fn select(
    basis: &Basis,
    unprojected: &Basis,
    target: &[f64; SUBFRAME_SAMPLES],
    book: ExcitationVector,
) -> Result<Selected, SearchError> {
    let mut basis_norm_sum = 0.0;
    for vector in unprojected {
        basis_norm_sum += dot(vector, vector)?.sqrt();
    }
    let accumulated_error = ROUNDOFF_OPERATIONS as f64 * f64::EPSILON;
    let amplitude_bound = accumulated_error / (1.0 - accumulated_error) * basis_norm_sum;
    let energy_bound = amplitude_bound * amplitude_bound;
    if !energy_bound.is_finite() {
        return Err(SearchError::Arithmetic);
    }
    let mut best = None;
    let mut best_metric = 0.0;
    // IS-54-B §2.1.3.3.2.6.4.3 uses complementary words to select the nonnegative gain sign.
    for code in MIN_CODE..=MAX_CODE {
        let vector = codebooks::vector(basis, code);
        let energy = dot(&vector, &vector)?;
        let correlation = dot(&vector, target)?;
        // A numerically rank-lost vector turns projection roundoff into a false finite search score.
        if energy <= energy_bound || correlation < 0.0 {
            continue;
        }
        let metric = correlation / energy.sqrt();
        if !metric.is_finite() {
            return Err(SearchError::Arithmetic);
        }
        if best.is_none() || metric > best_metric {
            best = Some(Selected {
                code,
                orthogonalized: vector,
            });
            best_metric = metric;
        }
    }
    best.ok_or(SearchError::Degenerate(book))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_removes_only_the_selected_component_and_zero_pitch_is_identity() {
        let mut vector = [0.0; SUBFRAME_SAMPLES];
        vector[0] = 7.0;
        vector[1] = 4.0;
        let original = vector;
        orthogonalize(&mut vector, &[0.0; SUBFRAME_SAMPLES]).unwrap();
        assert_eq!(vector, original);
        let mut against = [0.0; SUBFRAME_SAMPLES];
        against[0] = 2.0;
        orthogonalize(&mut vector, &against).unwrap();
        assert_eq!(vector[0], 0.0);
        assert_eq!(vector[1], 4.0);
        assert_eq!(dot(&vector, &against).unwrap(), 0.0);
    }
    #[test]
    fn reversing_the_target_selects_complementary_words_with_positive_gain() {
        let predictor = [0.0; LPC_COEFFICIENTS];
        let target = std::array::from_fn(|sample| ((sample * 37 % 101) as f64 - 50.0) / 64.0);
        let pitch = [0.0; SUBFRAME_SAMPLES];
        let forward = search(&predictor, &target, &pitch).unwrap();
        let reverse = search(&predictor, &target.map(|x| -x), &pitch).unwrap();
        assert_eq!(forward.code1 ^ MAX_CODE, reverse.code1);
        assert_eq!(forward.code2 ^ MAX_CODE, reverse.code2);
        assert_eq!(
            forward.excitation.codebook_one.map(|x| -x),
            reverse.excitation.codebook_one
        );
        assert_eq!(
            forward.excitation.codebook_two.map(|x| -x),
            reverse.excitation.codebook_two
        );
        let zero = search(&predictor, &pitch, &pitch).unwrap();
        assert_eq!(zero.code1, 0);
        assert_eq!(zero.code2, 0);
    }
    #[test]
    fn invalid_inputs_and_degenerate_bases_return_typed_errors() {
        let mut predictor = [0.0; LPC_COEFFICIENTS];
        predictor[0] = 1.1;
        let zero = [0.0; SUBFRAME_SAMPLES];
        assert_eq!(
            search(&predictor, &zero, &zero),
            Err(SearchError::Filter(SynthesisError::UnstablePredictor))
        );
        let predictor = [0.0; LPC_COEFFICIENTS];
        let mut invalid = zero;
        invalid[3] = f64::NAN;
        assert_eq!(
            search(&predictor, &invalid, &zero),
            Err(SearchError::NonFiniteTarget { sample: 3 })
        );
        assert_eq!(
            search(&predictor, &zero, &invalid),
            Err(SearchError::NonFinitePitch { sample: 3 })
        );
        assert_eq!(
            search(&predictor, &zero, &[f64::MAX; SUBFRAME_SAMPLES]),
            Err(SearchError::Arithmetic)
        );
        assert!(matches!(
            select(
                &[[0.0; SUBFRAME_SAMPLES]; BASIS_COUNT],
                &[[0.0; SUBFRAME_SAMPLES]; BASIS_COUNT],
                &zero,
                ExcitationVector::CodebookOne
            ),
            Err(SearchError::Degenerate(ExcitationVector::CodebookOne))
        ));
    }
    #[test]
    fn projected_vectors_below_the_basis_roundoff_bound_are_rejected_as_degenerate() {
        let mut original = [[0.0; SUBFRAME_SAMPLES]; BASIS_COUNT];
        original[0][0] = 1.0;
        let mut projected = original;
        projected[0][0] = f64::EPSILON;
        assert!(matches!(
            select(
                &projected,
                &original,
                &[1.0; SUBFRAME_SAMPLES],
                ExcitationVector::CodebookOne
            ),
            Err(SearchError::Degenerate(ExcitationVector::CodebookOne))
        ));
    }
}
