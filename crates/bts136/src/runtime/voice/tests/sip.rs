use super::*;
use crate::sip::{Command, Event, test_gateway};
fn outgoing(voice: &mut VoiceRuntime, registry: &LocalRegistry, number: &str) {
    let mut request =
        crate::l3::access_call::OriginationRequest::decode(&origin().messages[0]).unwrap();
    request.message.called_party.address = AddressValue::Tbcd(number.into());
    voice
        .receive_access(registry, &set(1, request.encode().unwrap()), 0, 0)
        .unwrap();
}
fn confirm(voice: &mut VoiceRuntime) {
    let phase = voice.allocation.phase(MobileIdentity::Min34(1)).unwrap();
    while !voice.assignments.is_empty() {
        voice.control(0, phase).unwrap();
        let token = voice.control_completion();
        voice.control_submitted().unwrap();
        voice.transmitted(token, 10).unwrap();
    }
    send(voice, Party::Caller, 0, 15);
    let filler = TransmitFrame::facch(&crate::link::traffic::SignalingWord::filler(), 1);
    receive(
        voice,
        Party::Caller,
        1,
        &TransmitFrame::interleave(&filler, &filler),
        20,
    );
}
#[test]
fn outgoing_sip_waits_for_answer_accepts_early_media_and_releases_gateway() {
    let (mut voice, registry) = setup();
    let (gateway, commands, events) = test_gateway();
    voice.set_sip_gateway(Some(gateway), true);
    outgoing(&mut voice, &registry, "5551212");
    let Command::Dial {
        id,
        called,
        caller,
        audio,
    } = commands.try_recv().unwrap()
    else {
        panic!("SIP dial expected")
    };
    assert_eq!(called, "5551212");
    assert_eq!(caller, "1001");
    confirm(&mut voice);
    assert_eq!(
        voice.active().unwrap().session.state(),
        CallState::WaitingAnswer
    );
    assert!(!voice.active().unwrap().session.media_ready());
    events
        .send(Event::Ringing {
            id: id.clone(),
            early: true,
        })
        .unwrap();
    voice.advance_sip(&registry, 21, 0).unwrap();
    assert!(matches!(
        voice.active().unwrap().legs[0].source,
        media::Source::Sip {
            early_media: true,
            ..
        }
    ));
    audio.downlink.force_push([1000; crate::sip::PCM_SAMPLES]);
    assert!(!voice.active().unwrap().session.media_ready());
    events.send(Event::Answered(id.clone())).unwrap();
    voice.advance_sip(&registry, 22, 0).unwrap();
    assert!(voice.active().unwrap().session.media_ready());
    voice.shutdown(100).unwrap();
    voice.advance_sip(&registry, 100, 0).unwrap();
    assert!(matches!(commands.try_recv().unwrap(),Command::Release{id:ended,..}if ended==id));
}
#[test]
fn local_numbers_and_sample_number_do_not_use_sip() {
    for number in ["1002", "2222"] {
        let (mut voice, registry) = setup();
        let (gateway, commands, _) = test_gateway();
        voice.set_sip_gateway(Some(gateway), true);
        outgoing(&mut voice, &registry, number);
        assert!(commands.try_recv().is_err());
        voice.shutdown(100).unwrap();
    }
}
#[test]
fn inbound_sip_pages_offline_subscriber_and_remote_cancel_stops_paging() {
    let (mut voice, registry) = setup();
    let offline = LocalRegistry::try_new(
        registry
            .subscribers()
            .iter()
            .map(|s| s.provisioning.clone())
            .collect(),
    )
    .unwrap();
    let (gateway, commands, events) = test_gateway();
    voice.set_sip_gateway(Some(gateway), true);
    events
        .send(Event::Inbound {
            id: "incoming".into(),
            called: "1002".into(),
            caller: "+18005551234".into(),
        })
        .unwrap();
    voice.advance_sip(&offline, 0, 0).unwrap();
    assert!(voice.page().is_some());
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::Attach { .. }
    ));
    assert!(matches!(commands.try_recv().unwrap(), Command::Ringing(_)));
    events
        .send(Event::Ended {
            id: "incoming".into(),
            status: 487,
        })
        .unwrap();
    voice.advance_sip(&offline, 1, 0).unwrap();
    assert!(voice.page().is_none());
    assert_eq!(voice.paging.audio_reservations().count(), 0);
}
#[test]
fn inbound_sip_unknown_and_busy_destinations_are_rejected_without_cell_failure() {
    let (mut voice, registry) = setup();
    let (gateway, commands, events) = test_gateway();
    voice.set_sip_gateway(Some(gateway), true);
    events
        .send(Event::Inbound {
            id: "unknown".into(),
            called: "9999".into(),
            caller: String::new(),
        })
        .unwrap();
    voice.advance_sip(&registry, 0, 0).unwrap();
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::Release { status: 404, .. }
    ));
    outgoing(&mut voice, &registry, "2222");
    events
        .send(Event::Inbound {
            id: "busy".into(),
            called: "1001".into(),
            caller: "2001".into(),
        })
        .unwrap();
    voice.advance_sip(&registry, 1, 0).unwrap();
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::Release { status: 486, .. }
    ));
    assert!(voice.active().is_some());
    voice.shutdown(100).unwrap();
}
#[test]
fn inbound_sip_pages_a_second_phone_while_another_call_holds_a_traffic_pair() {
    let (mut voice, registry) = setup();
    let (gateway, commands, events) = test_gateway();
    voice.set_sip_gateway(Some(gateway), true);
    outgoing(&mut voice, &registry, "2222");
    assert_eq!(voice.calls.len(), 1);
    events
        .send(Event::Inbound {
            id: "second".into(),
            called: "1002".into(),
            caller: "2001".into(),
        })
        .unwrap();
    voice.advance_sip(&registry, 1, 0).unwrap();
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::Attach { .. }
    ));
    assert!(matches!(commands.try_recv().unwrap(), Command::Ringing(_)));
    assert_eq!(voice.pages.len(), 1);
    assert_eq!(voice.sip_calls.len(), 1);
    assert_eq!(voice.operation_calls(1).len(), 2);
    events
        .send(Event::Inbound {
            id: "third".into(),
            called: "1002".into(),
            caller: "2002".into(),
        })
        .unwrap();
    voice.advance_sip(&registry, 2, 0).unwrap();
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::Release { status: 486, .. }
    ));
    voice.shutdown(100).unwrap();
}
#[test]
fn gateway_failure_releases_call_without_returning_cell_failure() {
    let (mut voice, registry) = setup();
    let (gateway, commands, events) = test_gateway();
    voice.set_sip_gateway(Some(gateway), true);
    outgoing(&mut voice, &registry, "5551212");
    let _ = commands.try_recv().unwrap();
    confirm(&mut voice);
    events
        .send(Event::Failed("gateway connection lost".into()))
        .unwrap();
    voice.advance_sip(&registry, 21, 0).unwrap();
    assert!(matches!(
        voice.active().unwrap().session.state(),
        CallState::Releasing(_)
    ));
    assert!(voice.gateway.is_none());
    voice.shutdown(100).unwrap();
}

#[test]
fn unprovisioned_originator_uses_decoded_min_for_sip_caller_id() {
    let (mut voice, registry) = setup();
    let mut request = origin();
    request.identity = MobileIdentity::Min34(3410151054);
    let mut decoded =
        crate::l3::access_call::OriginationRequest::decode(&request.messages[0]).unwrap();
    decoded.message.called_party.address = AddressValue::Tbcd("5551212".into());
    request.messages[0] = decoded.encode().unwrap();
    let (gateway, commands, _) = test_gateway();
    voice.set_sip_gateway(Some(gateway), true);
    voice.receive_access(&registry, &request, 0, 0).unwrap();
    let Command::Dial { caller, .. } = commands.try_recv().unwrap() else {
        panic!("SIP dial expected")
    };
    assert_eq!(caller, "3143781765");
    voice.shutdown(100).unwrap();
}

#[test]
fn sip_ringback_can_be_muted_without_muting_remote_early_media() {
    const LAST_INTERVAL: u64 = 20;
    #[derive(Clone, Copy)]
    enum Progress {
        Waiting,
        Ringing,
        EarlyMedia,
    }
    fn frames(
        generate_ringback: bool,
        progress: Progress,
    ) -> Vec<[bool; crate::phy::traffic::SLOT_BITS]> {
        let (mut voice, registry) = setup();
        let (gateway, commands, events) = test_gateway();
        voice.set_sip_gateway(Some(gateway), generate_ringback);
        outgoing(&mut voice, &registry, "5551212");
        let Command::Dial { id, audio, .. } = commands.try_recv().unwrap() else {
            panic!("SIP dial expected")
        };
        confirm(&mut voice);
        if !matches!(progress, Progress::Waiting) {
            events
                .send(Event::Ringing {
                    id: id.clone(),
                    early: matches!(progress, Progress::EarlyMedia),
                })
                .unwrap();
            voice.advance_sip(&registry, 21, 0).unwrap();
        }
        let mut frames = Vec::new();
        for interval in 1..=LAST_INTERVAL {
            if matches!(progress, Progress::EarlyMedia) {
                audio
                    .downlink
                    .force_push([i16::MAX / 8; crate::sip::PCM_SAMPLES]);
            }
            let slot = physical(&voice, Party::Caller, interval);
            let (_, _, burst) = voice.traffic(slot).unwrap().unwrap();
            frames.push(*burst.bits());
            send(&mut voice, Party::Caller, interval, 30 + interval);
        }
        events.send(Event::Answered(id)).unwrap();
        voice.advance_sip(&registry, 100, 0).unwrap();
        send(&mut voice, Party::Caller, LAST_INTERVAL + 1, 101);
        assert!(voice.active().unwrap().legs[0].ringback_start.is_none());
        voice.shutdown(200).unwrap();
        frames
    }
    let silent = frames(false, Progress::Waiting);
    assert_eq!(frames(false, Progress::Ringing), silent);
    assert_ne!(frames(true, Progress::Ringing), silent);
    let early_media = frames(false, Progress::EarlyMedia);
    assert_ne!(early_media, silent);
    assert_eq!(early_media, frames(true, Progress::EarlyMedia));
}
