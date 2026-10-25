use super::*;
use crate::{
    codec::speech::{Codec, Encoder},
    l3::access_call::{LegacyVoice, OriginationRequest, PageResponseRequest},
};

pub(super) fn assign_codecs(
    voice: &mut VoiceRuntime,
    registry: &mut LocalRegistry,
    caller: Codec,
    callee: Codec,
    capabilities: bool,
) {
    let report = crate::l3::capability_report::decode(&[
        0x05, 0x45, 0x80, 0x02, 0xaf, 0x03, 0x1f, 0xe8, 0x10, 0x40,
    ])
    .unwrap();
    for (identity, codec) in [(1, caller), (2, callee)] {
        if codec == Codec::AcelpCc1 && capabilities {
            registry
                .record_capabilities(MobileIdentity::Min34(identity), report.clone(), 0)
                .unwrap();
        }
    }
    let mut request = OriginationRequest::decode(&origin().messages[0]).unwrap();
    if caller == Codec::AcelpCc1 {
        request.options.voices.push(LegacyVoice {
            coder: caller.coder(),
            privacy: 0,
        });
    }
    voice
        .receive_access(registry, &set(1, request.encode().unwrap()), 0, 0)
        .unwrap();
    let phase = voice.page().unwrap().phase;
    let page = crate::l3::page::Page::decode(&voice.page().unwrap().transmission.bytes).unwrap();
    assert_eq!(
        page.calling_number.unwrap().address,
        AddressValue::Tbcd("1001".into())
    );
    complete_page(voice, 0, 10);
    let mut reply = PageResponseRequest::decode(&response().messages[0]).unwrap();
    if callee == Codec::AcelpCc1 {
        reply.options.voices.push(LegacyVoice {
            coder: callee.coder(),
            privacy: 0,
        });
    }
    voice
        .receive_access(registry, &set(2, reply.encode().unwrap()), 20, 3)
        .unwrap();
    for _ in 0..8 {
        if voice.assignments.is_empty() {
            break;
        }
        assert!(voice.control(0, phase + 1).is_some());
        let completion = voice.control_completion();
        voice.control_submitted().unwrap();
        voice.transmitted(completion, 30).unwrap();
    }
    assert!(voice.assignments.is_empty());
    let active = voice.active().unwrap();
    for (party, codec) in [(Party::Caller, caller), (Party::Callee, callee)] {
        let leg = &active.legs[index(party)];
        assert_eq!(leg.assignment.subscriber.codec, codec);
        let designation = crate::l3::designation::decode_prefix(&leg.designation).unwrap();
        assert_eq!(
            designation.voice_mode,
            if codec == Codec::AcelpCc1 {
                Some(crate::l3::designation::VoiceMode {
                    coder: 2,
                    privacy: 0,
                })
            } else {
                None
            }
        );
        assert!(leg.transcoding);
    }
}
pub(super) fn connect(voice: &mut VoiceRuntime) -> (TrafficTx, u64) {
    for party in [Party::Caller, Party::Callee] {
        send(voice, party, 1, 40);
    }
    let filler = TransmitFrame::facch(&crate::link::traffic::SignalingWord::filler(), 1);
    let data = TransmitFrame::interleave(&filler, &filler);
    receive(voice, Party::Caller, 1, &data, 50);
    receive(voice, Party::Callee, 1, &data, 50);
    voice.advance(51).unwrap();
    let alert_words = crate::link::traffic::encode(
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
    let extra = alert_words - 1;
    for interval in 2..=2 + alert_words {
        for party in [Party::Caller, Party::Callee] {
            send(voice, party, interval, 60 + interval);
        }
    }
    let alert_type = TrafficMessage::AlertWithInfo {
        information: Default::default(),
    }
    .message_type();
    let mut mobile = TrafficTx::try_new(1, 16, 2 + extra).unwrap();
    mobile
        .enqueue(&TrafficMessage::Ack {
            ack_message_type: alert_type,
            information: Default::default(),
        })
        .unwrap();
    for interval in [2 + extra, 3 + extra] {
        let slot = mobile.prepare(interval).unwrap().clone();
        receive(voice, Party::Callee, interval, slot.data(), 90 + interval);
        mobile.transmitted(&slot).unwrap();
    }
    assert_eq!(
        voice.active().unwrap().session.state(),
        CallState::WaitingAnswer
    );
    mobile.enqueue(&TrafficMessage::Connect).unwrap();
    for interval in [4 + extra, 5 + extra] {
        let slot = mobile.prepare(interval).unwrap().clone();
        receive(voice, Party::Callee, interval, slot.data(), 110 + interval);
        mobile.transmitted(&slot).unwrap();
    }
    voice.advance(120).unwrap();
    for interval in [4 + extra, 5 + extra] {
        for party in [Party::Caller, Party::Callee] {
            send(voice, party, interval, 125 + interval);
        }
    }
    assert!(voice.active().unwrap().session.media_ready());
    (mobile, extra)
}
#[test]
fn acelp_and_mixed_codec_calls_negotiate_ring_connect_and_transcode_speech() {
    for (caller, callee, capabilities) in [
        (Codec::AcelpCc1, Codec::AcelpCc1, true),
        (Codec::AcelpCc1, Codec::AcelpCc1, false),
        (Codec::Vselp, Codec::AcelpCc1, true),
        (Codec::Vselp, Codec::AcelpCc1, false),
        (Codec::AcelpCc1, Codec::Vselp, true),
        (Codec::AcelpCc1, Codec::Vselp, false),
    ] {
        let (mut voice, mut registry) = setup();
        assign_codecs(&mut voice, &mut registry, caller, callee, capabilities);
        let (mut mobile, extra) = connect(&mut voice);
        let mut source = Encoder::new(callee);
        let mut receiver = if caller == Codec::AcelpCc1 {
            crate::link::voice::TrafficFrameReceiver::acelp(1)
        } else {
            crate::link::voice::TrafficFrameReceiver::new(1, VselpParameters::default())
        };
        let mut speech_frames = 0;
        let mut energy = 0.0;
        for frame in 0..16u64 {
            let interval = 6 + extra + frame;
            let samples = std::array::from_fn(|i| {
                ((frame * 160 + i as u64) as f64 * std::f64::consts::TAU * 440.0 / 8000.0).sin()
                    * 0.1
            });
            if let Some(parameters) = source.encode(&samples).unwrap() {
                mobile
                    .offer_media(interval, TransmitFrame::from_parameters(&parameters))
                    .unwrap();
            }
            let incoming = mobile.prepare(interval).unwrap().clone();
            receive(
                &mut voice,
                Party::Callee,
                interval,
                incoming.data(),
                140 + frame * 20,
            );
            mobile.transmitted(&incoming).unwrap();
            let slot = physical(&voice, Party::Caller, interval);
            let (_, party, burst) = voice.traffic(slot).unwrap().unwrap();
            assert_eq!(party, Party::Caller);
            let (_, repeated_party, repeated) = voice.traffic(slot).unwrap().unwrap();
            assert_eq!(repeated_party, party);
            assert_eq!(repeated.bits(), burst.bits());
            let data = crate::phy::traffic::decode_forward(burst.bits())
                .unwrap()
                .fields
                .data;
            if let Some(decoded) = receiver.push(Some(&data)).unwrap()
                && matches!(
                    decoded.kind,
                    FrameKind::Speech {
                        crc: CrcStatus::Valid,
                        ..
                    }
                )
            {
                speech_frames += 1;
                if caller == Codec::AcelpCc1 {
                    assert!(decoded.acelp_parameters.is_some());
                } else {
                    assert!(decoded.parameters.is_some());
                }
                energy += decoded.samples.iter().map(|s| s * s).sum::<f64>();
            }

            let command = voice
                .traffic_submitted(voice.call_id().unwrap(), party, &burst)
                .unwrap();
            voice
                .transmitted(
                    Completion::Traffic {
                        party,
                        call_id: voice.call_id().unwrap(),
                        command,
                    },
                    145 + frame * 20,
                )
                .unwrap();
            send(&mut voice, Party::Callee, interval, 146 + frame * 20);
        }
        assert!(
            speech_frames >= 10,
            "{caller:?} <- {callee:?}: {speech_frames}"
        );
        assert!(energy > 0.01, "{caller:?} <- {callee:?}: {energy}");
        assert_eq!(
            voice.active().unwrap().session.state(),
            CallState::Connected
        );
    }
}
