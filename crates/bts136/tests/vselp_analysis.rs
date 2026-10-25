use bts136::codec::vselp::{
    FRAME_SAMPLES, LPC_COEFFICIENTS, SUBFRAMES, VselpParameters, VselpSubframe,
    analysis::{ANALYSIS_SAMPLES, analyze_energy, analyze_predictor},
    stream_analysis::StreamAnalyzer,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reference {
    name: String,
    samples: Vec<f64>,
    lpc: [u8; LPC_COEFFICIENTS],
    r0: u8,
}
#[test]
fn quantized_flat_codes_match_independent_decimal_reference_for_all_analysis_inputs() {
    let references: Vec<Reference> =
        serde_yaml_ng::from_str(include_str!("fixtures/vselp-analysis.json")).unwrap();
    for reference in references {
        let samples: [f64; ANALYSIS_SAMPLES] = reference.samples.try_into().unwrap();
        let codes = analyze_predictor(&samples).unwrap();
        assert_eq!(codes, reference.lpc, "{}", reference.name);
        assert_eq!(
            analyze_energy(&samples).unwrap(),
            reference.r0,
            "{} energy",
            reference.name
        );
        let frame =
            VselpParameters::try_new(reference.r0, codes, [VselpSubframe::default(); SUBFRAMES])
                .unwrap();
        assert!(
            frame
                .reflection_coefficients()
                .iter()
                .all(|reflection| reflection.abs() < 1.0)
        );
        assert!(
            frame
                .predictor_coefficients()
                .iter()
                .all(|coefficient| coefficient.is_finite())
        );
    }
}

#[test]
fn buffered_frame_analysis_matches_independent_predictor_and_energy_codes_at_the_required_alignment()
 {
    let references: Vec<Reference> =
        serde_yaml_ng::from_str(include_str!("fixtures/vselp-analysis.json")).unwrap();
    for reference in references {
        let mut input = [0.0; FRAME_SAMPLES * 2];
        input[55..225].copy_from_slice(&reference.samples);
        let frames = input.as_chunks::<FRAME_SAMPLES>().0;
        let mut analyzer = StreamAnalyzer::new();
        assert_eq!(analyzer.push(&frames[0]).unwrap(), None);
        let result = analyzer.push(&frames[1]).unwrap().unwrap();
        assert_eq!(result.samples, frames[0]);
        assert_eq!(result.lpc, reference.lpc, "{} LPC", reference.name);
        assert_eq!(result.r0, reference.r0, "{} energy", reference.name);
    }
}
