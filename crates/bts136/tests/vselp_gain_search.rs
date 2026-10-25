use bts136::codec::vselp::{
    LPC_COEFFICIENTS, PredictorOrigin, SUBFRAME_SAMPLES, SubframePredictor, VselpSubframe,
    gain_search, gains::PitchPrediction,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reference {
    predictor: [f64; LPC_COEFFICIENTS],
    energy: f64,
    pitch_enabled: bool,
    adaptive: Vec<f64>,
    code1: u8,
    code2: u8,
    target: Vec<f64>,
    gsp0: u8,
    gains: [f64; 3],
    excitation: Vec<f64>,
    weighted: Vec<f64>,
    error_energy: f64,
}
#[test]
fn exhaustive_gsp0_selection_matches_independent_decimal_candidate_synthesis() {
    let references: Vec<Reference> =
        serde_yaml_ng::from_str(include_str!("fixtures/vselp-gain-search.json")).unwrap();
    for reference in references {
        let predictor = SubframePredictor {
            coefficients: reference.predictor,
            energy_full_scale_squared: reference.energy,
            origin: PredictorOrigin::CurrentFrame,
        };
        let pitch = if reference.pitch_enabled {
            PitchPrediction::Enabled
        } else {
            PitchPrediction::Disabled
        };
        let adaptive: [f64; SUBFRAME_SAMPLES] = reference.adaptive.try_into().unwrap();
        let target: [f64; SUBFRAME_SAMPLES] = reference.target.try_into().unwrap();
        let codebooks = VselpSubframe::try_new(0, reference.code1, reference.code2, 0)
            .unwrap()
            .codebook_excitation();
        let selected =
            gain_search::search(&predictor, pitch, &adaptive, &codebooks, &target).unwrap();
        assert_eq!(selected.gsp0, reference.gsp0);
        let gains = selected.excitation.gains;
        for (actual, expected) in [gains.beta, gains.gamma_one, gains.gamma_two]
            .into_iter()
            .zip(reference.gains)
        {
            assert!((actual - expected).abs() < 1e-13);
        }
        for (actual, expected) in [
            (selected.excitation.samples, reference.excitation),
            (selected.weighted, reference.weighted),
        ] {
            assert_eq!(expected.len(), SUBFRAME_SAMPLES);
            for (sample, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
                assert!((actual - expected).abs() < 1e-13, "sample {sample}");
            }
        }
        assert!((selected.error_energy - reference.error_energy).abs() < 1e-17);
    }
}
