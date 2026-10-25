use bts136::codec::vselp::{
    FRAME_SAMPLES, LPC_COEFFICIENTS, SUBFRAMES, VselpParameters, VselpSubframe, decoder::Decoder,
    encoder::Encoder,
};
use bts136::phy::{
    acquisition::decode_forward_traffic,
    forward::ForwardModulator,
    speech,
    sync::SyncWord,
    traffic::{self, SACCH_BITS},
};
use serde::Deserialize;

const SAMPLES_PER_SYMBOL: usize = 8;
const DVCC: u8 = 10;
#[derive(Deserialize)]
struct Seed {
    r0: u8,
    lpc: [u8; LPC_COEFFICIENTS],
}
impl Seed {
    fn parameters(&self) -> VselpParameters {
        VselpParameters::try_new(self.r0, self.lpc, [VselpSubframe::default(); SUBFRAMES]).unwrap()
    }
}
#[derive(Deserialize)]
struct Frame {
    r0: u8,
    lpc: [u8; LPC_COEFFICIENTS],
    subframes: [[u8; 4]; SUBFRAMES],
    decoded_samples: Vec<f64>,
}
impl Frame {
    fn parameters(&self) -> VselpParameters {
        VselpParameters::try_new(
            self.r0,
            self.lpc,
            self.subframes.map(|[lag, code1, code2, gsp0]| {
                VselpSubframe::try_new(lag, code1, code2, gsp0).unwrap()
            }),
        )
        .unwrap()
    }
}
#[derive(Deserialize)]
struct Reference {
    seed: Seed,
    input: Vec<f64>,
    frames: Vec<Frame>,
}
fn reference() -> Reference {
    serde_yaml_ng::from_str(include_str!("fixtures/vselp-encoder.json")).unwrap()
}
fn encoded(reference: &Reference) -> Vec<VselpParameters> {
    let mut encoder = Encoder::new(reference.seed.parameters());
    let (chunks, remainder) = reference.input.as_chunks::<FRAME_SAMPLES>();
    assert!(remainder.is_empty());
    assert_eq!(encoder.push(&chunks[0]).unwrap(), None);
    chunks[1..]
        .iter()
        .map(|chunk| encoder.push(chunk).unwrap().unwrap())
        .collect()
}
fn assert_pcm(actual: [f64; FRAME_SAMPLES], expected: &[f64]) {
    assert_eq!(expected.len(), FRAME_SAMPLES);
    for (sample, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
        assert!(
            (actual - expected).abs() < 1e-10 * expected.abs().max(1.0),
            "sample {sample}: {actual} != {expected}"
        );
    }
}
#[test]
fn streaming_encoder_parameters_and_decoded_pcm_match_independent_decimal_history_reference() {
    let reference = reference();
    let actual = encoded(&reference);
    assert_eq!(actual.len(), reference.frames.len());
    let mut decoder = Decoder::new(reference.seed.parameters());
    for (frame, expected) in actual.iter().zip(&reference.frames) {
        assert_eq!(*frame, expected.parameters());
        assert_pcm(decoder.decode(frame).unwrap(), &expected.decoded_samples);
    }
}
#[test]
fn pcm_encoder_output_survives_complete_forward_iq_chain_and_decodes_to_reference_audio() {
    let reference = reference();
    let actual = encoded(&reference);
    let padding = speech::encode(&VselpParameters::default().speech_bits());
    let mut previous = padding;
    let mut modulator = ForwardModulator::try_new(SAMPLES_PER_SYMBOL).unwrap();
    let mut samples = Vec::new();
    for coded in actual
        .iter()
        .map(|frame| speech::encode(&frame.speech_bits()))
        .chain(std::iter::once(padding))
    {
        let slot = speech::interleave(&previous, &coded);
        let burst =
            traffic::encode_forward(SyncWord::One, &[false; SACCH_BITS], &slot, DVCC, 0).unwrap();
        samples.extend(modulator.encode_slot(&burst));
        previous = coded;
    }
    samples.extend(modulator.finish());
    let bursts = decode_forward_traffic(&samples, modulator.sample_rate_hz()).unwrap();
    assert_eq!(bursts.len(), reference.frames.len() + 1);
    let mut decoder = Decoder::new(reference.seed.parameters());
    for (index, pair) in bursts.windows(2).enumerate() {
        let channel =
            speech::deinterleave(&pair[0].fields.fields.data, &pair[1].fields.fields.data);
        let decoded = speech::decode(&channel).unwrap();
        assert_eq!(decoded.corrected_coded_bits, 0);
        let frame = VselpParameters::from_speech_bits(&decoded.bits);
        assert_eq!(frame, reference.frames[index].parameters());
        assert_pcm(
            decoder.decode(&frame).unwrap(),
            &reference.frames[index].decoded_samples,
        );
    }
}
