use super::{CodebookExcitation, SUBFRAME_SAMPLES, SubframePredictor, interpolation};
use std::{error::Error, fmt};

mod codebook;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PitchPrediction {
    Enabled,
    Disabled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcitationVector {
    Adaptive,
    CodebookOne,
    CodebookTwo,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainError {
    Parameters,
    Predictor,
    FrameEnergy,
    NonFiniteVector(ExcitationVector),
    ZeroVectorEnergy(ExcitationVector),
    Overflow,
}
impl fmt::Display for GainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP gain reconstruction failed: {self:?}")
    }
}
impl Error for GainError {}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainParameters {
    gs: f64,
    p0: f64,
    p1: f64,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExcitationGains {
    pub beta: f64,
    pub gamma_one: f64,
    pub gamma_two: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CombinedExcitation {
    pub samples: [f64; SUBFRAME_SAMPLES],
    pub gains: ExcitationGains,
}
impl GainParameters {
    pub fn from_code(code: u8) -> Self {
        codebook::PARAMETERS[usize::from(code)]
    }
    pub fn try_new(gs: f64, p0: f64, p1: f64) -> Result<Self, GainError> {
        if [gs, p0, p1]
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
            || p0 + p1 > 1.0
        {
            return Err(GainError::Parameters);
        }
        Ok(Self { gs, p0, p1 })
    }
    // IS-54-B §§2.1.3.3.2.6.5.2 and 2.2.2.2.4.4.5, unweighted vector energies and positive gains.
    pub fn reconstruct(
        &self,
        predictor: &SubframePredictor,
        pitch: PitchPrediction,
        adaptive: &[f64; SUBFRAME_SAMPLES],
        codebooks: &CodebookExcitation,
    ) -> Result<CombinedExcitation, GainError> {
        let residual_energy = residual_energy(predictor)?;
        let energy = residual_energy * self.gs;
        if !energy.is_finite() {
            return Err(GainError::Overflow);
        }
        let adaptive_energy = vector_energy(adaptive, ExcitationVector::Adaptive)?;
        let first_energy = vector_energy(&codebooks.codebook_one, ExcitationVector::CodebookOne)?;
        let second_energy = vector_energy(&codebooks.codebook_two, ExcitationVector::CodebookTwo)?;
        let beta = if pitch == PitchPrediction::Disabled || adaptive_energy == 0.0 {
            0.0
        } else {
            gain(
                energy * self.p0,
                adaptive_energy,
                ExcitationVector::Adaptive,
            )?
        };
        let gamma_one = gain(
            energy * self.p1,
            first_energy,
            ExcitationVector::CodebookOne,
        )?;
        let gamma_two = gain(
            energy * (1.0 - (self.p0 + self.p1)),
            second_energy,
            ExcitationVector::CodebookTwo,
        )?;
        let samples = std::array::from_fn(|sample| {
            beta * adaptive[sample]
                + gamma_one * codebooks.codebook_one[sample]
                + gamma_two * codebooks.codebook_two[sample]
        });
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(GainError::Overflow);
        }
        Ok(CombinedExcitation {
            samples,
            gains: ExcitationGains {
                beta,
                gamma_one,
                gamma_two,
            },
        })
    }
}
// IS-54-B Equation 2.1.3.3.2.6.5.1-3: N R'q(0) multiplied by normalized prediction gain.
fn residual_energy(predictor: &SubframePredictor) -> Result<f64, GainError> {
    let energy = predictor.energy_full_scale_squared;
    if !energy.is_finite() || energy < 0.0 {
        return Err(GainError::FrameEnergy);
    }
    let reflections = interpolation::reflection_coefficients(&predictor.coefficients)
        .ok_or(GainError::Predictor)?;
    let gain = reflections
        .iter()
        .map(|reflection| 1.0 - reflection * reflection)
        .product::<f64>();
    let residual = energy * (SUBFRAME_SAMPLES as f64 * gain);
    if !residual.is_finite() {
        return Err(GainError::Overflow);
    }
    Ok(residual)
}
fn vector_energy(
    samples: &[f64; SUBFRAME_SAMPLES],
    vector: ExcitationVector,
) -> Result<f64, GainError> {
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(GainError::NonFiniteVector(vector));
    }
    let energy = samples.iter().map(|sample| sample * sample).sum::<f64>();
    if !energy.is_finite() {
        return Err(GainError::Overflow);
    }
    Ok(energy)
}
fn gain(allocated: f64, vector_energy: f64, vector: ExcitationVector) -> Result<f64, GainError> {
    if allocated == 0.0 {
        return Ok(0.0);
    }
    if vector_energy == 0.0 {
        return Err(GainError::ZeroVectorEnergy(vector));
    }
    let gain = (allocated / vector_energy).sqrt();
    if !gain.is_finite() {
        return Err(GainError::Overflow);
    }
    Ok(gain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vselp::{LPC_COEFFICIENTS, PredictorOrigin, VselpSubframe};
    #[test]
    fn gain_codewords_match_scanned_table_boundaries_and_ocr_discrepancies() {
        for (code, gs, p0, p1) in [
            (0, 0.1674511004e-02, 0.1628413945e+00, 0.5090150237e+00),
            (60, 0.1174440011e+00, 0.7431836128e+00, 0.1049233973e+00),
            (127, 0.4775207937e+00, 0.9821900129e+00, 0.6872606464e-02),
            (128, 0.2058212012e+00, 0.6196407974e-01, 0.7798926234e+00),
            (170, 0.4068664014e+00, 0.7727342844e+00, 0.1524548978e+00),
            (171, 0.4181675911e+00, 0.8476157784e+00, 0.1214092970e+00),
            (172, 0.4287396073e+00, 0.7954624295e+00, 0.8431659639e-01),
            (235, 0.1811290979e+01, 0.8061832190e+00, 0.1446519941e+00),
            (242, 0.1025166988e+01, 0.8316953182e+00, 0.7419369370e-01),
            (255, 0.2636862040e+01, 0.9607506990e+00, 0.1253264863e-01),
        ] {
            assert_eq!(
                GainParameters::from_code(code),
                GainParameters::try_new(gs, p0, p1).unwrap()
            );
        }
    }
    #[test]
    fn every_gain_codeword_has_valid_positive_energy_allocations() {
        for code in u8::MIN..=u8::MAX {
            let parameters = GainParameters::from_code(code);
            assert!(parameters.gs > 0.0);
            assert_eq!(
                GainParameters::try_new(parameters.gs, parameters.p0, parameters.p1),
                Ok(parameters)
            );
            assert!(parameters.p0 > 0.0);
            assert!(parameters.p1 > 0.0);
            assert!(parameters.p0 + parameters.p1 < 1.0);
        }
    }
    #[test]
    fn encoded_subframes_reconstruct_all_gain_codewords_with_both_pitch_modes() {
        let predictor = predictor();
        let adaptive = [1.0; SUBFRAME_SAMPLES];
        for code in u8::MIN..=u8::MAX {
            let subframe = VselpSubframe::try_new(1, 37, 54, code).unwrap();
            let parameters = subframe.gain_parameters();
            let codebooks = subframe.codebook_excitation();
            let enabled = parameters
                .reconstruct(&predictor, PitchPrediction::Enabled, &adaptive, &codebooks)
                .unwrap();
            let disabled = parameters
                .reconstruct(&predictor, PitchPrediction::Disabled, &adaptive, &codebooks)
                .unwrap();
            let expected_adaptive_energy =
                residual_energy(&predictor).unwrap() * parameters.gs * parameters.p0;
            assert!(
                (enabled.gains.beta.powi(2) * SUBFRAME_SAMPLES as f64 - expected_adaptive_energy)
                    .abs()
                    < 1e-13
            );
            assert_eq!(disabled.gains.beta, 0.0);
            assert_eq!(enabled.gains.gamma_one, disabled.gains.gamma_one);
            assert_eq!(enabled.gains.gamma_two, disabled.gains.gamma_two);
            assert!(
                enabled
                    .samples
                    .iter()
                    .chain(&disabled.samples)
                    .all(|sample| sample.is_finite())
            );
        }
    }
    fn predictor() -> SubframePredictor {
        SubframePredictor {
            coefficients: [0.0; LPC_COEFFICIENTS],
            energy_full_scale_squared: 0.1,
            origin: PredictorOrigin::CurrentFrame,
        }
    }
    fn codebooks() -> CodebookExcitation {
        CodebookExcitation {
            codebook_one: [2.0; SUBFRAME_SAMPLES],
            codebook_two: [-1.0; SUBFRAME_SAMPLES],
        }
    }
    #[test]
    fn reconstruction_matches_independent_scalar_energies_and_combined_excitation() {
        let result = GainParameters::try_new(2.0, 0.25, 0.5)
            .unwrap()
            .reconstruct(
                &predictor(),
                PitchPrediction::Enabled,
                &[1.0; SUBFRAME_SAMPLES],
                &codebooks(),
            )
            .unwrap();
        assert!((result.gains.beta - 0.22360679774997896).abs() < 1e-14);
        assert!((result.gains.gamma_one - 0.15811388300841897).abs() < 1e-14);
        assert!((result.gains.gamma_two - 0.22360679774997896).abs() < 1e-14);
        assert!(
            result
                .samples
                .iter()
                .all(|sample| (*sample - 0.31622776601683794).abs() < 1e-14)
        );
    }
    #[test]
    fn disabled_pitch_and_empty_history_zero_beta_without_redistributing_codebook_energy() {
        let parameters = GainParameters::try_new(2.0, 0.25, 0.5).unwrap();
        let disabled = parameters
            .reconstruct(
                &predictor(),
                PitchPrediction::Disabled,
                &[1.0; SUBFRAME_SAMPLES],
                &codebooks(),
            )
            .unwrap();
        let empty = parameters
            .reconstruct(
                &predictor(),
                PitchPrediction::Enabled,
                &[0.0; SUBFRAME_SAMPLES],
                &codebooks(),
            )
            .unwrap();
        assert_eq!(disabled, empty);
        assert_eq!(disabled.gains.beta, 0.0);
        let mut predictor = predictor();
        predictor.energy_full_scale_squared = 0.0;
        let silent = parameters
            .reconstruct(
                &predictor,
                PitchPrediction::Enabled,
                &[0.0; SUBFRAME_SAMPLES],
                &codebooks(),
            )
            .unwrap();
        assert_eq!(silent.samples, [0.0; SUBFRAME_SAMPLES]);
    }
    #[test]
    fn residual_energy_uses_reflections_of_the_actual_subframe_predictor() {
        let mut predictor = predictor();
        predictor.coefficients[0] = 0.5;
        assert!((residual_energy(&predictor).unwrap() - 3.0).abs() < 1e-14);
    }
    #[test]
    fn invalid_parameters_predictors_vectors_and_zero_denominators_are_rejected() {
        for values in [(f64::NAN, 0.0, 0.0), (-1.0, 0.0, 0.0), (1.0, 0.8, 0.3)] {
            assert_eq!(
                GainParameters::try_new(values.0, values.1, values.2),
                Err(GainError::Parameters)
            );
        }
        let parameters = GainParameters::try_new(2.0, 0.25, 0.5).unwrap();
        let mut bad = predictor();
        bad.coefficients[0] = 1.0;
        assert_eq!(
            parameters.reconstruct(
                &bad,
                PitchPrediction::Disabled,
                &[0.0; SUBFRAME_SAMPLES],
                &codebooks()
            ),
            Err(GainError::Predictor)
        );
        let mut bad = codebooks();
        bad.codebook_one.fill(0.0);
        assert_eq!(
            parameters.reconstruct(
                &predictor(),
                PitchPrediction::Disabled,
                &[0.0; SUBFRAME_SAMPLES],
                &bad
            ),
            Err(GainError::ZeroVectorEnergy(ExcitationVector::CodebookOne))
        );
        let mut zero_second = codebooks();
        zero_second.codebook_two.fill(0.0);
        let boundary = GainParameters::try_new(1.0, 0.8, 0.2)
            .unwrap()
            .reconstruct(
                &predictor(),
                PitchPrediction::Disabled,
                &[0.0; SUBFRAME_SAMPLES],
                &zero_second,
            )
            .unwrap();
        assert_eq!(boundary.gains.gamma_two, 0.0);
        let mut bad_energy = predictor();
        bad_energy.energy_full_scale_squared = f64::NAN;
        assert_eq!(
            parameters.reconstruct(
                &bad_energy,
                PitchPrediction::Disabled,
                &[0.0; SUBFRAME_SAMPLES],
                &codebooks()
            ),
            Err(GainError::FrameEnergy)
        );
        bad.codebook_one[0] = f64::NAN;
        assert_eq!(
            parameters.reconstruct(
                &predictor(),
                PitchPrediction::Disabled,
                &[0.0; SUBFRAME_SAMPLES],
                &bad
            ),
            Err(GainError::NonFiniteVector(ExcitationVector::CodebookOne))
        );
    }
}
