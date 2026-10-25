use bts136::codec::mr74::{
    PARAMETER_COUNT, Parameters,
    decoder::{FRAME_SAMPLES, PcmDecoder},
};
use serde::Deserialize;
#[derive(Deserialize)]
struct ParametersVector {
    frame: usize,
    parameters: [u16; PARAMETER_COUNT],
}
#[derive(Deserialize)]
struct PcmVector {
    frame: usize,
    samples: Vec<i16>,
}
#[test]
fn consecutive_mr74_speech_parameters_match_native_reference_pcm_sample_for_sample() {
    let parameters: Vec<ParametersVector> =
        serde_yaml_ng::from_str(include_str!("fixtures/mr74_parameters.json")).unwrap();
    let pcm: Vec<PcmVector> =
        serde_yaml_ng::from_str(include_str!("fixtures/mr74_pcm.json")).unwrap();
    assert_eq!(parameters.len(), 35);
    assert_eq!(pcm.len(), parameters.len());
    let mut decoder = PcmDecoder::default();
    for (parameters, pcm) in parameters.iter().zip(&pcm) {
        assert_eq!(parameters.frame, pcm.frame);
        let expected: [i16; FRAME_SAMPLES] = pcm.samples.as_slice().try_into().unwrap();
        let parameters_value = Parameters::try_new(parameters.parameters).unwrap();
        assert_eq!(
            decoder.decode_good(&parameters_value),
            expected,
            "source frame={}",
            parameters.frame
        );
    }
}
