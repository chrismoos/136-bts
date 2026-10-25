use bts136::{
    codec::mr74::{
        self, PARAMETER_COUNT, Parameters,
        receiver::{Input, Receiver},
    },
    l3::legacy_traffic::TrafficMessage,
    link::{
        traffic::{self, SignalingWord},
        voice::{FrameKind, TrafficFrameReceiver, TransmitFrame},
    },
    phy::speech::CrcStatus,
};
use serde::Deserialize;
#[derive(Deserialize)]
struct Vector {
    parameters: [u16; PARAMETER_COUNT],
}
fn parameters() -> Vec<Parameters> {
    let vectors: Vec<Vector> =
        serde_yaml_ng::from_str(include_str!("fixtures/mr74_parameters.json")).unwrap();
    vectors
        .into_iter()
        .map(|v| Parameters::try_new(v.parameters).unwrap())
        .collect()
}
#[test]
fn interleaved_acelp_speech_matches_reference_receiver_and_preserves_parameters() {
    let parameters = parameters();
    let mut receiver = TrafficFrameReceiver::acelp(1);
    let mut reference = Receiver::default();
    let mut frames: Vec<_> = parameters.iter().map(TransmitFrame::acelp).collect();
    frames.push(TransmitFrame::facch(&SignalingWord::filler(), 1));
    let mut previous = TransmitFrame::facch(&SignalingWord::filler(), 1);
    for (index, current) in frames.iter().enumerate() {
        let data = TransmitFrame::interleave(&previous, current);
        let received = receiver.push(Some(&data)).unwrap();
        if index == 0 {
            assert!(received.is_none());
        } else {
            let received = received.unwrap();
            assert_eq!(
                received.kind,
                FrameKind::Speech {
                    crc: CrcStatus::Valid,
                    corrected_coded_bits: 0,
                    coded_bits: bts136::phy::acelp_cc1::PROTECTED_BITS as u16,
                }
            );
            assert_eq!(received.acelp_parameters, Some(parameters[index - 1]));
            assert!(received.parameters.is_none());
            let expected = reference
                .receive(Input::SpeechGood(parameters[index - 1]))
                .unwrap();
            assert_eq!(
                received.samples,
                expected.samples_q0.map(|s| f64::from(s) / 32768.0)
            );
        }
        previous = current.clone();
    }
}
#[test]
fn facch_stealing_initial_erasures_and_receive_reset_recover_to_acelp() {
    let parameters = parameters();
    let word = traffic::encode(&TrafficMessage::Connect).unwrap().remove(0);
    let filler = TransmitFrame::facch(&SignalingWord::filler(), 1);
    let facch = TransmitFrame::facch(&word, 1);
    let speech = TransmitFrame::acelp(&parameters[0]);
    let mut receiver = TrafficFrameReceiver::acelp(1);
    assert!(receiver.push(None).unwrap().is_none());
    assert_eq!(
        receiver.push(None).unwrap().unwrap().kind,
        FrameKind::Missing
    );
    receiver.reset(bts136::codec::vselp::VselpParameters::default());
    assert_eq!(receiver.codec(), bts136::codec::speech::Codec::AcelpCc1);
    let slots = [
        TransmitFrame::interleave(&filler, &facch),
        TransmitFrame::interleave(&facch, &speech),
        TransmitFrame::interleave(&speech, &filler),
    ];
    assert!(receiver.push(Some(&slots[0])).unwrap().is_none());
    let received = receiver.push(Some(&slots[1])).unwrap().unwrap();
    let FrameKind::Facch(received) = received.kind else {
        panic!("FACCH replacement must remain signaling")
    };
    assert_eq!(received.message, word.contents);
    let received = receiver.push(Some(&slots[2])).unwrap().unwrap();
    assert_eq!(received.acelp_parameters, Some(parameters[0]));
    assert!(matches!(
        received.kind,
        FrameKind::Speech {
            crc: CrcStatus::Valid,
            ..
        }
    ));
}
#[test]
fn first_corrupt_acelp_frame_is_concealed_without_established_speech() {
    let mut receiver = TrafficFrameReceiver::acelp(1);
    let noise = std::array::from_fn(|i| (i * 17 + 11) % 31 < 15);
    assert!(receiver.push(Some(&noise)).unwrap().is_none());
    let frame = receiver.push(Some(&noise)).unwrap().unwrap();
    assert!(!matches!(
        frame.kind,
        FrameKind::Speech {
            crc: CrcStatus::Valid,
            ..
        }
    ));
    assert!(frame.samples.iter().all(|v| v.is_finite()));
}
#[test]
fn runtime_pcm_encoder_rejects_nonfinite_input_without_advancing_state() {
    use bts136::codec::speech::{Codec, EncodeError, Encoder};
    for codec in [Codec::Vselp, Codec::AcelpCc1] {
        let mut encoder = Encoder::new(codec);
        let saved = encoder.clone();
        let mut samples = [0.0; mr74::decoder::FRAME_SAMPLES];
        samples[23] = f64::NAN;
        assert_eq!(
            encoder.encode(&samples),
            Err(EncodeError::NonFinite { sample: 23 })
        );
        assert_eq!(encoder, saved);
    }
}

#[test]
fn bundled_sample_transcodes_through_each_speech_codec_across_playback_wrap() {
    use bts136::{
        codec::{
            speech::{Codec, Encoder, Parameters as SpeechParameters},
            vselp::VselpParameters,
        },
        media::sample::SampleAudio,
    };
    const FRAMES: u64 = 360;
    for codec in [Codec::Vselp, Codec::AcelpCc1] {
        let source = SampleAudio::new(0);
        let mut encoder = Encoder::new(codec);
        let mut receiver = match codec {
            Codec::Vselp => TrafficFrameReceiver::new(1, VselpParameters::default()),
            Codec::AcelpCc1 => TrafficFrameReceiver::acelp(1),
        };
        let mut previous = TransmitFrame::facch(&SignalingWord::filler(), 1);
        let mut expected = None;
        let mut decoded_count = 0;
        let mut energy = 0.0;
        let started = std::time::Instant::now();
        for interval in 0..FRAMES {
            let parameters = encoder.encode(&source.frame(interval).unwrap()).unwrap();
            let frame = parameters.as_ref().map_or_else(
                || TransmitFrame::facch(&SignalingWord::filler(), 1),
                TransmitFrame::from_parameters,
            );
            let received = receiver
                .push(Some(&TransmitFrame::interleave(&previous, &frame)))
                .unwrap();
            if let Some(expected) = expected {
                let received = received.unwrap();
                assert!(matches!(
                    received.kind,
                    FrameKind::Speech {
                        crc: CrcStatus::Valid,
                        corrected_coded_bits: 0,
                        ..
                    }
                ));
                match expected {
                    SpeechParameters::Vselp(parameters) => {
                        assert_eq!(received.parameters, Some(parameters))
                    }
                    SpeechParameters::AcelpCc1(parameters) => {
                        assert_eq!(received.acelp_parameters, Some(parameters))
                    }
                }
                assert!(received.samples.iter().all(|sample| sample.is_finite()));
                energy += received
                    .samples
                    .iter()
                    .map(|sample| sample * sample)
                    .sum::<f64>();
                decoded_count += 1;
            }
            expected = parameters;
            previous = frame;
        }
        let expected_count = FRAMES - if codec == Codec::Vselp { 2 } else { 1 };
        assert_eq!(decoded_count, expected_count);
        assert!(energy > 1.0);
        eprintln!(
            "sample codec={} frames={} audio_seconds={} elapsed_ms={} realtime_ratio={:.2}",
            codec.label(),
            decoded_count,
            FRAMES as f64 * 0.02,
            started.elapsed().as_millis(),
            FRAMES as f64 * 0.02 / started.elapsed().as_secs_f64()
        );
    }
}
