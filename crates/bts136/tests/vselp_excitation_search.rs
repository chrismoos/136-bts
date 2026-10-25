use bts136::codec::vselp::{LPC_COEFFICIENTS, SUBFRAME_SAMPLES, excitation_search};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reference {
    predictor: [f64; LPC_COEFFICIENTS],
    pitch: Vec<f64>,
    target: Vec<f64>,
    code1: u8,
    code2: u8,
    raw_one: Vec<f64>,
    raw_two: Vec<f64>,
    weighted_one: Vec<f64>,
    weighted_two: Vec<f64>,
}
#[test]
fn sequential_projection_search_matches_independent_whole_vector_decimal_reference() {
    let references: Vec<Reference> =
        serde_yaml_ng::from_str(include_str!("fixtures/vselp-code-search.json")).unwrap();
    for reference in references {
        let pitch: [f64; SUBFRAME_SAMPLES] = reference.pitch.try_into().unwrap();
        let target: [f64; SUBFRAME_SAMPLES] = reference.target.try_into().unwrap();
        let result = excitation_search::search(&reference.predictor, &target, &pitch).unwrap();
        assert_eq!(result.code1, reference.code1);
        assert_eq!(result.code2, reference.code2);
        for (actual, expected) in [
            (result.excitation.codebook_one, reference.raw_one),
            (result.excitation.codebook_two, reference.raw_two),
            (result.weighted.codebook_one, reference.weighted_one),
            (result.weighted.codebook_two, reference.weighted_two),
        ] {
            assert_eq!(expected.len(), SUBFRAME_SAMPLES);
            for (sample, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
                assert!((actual - expected).abs() < 1e-13, "sample {sample}");
            }
        }
    }
}
