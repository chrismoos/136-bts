use bts136::{
    codec::mr74::{
        PARAMETER_COUNT, Parameters,
        decoder::{FRAME_SAMPLES, PcmDecoder},
    },
    phy::acelp_cc1::{self, CHANNEL_BITS, Interleaving},
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
fn cc1_channel_and_speech_chain_preserves_reference_pcm_across_interleaving_depths() {
    let parameters: Vec<ParametersVector> =
        serde_yaml_ng::from_str(include_str!("fixtures/mr74_parameters.json")).unwrap();
    let pcm: Vec<PcmVector> =
        serde_yaml_ng::from_str(include_str!("fixtures/mr74_pcm.json")).unwrap();
    assert_eq!(parameters.len(), 35);
    assert_eq!(pcm.len(), parameters.len());
    for mode in [Interleaving::One, Interleaving::Two, Interleaving::Three] {
        let mut decoder = PcmDecoder::default();
        let mut frames = vec![[false; CHANNEL_BITS]; 2];
        for (index, vector) in parameters.iter().enumerate() {
            let mut encoded = acelp_cc1::encode(&Parameters::try_new(vector.parameters).unwrap());
            encoded[index] ^= true;
            frames.push(encoded);
        }
        frames.extend([[false; CHANNEL_BITS]; 2]);
        let slots: Vec<_> = frames
            .windows(3)
            .map(|history| acelp_cc1::interleave(history.try_into().unwrap(), mode))
            .collect();
        for (index, (parameters, pcm)) in parameters.iter().zip(&pcm).enumerate() {
            assert_eq!(parameters.frame, pcm.frame);
            let channel =
                acelp_cc1::deinterleave(slots[index..index + 3].try_into().unwrap(), mode);
            let decoded = acelp_cc1::decode(&channel).unwrap();
            assert!(decoded.crc_valid);
            assert_eq!(decoded.corrected_coded_bits, 1);
            assert_eq!(decoded.parameters.values(), &parameters.parameters);
            let samples: [i16; FRAME_SAMPLES] = pcm.samples.as_slice().try_into().unwrap();
            assert_eq!(
                decoder.decode_good(&decoded.parameters),
                samples,
                "{mode:?} frame {}",
                pcm.frame
            );
        }
    }
}
