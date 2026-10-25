use bts136::{
    codec::vselp::{FRAME_SAMPLES, SUBFRAMES, VselpParameters, VselpSubframe, decoder::Decoder},
    l3::legacy_traffic::TrafficMessage,
    link::{
        traffic::{self as traffic_link, SignalingWord, TrafficMessageReceiver},
        voice::{FrameKind, TrafficFrameReceiver, TransmitFrame},
    },
    phy::{
        acquisition::decode_forward_traffic,
        forward::ForwardModulator,
        speech::CrcStatus,
        sync::SyncWord,
        traffic::{self, SACCH_BITS},
    },
};

const DVCC: u8 = 10;
const SAMPLES_PER_SYMBOL: usize = 8;
#[derive(Clone)]
enum Frame {
    Speech(VselpParameters),
    Facch(SignalingWord),
}
impl Frame {
    fn transmit(&self) -> TransmitFrame {
        match self {
            Self::Speech(frame) => TransmitFrame::speech(frame),
            Self::Facch(word) => TransmitFrame::facch(word, DVCC),
        }
    }
}
fn parameters(r0: u8, code: u8) -> VselpParameters {
    VselpParameters::try_new(
        r0,
        [32, 16, 16, 8, 8, 4, 4, 4, 4, 2],
        [VselpSubframe::try_new(21, code, code ^ 0x7f, 170).unwrap(); SUBFRAMES],
    )
    .unwrap()
}
#[test]
fn mixed_speech_and_facch_forward_iq_recovers_signaling_and_one_audio_frame_per_interval() {
    let seed = parameters(20, 37);
    let connect = TrafficMessage::Connect;
    let release = TrafficMessage::Release {
        information: bts136::l3::legacy_traffic::ReleaseInformation {
            unparsed_bits: ([1, 2, 3, 4, 5])
                .iter()
                .flat_map(|byte| {
                    (0..u8::BITS)
                        .rev()
                        .map(move |shift| byte & (1 << shift) != 0)
                })
                .collect(),
            ..Default::default()
        },
    };
    let mut frames = vec![Frame::Speech(seed.clone())];
    frames.extend(
        traffic_link::encode(&connect)
            .unwrap()
            .into_iter()
            .map(Frame::Facch),
    );
    frames.push(Frame::Speech(parameters(21, 54)));
    frames.extend(
        traffic_link::encode(&release)
            .unwrap()
            .into_iter()
            .map(Frame::Facch),
    );
    frames.push(Frame::Speech(parameters(19, 23)));
    frames.push(Frame::Speech(parameters(19, 23)));
    let padding = TransmitFrame::speech(&VselpParameters::default());
    let mut previous = padding.clone();
    let mut modulator = ForwardModulator::try_new(SAMPLES_PER_SYMBOL).unwrap();
    let mut samples = Vec::new();
    for current in frames
        .iter()
        .map(Frame::transmit)
        .chain(std::iter::once(padding))
    {
        let data = TransmitFrame::interleave(&previous, &current);
        let burst =
            traffic::encode_forward(SyncWord::One, &[false; SACCH_BITS], &data, DVCC, 0).unwrap();
        samples.extend(modulator.encode_slot(&burst));
        previous = current;
    }
    samples.extend(modulator.finish());
    let bursts = decode_forward_traffic(&samples, modulator.sample_rate_hz()).unwrap();
    assert_eq!(bursts.len(), frames.len() + 1);
    let mut receiver = TrafficFrameReceiver::new(DVCC, seed.clone());
    let mut expected_decoder = Decoder::new(seed);
    let mut signaling = TrafficMessageReceiver::default();
    let mut messages = Vec::new();
    let mut audio = Vec::new();
    assert_eq!(
        receiver.push(Some(&bursts[0].fields.fields.data)).unwrap(),
        None
    );
    for (burst, expected) in bursts[1..].iter().zip(&frames) {
        let frame = receiver
            .push(Some(&burst.fields.fields.data))
            .unwrap()
            .unwrap();
        let expected_pcm = match expected {
            Frame::Speech(parameters) => {
                assert_eq!(
                    frame.kind,
                    FrameKind::Speech {
                        crc: CrcStatus::Valid,
                        corrected_coded_bits: 0,
                        coded_bits: bts136::phy::speech::PROTECTED_BITS as u16,
                    }
                );
                expected_decoder.decode(parameters).unwrap()
            }
            Frame::Facch(expected) => {
                let FrameKind::Facch(word) = frame.kind else {
                    panic!("FACCH classified as speech");
                };
                assert_eq!(word.message, expected.contents);
                assert_eq!(word.continuation, expected.continuation);
                assert_eq!(word.corrected_coded_bits, 0);
                if let Some(message) = signaling.push(&SignalingWord::from(&word)).unwrap() {
                    messages.push(message);
                }
                expected_decoder.conceal_missing_frame().unwrap()
            }
        };
        assert_eq!(frame.samples, expected_pcm);
        audio.extend(frame.samples);
    }
    assert_eq!(messages, [connect, release]);
    assert_eq!(audio.len(), frames.len() * FRAME_SAMPLES);
}
#[test]
fn one_lost_slot_conceals_both_neighboring_speech_frames_and_preserves_recovery_timing() {
    let seed = parameters(20, 37);
    let speech = TransmitFrame::speech(&seed);
    let slot = TransmitFrame::interleave(&speech, &speech);
    let mut receiver = TrafficFrameReceiver::new(DVCC, seed.clone());
    let mut expected = Decoder::new(seed.clone());
    assert_eq!(receiver.push(Some(&slot)).unwrap(), None);
    for next in [None, Some(&slot)] {
        let output = receiver.push(next).unwrap().unwrap();
        assert_eq!(output.kind, FrameKind::Missing);
        assert_eq!(output.samples, expected.conceal_missing_frame().unwrap());
    }
    let recovered = receiver.push(Some(&slot)).unwrap().unwrap();
    assert_eq!(
        recovered.kind,
        FrameKind::Speech {
            crc: CrcStatus::Valid,
            corrected_coded_bits: 0,
            coded_bits: bts136::phy::speech::PROTECTED_BITS as u16,
        }
    );
    assert_eq!(recovered.samples, expected.decode(&seed).unwrap());
}
