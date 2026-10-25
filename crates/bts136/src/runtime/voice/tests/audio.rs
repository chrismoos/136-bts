use super::*;
use crate::codec::speech::{Codec, Encoder, Parameters};

fn assign_audio(voice: &mut VoiceRuntime, registry: &LocalRegistry, codec: Codec, start_ms: u64) {
    let mut origin =
        crate::l3::access_call::OriginationRequest::decode(&origin().messages[0]).unwrap();
    origin.message.called_party.address = AddressValue::Tbcd("5551234".into());
    if codec == Codec::AcelpCc1 {
        origin
            .options
            .voices
            .push(crate::l3::access_call::LegacyVoice {
                coder: codec.coder(),
                privacy: 0,
            });
    }
    voice
        .receive_access(registry, &set(1, origin.encode().unwrap()), start_ms, 0)
        .unwrap();
    assert!(voice.page().is_none());
    assert_eq!(voice.paging.reservations().count(), 0);
    assert_eq!(voice.paging.audio_reservations().count(), 1);
    assert_eq!(voice.active().unwrap().legs.iter().count(), 1);
    let calls = voice.operation_calls(start_ms);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].legs[0].number, "1001");
    assert_eq!(calls[0].legs[1].number, "5551234");
    let phase = voice.allocation.phase(MobileIdentity::Min34(1)).unwrap();
    while !voice.assignments.is_empty() {
        voice.control(0, phase).unwrap();
        let token = voice.control_completion();
        voice.control_submitted().unwrap();
        voice.transmitted(token, start_ms + 10).unwrap();
    }
    send(voice, Party::Caller, 0, start_ms + 15);
    let filler = TransmitFrame::facch(&crate::link::traffic::SignalingWord::filler(), 1);
    receive(
        voice,
        Party::Caller,
        1,
        &TransmitFrame::interleave(&filler, &filler),
        start_ms + 20,
    );
    assert_eq!(
        voice.active().unwrap().session.state(),
        CallState::WaitingAnswer
    );
    assert!(
        matches!(voice.active().unwrap().sample_answer, sample::Answer::Ringing { started_ms } if started_ms == start_ms + 20)
    );
}

#[test]
fn unknown_number_plays_bundled_sample_in_negotiated_codec_and_releases_one_bearer() {
    for (codec, style) in [Codec::Vselp, Codec::AcelpCc1]
        .into_iter()
        .flat_map(|codec| {
            [
                crate::config::RingbackStyle::Us,
                crate::config::RingbackStyle::Eu,
            ]
            .map(|style| (codec, style))
        })
    {
        let (mut voice, registry) = setup();
        voice.ringback = style;
        assign_audio(&mut voice, &registry, codec, 0);
        let filler = TransmitFrame::facch(&crate::link::traffic::SignalingWord::filler(), 1);
        let data = TransmitFrame::interleave(&filler, &filler);
        let mut ringing_receiver = match codec {
            Codec::Vselp => {
                crate::link::voice::TrafficFrameReceiver::new(1, VselpParameters::default())
            }
            Codec::AcelpCc1 => crate::link::voice::TrafficFrameReceiver::acelp(1),
        };
        let mut ring_energy = 0.0;
        let mut reference = Encoder::new(codec);
        for interval in 1..250 {
            let now_ms = 20 + interval * 20;
            receive(&mut voice, Party::Caller, interval + 1, &data, now_ms);
            voice.advance(now_ms).unwrap();
            assert_eq!(
                voice.active().unwrap().session.state(),
                CallState::WaitingAnswer
            );
            assert!(matches!(
                voice.active().unwrap().legs[index(Party::Caller)].source,
                media::Source::Sample(None)
            ));
            reference
                .encode(&super::super::ringback::frame(voice.ringback, interval - 1))
                .unwrap();
            let slot = physical(&voice, Party::Caller, interval);
            let (_, _, burst) = voice.traffic(slot).unwrap().unwrap();
            let data = crate::phy::traffic::decode_forward(burst.bits())
                .unwrap()
                .fields
                .data;
            if let Some(frame) = ringing_receiver.push(Some(&data)).unwrap() {
                ring_energy += frame.samples.iter().map(|s| s * s).sum::<f64>();
            }
            send(&mut voice, Party::Caller, interval, now_ms);
        }
        assert!(ring_energy > 0.001);
        receive(&mut voice, Party::Caller, 250, &data, 5019);
        voice.advance(5019).unwrap();
        assert_eq!(
            voice.active().unwrap().session.state(),
            CallState::WaitingAnswer
        );
        voice.advance(5020).unwrap();
        assert!(voice.active().unwrap().session.media_ready());
        assert!(matches!(
            voice.active().unwrap().sample_answer,
            sample::Answer::Playing
        ));
        let source = crate::media::sample::SampleAudio::new(250);
        let mut receiver = match codec {
            Codec::Vselp => {
                crate::link::voice::TrafficFrameReceiver::new(250, VselpParameters::default())
            }
            Codec::AcelpCc1 => crate::link::voice::TrafficFrameReceiver::acelp(250),
        };
        let mut expected = None;
        let mut count = 0;
        for interval in 250..269 {
            let parameters = reference.encode(&source.frame(interval).unwrap()).unwrap();
            let slot = physical(&voice, Party::Caller, interval);
            let (_, party, burst) = voice.traffic(slot).unwrap().unwrap();
            assert_eq!(party, Party::Caller);
            assert!(
                voice
                    .traffic_submitted(voice.call_id().unwrap(), Party::Callee, &burst)
                    .is_err()
            );
            let (_, _, repeated) = voice.traffic(slot).unwrap().unwrap();
            assert_eq!(burst.bits(), repeated.bits());
            let data = crate::phy::traffic::decode_forward(burst.bits())
                .unwrap()
                .fields
                .data;
            let decoded = receiver.push(Some(&data)).unwrap();
            if let Some(expected) = expected {
                let decoded = decoded.unwrap();
                match expected {
                    Parameters::Vselp(parameters) => {
                        assert_eq!(decoded.parameters, Some(parameters))
                    }
                    Parameters::AcelpCc1(parameters) => {
                        assert_eq!(decoded.acelp_parameters, Some(parameters))
                    }
                }
                count += 1;
            }
            expected = parameters;
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
                    5020 + (interval - 250) * 20,
                )
                .unwrap();
        }
        assert!(count >= 16);
        let call_id = voice.call_id().unwrap();
        voice.release_call(call_id, 5500).unwrap();
        voice.shutdown(5501).unwrap();
        assert!(voice.active().is_none());
        assert_eq!(voice.paging.audio_reservations().count(), 0);
        assert!(!voice.subscriber_busy(MobileIdentity::Min34(1)));
        assign_audio(&mut voice, &registry, codec, 5600);
    }
}

#[test]
fn incoming_audio_pages_without_registration_rings_with_caller_id_and_connects_in_each_codec() {
    for (codec, use_sip) in [Codec::Vselp, Codec::AcelpCc1]
        .into_iter()
        .flat_map(|c| [(c, false), (c, true)])
    {
        let (mut voice, registry) = setup();
        let registry = LocalRegistry::try_new(
            registry
                .subscribers()
                .iter()
                .map(|subscriber| subscriber.provisioning.clone())
                .collect(),
        )
        .unwrap();
        let (gateway, commands, events) = crate::sip::test_gateway();
        let call_id = if use_sip {
            voice.set_sip_gateway(Some(gateway), true);
            events
                .send(crate::sip::Event::Inbound {
                    id: "incoming".into(),
                    called: "1001".into(),
                    caller: "2001".into(),
                })
                .unwrap();
            voice.advance_sip(&registry, 0, 0).unwrap();
            assert!(matches!(
                commands.try_recv().unwrap(),
                crate::sip::Command::Attach { .. }
            ));
            assert!(matches!(
                commands.try_recv().unwrap(),
                crate::sip::Command::Ringing(_)
            ));
            voice.page().unwrap().request.handle.id().value()
        } else {
            voice
                .begin_audio_call(&registry, MobileIdentity::Min34(1), "2001", 0)
                .unwrap()
        };
        let page =
            crate::l3::page::Page::decode(&voice.page().unwrap().transmission.bytes).unwrap();
        assert_eq!(
            page.calling_number.unwrap().address,
            AddressValue::Tbcd("2001".into())
        );
        complete_page(&mut voice, 0, 10);
        let mut response =
            crate::l3::access_call::PageResponseRequest::decode(&response().messages[0]).unwrap();
        if codec == Codec::AcelpCc1 {
            response
                .options
                .voices
                .push(crate::l3::access_call::LegacyVoice {
                    coder: codec.coder(),
                    privacy: 0,
                });
        }
        let response_bytes = if codec == Codec::AcelpCc1 {
            vec![0x08, 0x42, 0xcc, 0x50]
        } else {
            response.encode().unwrap()
        };
        voice
            .receive_access(&registry, &set(1, response_bytes), 20, 3)
            .unwrap();
        assert_eq!(voice.active().unwrap().legs.iter().count(), 1);
        while !voice.assignments.is_empty() {
            voice.control(0, 1).unwrap();
            let completion = voice.control_completion();
            voice.control_submitted().unwrap();
            voice.transmitted(completion, 30).unwrap();
        }
        send(&mut voice, Party::Callee, 1, 40);
        let filler = TransmitFrame::facch(&crate::link::traffic::SignalingWord::filler(), 1);
        receive(
            &mut voice,
            Party::Callee,
            1,
            &TransmitFrame::interleave(&filler, &filler),
            50,
        );
        assert_eq!(voice.active().unwrap().session.state(), CallState::Alerting);
        voice.advance(51).unwrap();
        let alert = voice
            .active()
            .unwrap()
            .session
            .order(Party::Callee)
            .unwrap()
            .message()
            .clone();
        let TrafficMessage::AlertWithInfo { information } = &alert else {
            panic!("Alert expected")
        };
        assert_eq!(
            information.calling_numbers,
            vec![crate::l3::legacy_traffic::CallingNumber::network("2001").unwrap()]
        );
        let words = crate::link::traffic::encode(&alert).unwrap().len() as u64;
        let extra = words - 1;
        for interval in 2..=2 + words {
            send(&mut voice, Party::Callee, interval, 60 + interval);
        }
        let mut mobile = TrafficTx::try_new(1, 16, 2 + extra).unwrap();
        mobile
            .enqueue(&TrafficMessage::Ack {
                ack_message_type: alert.message_type(),
                information: Default::default(),
            })
            .unwrap();
        for interval in [2 + extra, 3 + extra] {
            let slot = mobile.prepare(interval).unwrap().clone();
            receive(
                &mut voice,
                Party::Callee,
                interval,
                slot.data(),
                90 + interval,
            );
            mobile.transmitted(&slot).unwrap();
        }
        assert_eq!(
            voice.active().unwrap().session.state(),
            CallState::WaitingAnswer
        );
        if use_sip {
            voice.advance_sip(&registry, 110, 0).unwrap();
            assert!(commands.try_recv().is_err());
        }
        mobile.enqueue(&TrafficMessage::Connect).unwrap();
        for interval in [4 + extra, 5 + extra] {
            let slot = mobile.prepare(interval).unwrap().clone();
            receive(
                &mut voice,
                Party::Callee,
                interval,
                slot.data(),
                110 + interval,
            );
            mobile.transmitted(&slot).unwrap();
        }
        voice.advance(120).unwrap();
        for interval in [4 + extra, 5 + extra] {
            send(&mut voice, Party::Callee, interval, 125 + interval);
        }
        assert!(voice.active().unwrap().session.media_ready());
        if use_sip {
            voice.advance_sip(&registry, 140, 0).unwrap();
            assert!(matches!(
                commands.try_recv().unwrap(),
                crate::sip::Command::Answer(_)
            ));
        }
        assert_eq!(voice.call_id(), Some(call_id));
        let slot = physical(&voice, Party::Callee, 6 + extra);
        let (_, _, burst) = voice.traffic(slot).unwrap().unwrap();
        assert!(crate::phy::traffic::decode_forward(burst.bits()).is_ok());
        voice.shutdown(200).unwrap();
        assert_eq!(voice.paging.audio_reservations().count(), 0);
    }
}

#[test]
fn unprovisioned_origination_assigns_traffic_and_plays_sample_audio() {
    for codec in [Codec::Vselp, Codec::AcelpCc1] {
        let (mut voice, _) = setup();
        let registry = LocalRegistry::try_new(vec![]).unwrap();
        let core = crate::core::messaging::LocalMessaging::from_store(
            crate::core::store::StateStore::open(":memory:").unwrap(),
        )
        .unwrap();
        let mut sms = crate::runtime::sms::SmsRuntime::try_new(
            core,
            voice.allocation,
            "unprovisioned-caller".into(),
        )
        .unwrap();
        let mut request =
            crate::l3::access_call::OriginationRequest::decode(&origin().messages[0]).unwrap();
        request.message.called_party.address = AddressValue::Tbcd("2222".into());
        request
            .options
            .voices
            .push(crate::l3::access_call::LegacyVoice {
                coder: codec.coder(),
                privacy: 0,
            });
        voice
            .receive_access(&registry, &set(1, request.encode().unwrap()), 0, 0)
            .unwrap();
        voice.exchange_sms(&mut sms, 0, 0).unwrap();
        sms.advance(0).unwrap();
        assert!(voice.active().is_some());
        assert!(registry.subscribers().is_empty());
        let phase = voice.allocation.phase(MobileIdentity::Min34(1)).unwrap();
        while !voice.assignments.is_empty() {
            voice.control(0, phase).unwrap();
            let token = voice.control_completion();
            voice.control_submitted().unwrap();
            voice.transmitted(token, 10).unwrap();
        }
        send(&mut voice, Party::Caller, 0, 15);
        let filler = TransmitFrame::facch(&crate::link::traffic::SignalingWord::filler(), 1);
        receive(
            &mut voice,
            Party::Caller,
            1,
            &TransmitFrame::interleave(&filler, &filler),
            20,
        );
        assert_eq!(
            voice.active().unwrap().session.state(),
            CallState::WaitingAnswer
        );
        assert!(matches!(
            voice.active().unwrap().sample_answer,
            sample::Answer::Ringing { .. }
        ));
        voice.exchange_sms(&mut sms, 20, 20).unwrap();
        sms.advance(20).unwrap();
        let slot = physical(&voice, Party::Caller, 1);
        assert!(voice.traffic(slot).unwrap().is_some());
        voice.shutdown(100).unwrap();
        voice.exchange_sms(&mut sms, 100, 100).unwrap();
        assert!(sms.core().registry().subscribers().is_empty());
        assert!(voice.active().is_none());
    }
}

#[test]
fn cancelling_wav_call_during_ringing_does_not_answer_or_start_playback() {
    let (mut voice, registry) = setup();
    assign_audio(&mut voice, &registry, Codec::Vselp, 0);
    let call_id = voice.call_id().unwrap();
    voice.release_call(call_id, 100).unwrap();
    let active = voice.active().unwrap();
    assert!(!active.session.media_ready());
    assert!(matches!(
        active.legs[index(Party::Caller)].source,
        media::Source::Sample(None)
    ));
    voice.advance(5020).unwrap();
    assert!(
        voice
            .active()
            .is_none_or(|active| !active.session.media_ready()
                && matches!(active.sample_answer, sample::Answer::Stopped))
    );
    voice.shutdown(5021).unwrap();
}

#[test]
fn wav_answer_delay_does_not_override_the_answer_timeout() {
    let (mut voice, registry) = setup();
    voice.answer_ms = 100;
    assign_audio(&mut voice, &registry, Codec::Vselp, 0);
    voice.advance(120).unwrap();
    let active = voice.active().unwrap();
    assert!(matches!(
        active.session.state(),
        CallState::Releasing(EndReason::AnswerTimeout)
            | CallState::Stopped(EndReason::AnswerTimeout)
    ));
    assert!(matches!(active.sample_answer, sample::Answer::Stopped));
    assert!(matches!(
        active.legs[index(Party::Caller)].source,
        media::Source::Sample(None)
    ));
    voice.shutdown(121).unwrap();
}
