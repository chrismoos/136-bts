use bts136::codec::vselp::{
    LPC_COEFFICIENTS, SUBFRAME_SAMPLES, excitation::LongTermHistory, pitch,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reference {
    history: Vec<f64>,
    predictor: [f64; LPC_COEFFICIENTS],
    target: Vec<f64>,
    lag_code: u8,
    adaptive: Vec<f64>,
    weighted: Vec<f64>,
    optimal_gain: f64,
}
#[test]
fn closed_loop_pitch_matches_independent_decimal_convolution_and_exhaustive_scores() {
    let references: Vec<Reference> =
        serde_yaml_ng::from_str(include_str!("fixtures/vselp-pitch.json")).unwrap();
    for reference in references {
        let mut history = LongTermHistory::new();
        let (chunks, remainder) = reference.history.as_chunks::<SUBFRAME_SAMPLES>();
        assert!(remainder.is_empty());
        for chunk in chunks {
            history.commit(chunk).unwrap();
        }
        let before = history.clone();
        let target: [f64; SUBFRAME_SAMPLES] = reference.target.try_into().unwrap();
        let result = pitch::search(&history, &reference.predictor, &target).unwrap();
        assert_eq!(result.lag_code, reference.lag_code);
        assert_eq!(result.adaptive.as_slice(), reference.adaptive);
        assert!((result.optimal_gain - reference.optimal_gain).abs() < 1e-13);
        assert_eq!(reference.weighted.len(), SUBFRAME_SAMPLES);
        for (actual, expected) in result.weighted.into_iter().zip(reference.weighted) {
            assert!((actual - expected).abs() < 1e-13);
        }
        assert_eq!(history, before);
    }
}
