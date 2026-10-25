use bts136::codec::vselp::{
    FRAME_SAMPLES, LPC_COEFFICIENTS, SUBFRAMES, VselpParameters, VselpSubframe, decoder::Decoder,
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
#[derive(Deserialize)]
struct Frame {
    r0: u8,
    lpc: [u8; LPC_COEFFICIENTS],
    subframes: [[u8; 4]; SUBFRAMES],
    samples: Vec<f64>,
}
#[derive(Deserialize)]
struct Reference {
    seed: Seed,
    frames: Vec<Frame>,
}

#[test]
fn streaming_synthesis_matches_independent_decimal_reference_for_every_sample() {
    let reference: Reference =
        serde_yaml_ng::from_str(include_str!("fixtures/vselp-synthesis.json")).unwrap();
    let seed = VselpParameters::try_new(
        reference.seed.r0,
        reference.seed.lpc,
        [VselpSubframe::default(); SUBFRAMES],
    )
    .unwrap();
    let mut decoder = Decoder::new(seed);
    for (frame_index, frame) in reference.frames.into_iter().enumerate() {
        let parameters = VselpParameters::try_new(
            frame.r0,
            frame.lpc,
            frame.subframes.map(|[lag, code1, code2, gsp0]| {
                VselpSubframe::try_new(lag, code1, code2, gsp0).unwrap()
            }),
        )
        .unwrap();
        let decoded = decoder.decode(&parameters).unwrap();
        assert_eq!(frame.samples.len(), FRAME_SAMPLES);
        for (sample, (actual, expected)) in decoded.into_iter().zip(frame.samples).enumerate() {
            let tolerance = 1e-10 * expected.abs().max(1.0);
            assert!(
                (actual - expected).abs() < tolerance,
                "frame {frame_index}, sample {sample}: {actual} != {expected}"
            );
        }
    }
}

#[test]
fn forward_iq_recovers_the_independent_reference_pcm_stream() {
    let reference: Reference =
        serde_yaml_ng::from_str(include_str!("fixtures/vselp-synthesis.json")).unwrap();
    let parameters: Vec<_> = reference
        .frames
        .iter()
        .map(|frame| {
            VselpParameters::try_new(
                frame.r0,
                frame.lpc,
                frame.subframes.map(|[lag, code1, code2, gsp0]| {
                    VselpSubframe::try_new(lag, code1, code2, gsp0).unwrap()
                }),
            )
            .unwrap()
        })
        .collect();
    let padding = speech::encode(&VselpParameters::default().speech_bits());
    let mut previous = padding;
    let mut modulator = ForwardModulator::try_new(SAMPLES_PER_SYMBOL).unwrap();
    let mut samples = Vec::new();
    for coded in parameters
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
    assert_eq!(bursts.len(), parameters.len() + 1);
    let seed = VselpParameters::try_new(
        reference.seed.r0,
        reference.seed.lpc,
        [VselpSubframe::default(); SUBFRAMES],
    )
    .unwrap();
    let mut decoder = Decoder::new(seed);
    for (index, pair) in bursts.windows(2).enumerate() {
        let channel =
            speech::deinterleave(&pair[0].fields.fields.data, &pair[1].fields.fields.data);
        let speech = speech::decode(&channel).unwrap();
        assert_eq!(speech.corrected_coded_bits, 0);
        let recovered = VselpParameters::from_speech_bits(&speech.bits);
        assert_eq!(recovered, parameters[index]);
        let pcm = decoder.decode(&recovered).unwrap();
        for (sample, expected) in pcm.into_iter().zip(&reference.frames[index].samples) {
            assert!((sample - expected).abs() < 1e-10 * expected.abs().max(1.0));
        }
    }
}

#[test]
fn crc_failed_forward_frames_are_masked_before_stateful_synthesis_and_recover_after_two_good_crcs()
{
    let seed = VselpParameters::try_new(
        20,
        [32, 16, 16, 8, 8, 4, 4, 4, 4, 2],
        [VselpSubframe::try_new(1, 37, 54, 170).unwrap(); SUBFRAMES],
    )
    .unwrap();
    let recovery = VselpParameters::try_new(
        10,
        [30, 15, 14, 7, 7, 3, 3, 3, 3, 1],
        [VselpSubframe::try_new(21, 54, 37, 60).unwrap(); SUBFRAMES],
    )
    .unwrap();
    let invalid = "10000000011100100111000110011010100011111110001101100101000111111100011011010110001111111000101001011010010100101101001010010110100101101010001111111000110111010100011111110001101101001000111111100011011010011101011000000000010110100101001011010010100101101001";
    let invalid: [bool; speech::CHANNEL_BITS] = invalid
        .bytes()
        .map(|bit| bit == b'1')
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let good = speech::encode(&seed.speech_bits());
    let recovered = speech::encode(&recovery.speech_bits());
    let words = [
        good, invalid, invalid, invalid, invalid, invalid, invalid, recovered, invalid, recovered,
        recovered,
    ];
    let padding = speech::encode(&VselpParameters::default().speech_bits());
    let mut previous = padding;
    let mut modulator = ForwardModulator::try_new(SAMPLES_PER_SYMBOL).unwrap();
    let mut samples = Vec::new();
    for coded in words.into_iter().chain(std::iter::once(padding)) {
        let slot = speech::interleave(&previous, &coded);
        let burst =
            traffic::encode_forward(SyncWord::One, &[false; SACCH_BITS], &slot, DVCC, 0).unwrap();
        samples.extend(modulator.encode_slot(&burst));
        previous = coded;
    }
    samples.extend(modulator.finish());
    let bursts = decode_forward_traffic(&samples, modulator.sample_rate_hz()).unwrap();
    assert_eq!(bursts.len(), words.len() + 1);
    let expected_r0 = [20, 20, 20, 18, 16, 14, 0, 0, 0, 0, 10];
    let mut decoder = Decoder::new(seed.clone());
    let mut expected_decoder = Decoder::new(seed.clone());
    for (index, pair) in bursts.windows(2).enumerate() {
        let channel =
            speech::deinterleave(&pair[0].fields.fields.data, &pair[1].fields.fields.data);
        let candidate = speech::decode_candidate(&channel).unwrap();
        let expected_crc = if words[index] == invalid {
            speech::CrcStatus::Failed
        } else {
            speech::CrcStatus::Valid
        };
        assert_eq!(candidate.crc, expected_crc);
        assert_eq!(candidate.frame.corrected_coded_bits, 0);
        if expected_crc == speech::CrcStatus::Failed {
            assert_eq!(speech::decode(&channel), Err(speech::SpeechError::Crc));
        }
        let received = VselpParameters::from_speech_bits(&candidate.frame.bits);
        let expected_lpc = if index + 1 == words.len() {
            *recovery.lpc()
        } else {
            *seed.lpc()
        };
        let masked =
            VselpParameters::try_new(expected_r0[index], expected_lpc, *received.subframes())
                .unwrap();
        assert_eq!(
            decoder.decode_candidate(&candidate).unwrap(),
            expected_decoder.decode(&masked).unwrap()
        );
    }
}
