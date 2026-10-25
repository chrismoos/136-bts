use super::*;
use crate::{codec::speech::Codec, config::RingbackStyle};

fn alerting(voice: &mut VoiceRuntime) {
    for party in [Party::Caller, Party::Callee] {
        send(voice, party, 1, 40);
    }
    let filler = TransmitFrame::facch(&crate::link::traffic::SignalingWord::filler(), 1);
    let data = TransmitFrame::interleave(&filler, &filler);
    receive(voice, Party::Caller, 1, &data, 50);
    receive(voice, Party::Callee, 1, &data, 50);
    voice.advance(51).unwrap();
    assert_eq!(voice.active().unwrap().session.state(), CallState::Alerting);
}

fn forward(
    voice: &mut VoiceRuntime,
    party: Party,
    interval: u64,
    receiver: &mut crate::link::voice::TrafficFrameReceiver,
) -> f64 {
    let slot = physical(voice, party, interval);
    let (_, _, burst) = voice.traffic(slot).unwrap().unwrap();
    let data = crate::phy::traffic::decode_forward(burst.bits())
        .unwrap()
        .fields
        .data;
    let energy = receiver.push(Some(&data)).unwrap().map_or(0.0, |frame| {
        frame.samples.iter().map(|sample| sample * sample).sum()
    });
    send(voice, party, interval, 60 + interval);
    energy
}

#[test]
fn local_call_ringback_is_audible_only_to_the_caller_in_each_codec_and_style() {
    const LAST_INTERVAL: u64 = 24;
    const MINIMUM_ENERGY: f64 = 0.001;
    for style in [RingbackStyle::Us, RingbackStyle::Eu] {
        for codec in [Codec::Vselp, Codec::AcelpCc1] {
            let (mut voice, mut registry) = setup();
            voice.ringback = style;
            if codec == Codec::Vselp {
                assigned(&mut voice, &registry);
            } else {
                super::acelp::assign_codecs(&mut voice, &mut registry, codec, codec, true);
            }
            alerting(&mut voice);
            let mut caller = match codec {
                Codec::Vselp => {
                    crate::link::voice::TrafficFrameReceiver::new(1, VselpParameters::default())
                }
                Codec::AcelpCc1 => crate::link::voice::TrafficFrameReceiver::acelp(1),
            };
            let mut energy = 0.0;
            for interval in 2..=LAST_INTERVAL {
                energy += forward(&mut voice, Party::Caller, interval, &mut caller);
                send(&mut voice, Party::Callee, interval, 60 + interval);
            }
            assert!(energy > MINIMUM_ENERGY, "{codec:?}, {style:?}: {energy}");
            assert!(
                voice.active().unwrap().legs[index(Party::Caller)]
                    .ringback_start
                    .is_some()
            );
            assert!(
                voice.active().unwrap().legs[index(Party::Callee)]
                    .ringback_start
                    .is_none()
            );
            voice.shutdown(100).unwrap();
        }
    }
}

#[test]
fn answering_stops_ringback_and_enables_normal_speech() {
    for codec in [Codec::Vselp, Codec::AcelpCc1] {
        let (mut voice, mut registry) = setup();
        super::acelp::assign_codecs(&mut voice, &mut registry, codec, Codec::AcelpCc1, true);
        super::acelp::connect(&mut voice);
        assert!(voice.active().unwrap().session.media_ready());
        assert!(
            voice.active().unwrap().legs[index(Party::Caller)]
                .ringback_start
                .is_none()
        );
        voice.shutdown(200).unwrap();
    }
}

#[test]
fn release_and_answer_timeout_stop_local_ringback() {
    for timeout in [false, true] {
        let (mut voice, registry) = setup();
        const ANSWER_TIMEOUT_MS: u64 = 100;
        const AFTER_TIMEOUT_MS: u64 = 250;
        voice.answer_ms = ANSWER_TIMEOUT_MS;
        assigned(&mut voice, &registry);
        alerting(&mut voice);
        send(&mut voice, Party::Caller, 2, 62);
        assert!(
            voice.active().unwrap().legs[index(Party::Caller)]
                .ringback_start
                .is_some()
        );
        if timeout {
            let words = crate::link::traffic::encode(
                voice
                    .active()
                    .unwrap()
                    .session
                    .order(Party::Callee)
                    .unwrap()
                    .message(),
            )
            .unwrap()
            .len() as u64;
            for interval in 2..=2 + words {
                send(&mut voice, Party::Callee, interval, 60 + interval);
            }
            let state = voice
                .active_mut()
                .unwrap()
                .session
                .advance(AFTER_TIMEOUT_MS)
                .unwrap();
            assert_eq!(state, CallState::Releasing(EndReason::AnswerTimeout));
        } else {
            voice.release_call(voice.call_id().unwrap(), 70).unwrap();
        }
        if voice.active().is_some() {
            let state = voice.active().unwrap().session.state();
            assert!(!matches!(
                state,
                CallState::Alerting | CallState::WaitingAnswer | CallState::Connected
            ));
            let active = voice.active_mut().unwrap();
            let leg = &mut active.legs[index(Party::Caller)];
            super::super::media::offer(
                leg,
                &mut dtmf::Sender::default(),
                3,
                state,
                RingbackStyle::Us,
            )
            .unwrap();
            assert!(leg.ringback_start.is_none());
        }
        voice.shutdown(AFTER_TIMEOUT_MS + 1).unwrap();
    }
}
