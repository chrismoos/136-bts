use super::*;

fn physical_in(voice: &VoiceRuntime, call: usize, party: Party, interval: u64) -> u64 {
    interval * FULL_RATE_PERIOD
        + phase(
            voice.calls[call].legs[index(party)]
                .assignment
                .subscriber
                .assigned_slots,
        )
}
fn call_id(voice: &VoiceRuntime, call: usize) -> u64 {
    voice.calls[call].session.handle().id().value()
}
fn send_in(voice: &mut VoiceRuntime, call: usize, party: Party, interval: u64, now: u64) {
    let slot = physical_in(voice, call, party, interval);
    let (owner, found, burst) = voice.traffic(slot).unwrap().unwrap();
    assert_eq!(owner, call_id(voice, call));
    assert_eq!(found, party);
    let command = voice.traffic_submitted(owner, party, &burst).unwrap();
    voice
        .transmitted(
            Completion::Traffic {
                party,
                call_id: owner,
                command,
            },
            now,
        )
        .unwrap();
}
fn receive_in(voice: &mut VoiceRuntime, call: usize, party: Party, interval: u64, now: u64) {
    if voice.calls[call].legs[index(party)].initial_alignment
        == alignment::InitialAlignment::Shortened
    {
        let shortened_slot = physical_in(voice, call, party, interval.saturating_sub(1));
        let sync = SyncWord::ALL[(shortened_slot % FULL_RATE_PERIOD) as usize];
        let shortened =
            crate::phy::shortened::decode(&crate::phy::shortened::encode(sync, 1).unwrap())
                .unwrap();
        voice
            .receive_shortened(shortened_slot as f64 * 162.0 - 207.0, &shortened, now)
            .unwrap();
        let active = &mut voice.calls[call];
        let handle = active.legs[index(party)].assignment.handle.clone();
        let transmission = active.session.order(party).unwrap().clone();
        active
            .session
            .order_transmitted(&handle, &transmission, now)
            .unwrap();
        let TrafficMessage::PhysicalControl(control) = transmission.message() else {
            panic!("initial timing order expected");
        };
        active
            .session
            .receive(
                &handle,
                &TrafficMessage::PhysicalControlAck(control.clone()),
                now,
            )
            .unwrap();
    }
    let filler = TransmitFrame::facch(&crate::link::traffic::SignalingWord::filler(), 1);
    let data = TransmitFrame::interleave(&filler, &filler);
    let slot = physical_in(voice, call, party, interval);
    let mut decoded = fields(party, &data, 1);
    decoded.sync = SyncWord::ALL[(slot % FULL_RATE_PERIOD) as usize];
    voice
        .receive_traffic(slot as f64 * 162.0 - 207.0, 0, &decoded, None, now)
        .unwrap();
}
fn originate(voice: &mut VoiceRuntime, registry: &LocalRegistry, identity: u64, now: u64) {
    let mut request =
        crate::l3::access_call::OriginationRequest::decode(&origin().messages[0]).unwrap();
    request.message.called_party.address = AddressValue::Tbcd("5551234".into());
    voice
        .receive_access(registry, &set(identity, request.encode().unwrap()), now, 0)
        .unwrap();
    let phase = voice
        .allocation
        .phase(MobileIdentity::Min34(identity))
        .unwrap();
    while !voice.assignments.is_empty() {
        voice.control(0, phase).unwrap();
        let token = voice.control_completion();
        voice.control_submitted().unwrap();
        voice.transmitted(token, now + 10).unwrap();
    }
}

#[test]
fn two_audio_calls_share_the_carrier_and_end_independently() {
    let (mut voice, registry) = setup();
    originate(&mut voice, &registry, 1, 0);
    send_in(&mut voice, 0, Party::Caller, 0, 15);
    receive_in(&mut voice, 0, Party::Caller, 1, 20);
    originate(&mut voice, &registry, 2, 100);
    assert_eq!(voice.calls.len(), 2);
    send_in(&mut voice, 1, Party::Caller, 0, 115);
    receive_in(&mut voice, 1, Party::Caller, 1, 120);
    let (first, second) = (call_id(&voice, 0), call_id(&voice, 1));
    assert_ne!(first, second);
    assert_ne!(
        voice.calls[0].legs[0].assignment.subscriber.assigned_slots,
        voice.calls[1].legs[0].assignment.subscriber.assigned_slots
    );
    assert!(
        voice
            .calls
            .iter()
            .all(|call| call.session.state() == CallState::WaitingAnswer)
    );
    assert_eq!(voice.operation_calls(130).len(), 2);
    for interval in 1..5 {
        send_in(&mut voice, 0, Party::Caller, interval, 130);
        send_in(&mut voice, 1, Party::Caller, interval, 130);
    }

    voice.release_call(first, 200).unwrap();
    receive_in(&mut voice, 1, Party::Caller, 10, 5100);
    voice.advance(5250).unwrap();
    assert_eq!(voice.calls.len(), 1);
    assert_eq!(call_id(&voice, 0), second);
    assert!(voice.calls[0].session.media_ready());
    assert_eq!(voice.recent_calls().len(), 1);
    assert_eq!(voice.recent_calls()[0].id, first);
    assert_eq!(voice.operation_calls(5250).len(), 1);
    voice.shutdown(5300).unwrap();
}
