mod acelp;
mod audio;
mod auxiliary;
mod concurrent;
mod paging;
mod ringback;
mod sip;
mod termination;
use super::*;
use crate::{
    config::Config,
    core::call_session::CallState,
    l3::{
        address::{AddressInfo, AddressValue},
        legacy_traffic::TrafficMessage,
        origination::Origination,
        page_response::{PageResponse, ServiceCode},
        registration::{AccessMessage, RegistrationType, RejectCause},
    },
    link::{rach::MobileIdentity, traffic_tx::TrafficTx},
    phy::traffic::{DATA_BITS, SACCH_BITS},
};
fn set(identity: u64, message: Vec<u8>) -> MessageSet {
    MessageSet {
        identity: MobileIdentity::Min34(identity),
        messages: vec![message],
    }
}
fn setup() -> (VoiceRuntime, LocalRegistry) {
    let config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) =
        super::super::serving::messages(&config)
            .into_iter()
            .next()
            .unwrap()
    else {
        panic!()
    };
    let mut state = config.prepare().unwrap();
    for identity in [1, 2] {
        state
            .registry
            .register(
                &set(
                    identity,
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ),
                0,
            )
            .unwrap();
    }
    let voice = VoiceRuntime::try_new(
        state.traffic,
        PagingAllocation::try_new(&structure).unwrap(),
        1,
        config.cell.traffic_power_level,
        &config.calls,
    )
    .unwrap();
    (voice, state.registry)
}
fn origin() -> MessageSet {
    set(
        1,
        Origination {
            protocol_version: ProtocolVersion::Is136RevA,
            emergency_call: false,
            called_party: AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("1002".into()),
            },
            last_try: false,
            scm: 15,
            service_code: ServiceCode::DigitalSpeech,
        }
        .encode()
        .unwrap(),
    )
}
fn response() -> MessageSet {
    set(
        2,
        PageResponse {
            protocol_version: ProtocolVersion::Is136RevA,
            last_try: false,
            scm: 15,
            service_code: ServiceCode::DigitalSpeech,
        }
        .encode()
        .unwrap(),
    )
}
fn fields(party: Party, data: &[bool; DATA_BITS], dvcc: u8) -> TrafficFields {
    TrafficFields {
        sync: match party {
            Party::Caller => SyncWord::Two,
            Party::Callee => SyncWord::Three,
        },
        sacch: [false; SACCH_BITS],
        data: *data,
        color: crate::phy::color::DecodedDvcc {
            dvcc,
            corrected_bits: 0,
        },
    }
}
fn physical(voice: &VoiceRuntime, party: Party, interval: u64) -> u64 {
    interval * 3
        + phase(
            voice.active().unwrap().legs[index(party)]
                .assignment
                .subscriber
                .assigned_slots,
        )
}
fn send(voice: &mut VoiceRuntime, party: Party, interval: u64, now: u64) {
    let slot = physical(voice, party, interval);
    let (_, found, burst) = voice.traffic(slot).unwrap().unwrap();
    assert_eq!(found, party);
    let call_id = voice.call_id().unwrap();
    let command = voice
        .traffic_submitted(voice.call_id().unwrap(), party, &burst)
        .unwrap();
    voice
        .transmitted(
            Completion::Traffic {
                party,
                call_id,
                command,
            },
            now,
        )
        .unwrap();
}
fn submit_page(voice: &mut VoiceRuntime, superframe: u64) -> Vec<Completion> {
    let mut opportunity = (superframe, voice.page().unwrap().phase);
    let mut completions = Vec::new();
    loop {
        voice.control(opportunity.0, opportunity.1).unwrap();
        completions.push(voice.control_completion());
        voice.control_submitted().unwrap();
        if !voice.control_in_progress() {
            break;
        }
        opportunity = voice
            .allocation
            .continuation(opportunity.0, opportunity.1)
            .unwrap();
    }
    completions
}
fn complete_page(voice: &mut VoiceRuntime, superframe: u64, now_ms: u64) {
    for completion in submit_page(voice, superframe) {
        voice.transmitted(completion, now_ms).unwrap();
    }
}
fn assigned(voice: &mut VoiceRuntime, registry: &LocalRegistry) {
    voice.receive_access(registry, &origin(), 0, 0).unwrap();
    let phase = voice.page().unwrap().phase;
    let completions = submit_page(voice, 0);
    assert!(completions.len() > 1);
    assert!(voice.receive_access(registry, &response(), 1, 3).is_err());
    for completion in completions {
        voice.transmitted(completion, 10).unwrap();
    }
    voice.receive_access(registry, &response(), 20, 3).unwrap();
    for _ in 0..2 {
        assert!(voice.control(0, phase + 1).is_some());
        let token = voice.control_completion();
        voice.control_submitted().unwrap();
        voice.transmitted(token, 30).unwrap();
    }
    assert_eq!(
        voice.active().unwrap().session.state(),
        CallState::Establishing
    );
}
fn receive(
    voice: &mut VoiceRuntime,
    party: Party,
    interval: u64,
    data: &[bool; DATA_BITS],
    now: u64,
) {
    if voice.active().unwrap().legs[index(party)].initial_alignment
        == alignment::InitialAlignment::Shortened
    {
        let shortened_slot = physical(voice, party, interval.saturating_sub(1));
        let sync = SyncWord::ALL[phase(
            voice.active().unwrap().legs[index(party)]
                .assignment
                .subscriber
                .assigned_slots,
        ) as usize];
        let shortened =
            crate::phy::shortened::decode(&crate::phy::shortened::encode(sync, 1).unwrap())
                .unwrap();
        voice
            .receive_shortened(shortened_slot as f64 * 162.0 - 207.0, &shortened, now)
            .unwrap();
        let active = voice.active_mut().unwrap();
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
    let slot = physical(voice, party, interval);
    let mut decoded = fields(party, data, 1);
    decoded.sync = SyncWord::ALL[(slot % FULL_RATE_PERIOD) as usize];
    voice
        .receive_traffic(slot as f64 * 162.0 - 207.0, 0, &decoded, None, now)
        .unwrap();
}
#[test]
fn assignment_requires_valid_reverse_dvcc_and_paging_repeat_survives_page_response() {
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    let silent = TransmitFrame::speech(&VselpParameters::default());
    let data = TransmitFrame::interleave(&silent, &silent);
    let slot = physical(&voice, Party::Caller, 1);
    voice
        .receive_traffic(
            slot as f64 * 162.0 - 207.0,
            0,
            &fields(Party::Caller, &data, 2),
            None,
            40,
        )
        .unwrap();
    assert_eq!(
        voice.active().unwrap().session.bearer_state(Party::Caller),
        BearerState::Assigning
    );
    receive(&mut voice, Party::Caller, 1, &data, 50);
    receive(&mut voice, Party::Callee, 1, &data, 50);
    assert_eq!(voice.active().unwrap().session.state(), CallState::Alerting);
    assert!(!voice.active().unwrap().session.media_ready());
    let repeats: Vec<_> = voice
        .repeats
        .iter()
        .map(|repeat| (repeat.superframe, repeat.phase, repeat.information))
        .collect();
    assert!(repeats.len() > 1);
    for (superframe, phase, information) in repeats {
        assert!(voice.has_repeat(superframe, phase));
        assert_eq!(voice.control(superframe, phase), Some(&information));
        voice.control_submitted().unwrap();
    }
    assert!(voice.repeats.is_empty());
}
#[test]
fn call_alert_connect_voice_and_release_follow_coded_traffic_and_air_receipts() {
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    for party in [Party::Caller, Party::Callee] {
        send(&mut voice, party, 1, 40);
    }
    let silent = TransmitFrame::speech(&VselpParameters::default());
    let data = TransmitFrame::interleave(&silent, &silent);
    receive(&mut voice, Party::Caller, 1, &data, 50);
    receive(&mut voice, Party::Callee, 1, &data, 50);
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
    assert!(alert_words > 1);
    let extra_intervals = alert_words - 1;
    for interval in 2..=2 + alert_words {
        for party in [Party::Caller, Party::Callee] {
            send(&mut voice, party, interval, 60 + interval);
        }
    }
    let alert_type = TrafficMessage::AlertWithInfo {
        information: Default::default(),
    }
    .message_type();
    let mut mobile = TrafficTx::try_new(1, 16, 2 + extra_intervals).unwrap();
    mobile
        .enqueue(&TrafficMessage::Ack {
            ack_message_type: alert_type,
            information: Default::default(),
        })
        .unwrap();
    for interval in [2, 3].map(|interval| interval + extra_intervals) {
        let prepared = mobile.prepare(interval).unwrap().clone();
        receive(
            &mut voice,
            Party::Callee,
            interval,
            prepared.data(),
            90 + interval,
        );
        mobile.transmitted(&prepared).unwrap();
    }
    assert_eq!(
        voice.active().unwrap().session.state(),
        CallState::WaitingAnswer
    );
    mobile.enqueue(&TrafficMessage::Connect).unwrap();
    for interval in [4, 5].map(|interval| interval + extra_intervals) {
        let prepared = mobile.prepare(interval).unwrap().clone();
        receive(
            &mut voice,
            Party::Callee,
            interval,
            prepared.data(),
            110 + interval,
        );
        mobile.transmitted(&prepared).unwrap();
    }
    assert_eq!(
        voice.active().unwrap().session.state(),
        CallState::Connecting
    );
    assert!(!voice.active().unwrap().session.media_ready());
    voice.advance(120).unwrap();
    for interval in [4, 5].map(|interval| interval + extra_intervals) {
        for party in [Party::Caller, Party::Callee] {
            send(&mut voice, party, interval, 125 + interval);
        }
    }
    assert!(voice.active().unwrap().session.media_ready());
    let parameters = VselpParameters::try_new(
        14,
        [1; 10],
        [crate::codec::vselp::VselpSubframe::try_new(20, 31, 42, 105).unwrap(); 4],
    )
    .unwrap();
    mobile
        .offer_media(6 + extra_intervals, TransmitFrame::speech(&parameters))
        .unwrap();
    let prepared = mobile.prepare(6 + extra_intervals).unwrap().clone();
    receive(
        &mut voice,
        Party::Callee,
        6 + extra_intervals,
        prepared.data(),
        140,
    );
    mobile.transmitted(&prepared).unwrap();
    mobile
        .offer_media(7 + extra_intervals, TransmitFrame::speech(&parameters))
        .unwrap();
    let prepared = mobile.prepare(7 + extra_intervals).unwrap().clone();
    receive(
        &mut voice,
        Party::Callee,
        7 + extra_intervals,
        prepared.data(),
        160,
    );
    mobile.transmitted(&prepared).unwrap();
    assert!(voice.active().unwrap().legs[0].media.is_some());
    let (_, _, first) = voice
        .traffic(physical(&voice, Party::Caller, 6 + extra_intervals))
        .unwrap()
        .unwrap();
    let first_data = crate::phy::traffic::decode_forward(first.bits())
        .unwrap()
        .fields
        .data;
    let call_id = voice.call_id().unwrap();
    let command = voice
        .traffic_submitted(voice.call_id().unwrap(), Party::Caller, &first)
        .unwrap();
    voice
        .transmitted(
            Completion::Traffic {
                party: Party::Caller,
                call_id,
                command,
            },
            165,
        )
        .unwrap();
    mobile
        .offer_media(8 + extra_intervals, TransmitFrame::speech(&parameters))
        .unwrap();
    let incoming = mobile.prepare(8 + extra_intervals).unwrap().clone();
    receive(
        &mut voice,
        Party::Callee,
        8 + extra_intervals,
        incoming.data(),
        170,
    );
    mobile.transmitted(&incoming).unwrap();
    let (_, _, second) = voice
        .traffic(physical(&voice, Party::Caller, 7 + extra_intervals))
        .unwrap()
        .unwrap();
    let second_data = crate::phy::traffic::decode_forward(second.bits())
        .unwrap()
        .fields
        .data;
    let decoded =
        crate::phy::speech::decode(&crate::phy::speech::deinterleave(&first_data, &second_data))
            .unwrap();
    assert_eq!(decoded.bits, parameters.speech_bits());
    let command = voice
        .traffic_submitted(voice.call_id().unwrap(), Party::Caller, &second)
        .unwrap();
    voice
        .transmitted(
            Completion::Traffic {
                party: Party::Caller,
                call_id,
                command,
            },
            175,
        )
        .unwrap();

    mobile
        .enqueue(&TrafficMessage::Release {
            information: Default::default(),
        })
        .unwrap();
    for interval in [9, 10].map(|interval| interval + extra_intervals) {
        let prepared = mobile.prepare(interval).unwrap().clone();
        receive(
            &mut voice,
            Party::Callee,
            interval,
            prepared.data(),
            180 + interval,
        );
        mobile.transmitted(&prepared).unwrap();
    }
    assert!(matches!(
        voice.active().unwrap().session.state(),
        CallState::Releasing(_)
    ));
    voice.advance(6000).unwrap();
    assert!(voice.active().is_none());
    assert_eq!(voice.paging.reservations().count(), 0);
}
#[test]
fn establishment_timeout_keeps_slots_reserved_until_queued_assignment_finishes() {
    let (mut voice, registry) = setup();
    voice.receive_access(&registry, &origin(), 0, 0).unwrap();
    let phase = voice.page().unwrap().phase;
    complete_page(&mut voice, 0, 10);
    voice.receive_access(&registry, &response(), 20, 3).unwrap();
    voice.control(0, phase + 1).unwrap();
    let assignment = voice.control_completion();
    voice.control_submitted().unwrap();
    voice.advance(5020).unwrap();
    assert!(voice.active().is_some());
    assert_eq!(voice.paging.reservations().count(), 1);
    assert!(voice.transmitted(assignment, 5021).is_err());
    voice.advance(5022).unwrap();
    assert!(voice.active().is_none());
    assert_eq!(voice.paging.reservations().count(), 0);
}

#[test]
fn missed_interleaved_slot_requeues_a_complete_alert_without_claiming_transmission() {
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    let silent = TransmitFrame::speech(&VselpParameters::default());
    let data = TransmitFrame::interleave(&silent, &silent);
    for party in [Party::Caller, Party::Callee] {
        receive(&mut voice, party, 1, &data, 50);
    }
    voice.advance(51).unwrap();
    let expected = voice
        .active()
        .unwrap()
        .session
        .order(Party::Callee)
        .unwrap()
        .message()
        .clone();
    let words = crate::link::traffic::encode(&expected).unwrap().len() as u64;
    assert!(words > 1);
    send(&mut voice, Party::Callee, 1, 52);
    assert!(
        voice
            .active()
            .unwrap()
            .session
            .order(Party::Callee)
            .is_some()
    );
    let skipped = physical(&voice, Party::Callee, 2);
    voice.skipped_slot(skipped).unwrap();
    assert!(!voice.active().unwrap().session.media_ready());
    voice.advance(60).unwrap();
    let mut data = Vec::new();
    for interval in 3..=3 + words {
        let slot = physical(&voice, Party::Callee, interval);
        let (_, _, burst) = voice.traffic(slot).unwrap().unwrap();
        data.push(
            crate::phy::traffic::decode_forward(burst.bits())
                .unwrap()
                .fields
                .data,
        );
        let call_id = voice.call_id().unwrap();
        let command = voice
            .traffic_submitted(voice.call_id().unwrap(), Party::Callee, &burst)
            .unwrap();
        voice
            .transmitted(
                Completion::Traffic {
                    party: Party::Callee,
                    call_id,
                    command,
                },
                60 + interval,
            )
            .unwrap();
    }
    assert!(
        voice
            .active()
            .unwrap()
            .session
            .order(Party::Callee)
            .is_none()
    );
    let mut receiver =
        crate::link::traffic::TrafficMessageReceiver::new(crate::l3::relay::Direction::Forward);
    let mut received = Vec::new();
    for pair in data.windows(2) {
        let decoded =
            crate::phy::facch::decode(&crate::phy::facch::deinterleave(&pair[0], &pair[1]), 1)
                .unwrap();
        if let Some(message) = receiver
            .push(&crate::link::traffic::SignalingWord::from(&decoded))
            .unwrap()
        {
            received.push(message);
        }
    }
    assert_eq!(received, vec![expected]);
}

#[test]
fn operator_release_uses_call_state_machine_and_keeps_resources_until_bearers_stop() {
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    let calls = voice.operation_calls(30);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].legs.len(), 2);
    let id = calls[0].id;
    assert!(voice.release_call(id + 1, 30).is_err());
    voice.release_call(id, 30).unwrap();
    assert!(matches!(
        voice.active().unwrap().session.state(),
        CallState::Releasing(EndReason::Cancelled)
    ));
    assert_eq!(voice.operation_calls(30).len(), 1);
    voice.advance(6000).unwrap();
    assert!(voice.active().is_none());
    assert!(voice.operation_calls(6000).is_empty());
    assert_eq!(voice.recent_calls().len(), 1);
    assert!(voice.recent_calls()[0].end_reason.contains("Cancelled"));
}

#[test]
fn canceled_paging_records_the_outcome_and_allows_a_new_call() {
    let (mut voice, registry) = setup();
    voice.receive_access(&registry, &origin(), 0, 0).unwrap();
    let id = voice.operation_calls(1)[0].id;
    voice.release_call(id, 1).unwrap();
    assert!(voice.operation_calls(1).is_empty());
    assert_eq!(voice.recent_calls()[0].end_reason, "Cancelled while paging");
    voice.receive_access(&registry, &origin(), 2, 0).unwrap();
    assert_eq!(voice.operation_calls(2).len(), 1);
}

#[test]
fn canceled_paging_preserves_secondary_copy_and_clears_both_phones() {
    let (mut voice, registry) = setup();
    voice.receive_access(&registry, &origin(), 0, 0).unwrap();
    let phase = voice.page().unwrap().phase;
    let primary = *voice.control(0, phase).unwrap();
    let completion = voice.control_completion();
    voice.control_submitted().unwrap();
    let id = voice.operation_calls(1)[0].id;
    voice.release_call(id, 1).unwrap();
    voice.transmitted(completion, 2).unwrap();
    assert!(voice.page().is_none());
    assert_eq!(voice.rejections.len(), 2);
    assert_eq!(voice.rejections[0].identity, MobileIdentity::Min34(1));
    assert_eq!(voice.rejections[1].identity, MobileIdentity::Min34(2));
    let cause = crate::l3::registration::RejectCause::ServiceCodeNotAvailable;
    assert_eq!(
        voice.rejections[0].bytes,
        crate::l3::call_control::reorder(cause)
    );
    assert_eq!(
        voice.rejections[1].bytes,
        crate::l3::call_control::release(cause)
    );
    assert_eq!(voice.control(1, phase), Some(&primary));
    voice.control_submitted().unwrap();
    assert!(!voice.has_repeat(1, phase));
    for _ in 0..2 {
        assert!(voice.control(1, phase).is_some());
        voice.control_submitted().unwrap();
    }
    assert!(voice.rejections.is_empty());
    assert!(voice.receive_access(&registry, &response(), 3, 3).is_err());
    voice.receive_access(&registry, &origin(), 4, 3).unwrap();
    assert_eq!(voice.operation_calls(4).len(), 1);
}

#[test]
fn canceled_page_completion_cannot_mark_a_new_attempt_transmitted() {
    let (mut voice, registry) = setup();
    voice.receive_access(&registry, &origin(), 0, 0).unwrap();
    let phase = voice.page().unwrap().phase;
    assert!(voice.control(0, phase).is_some());
    let stale = voice.control_completion();
    voice.control_submitted().unwrap();
    let old_id = voice.operation_calls(1)[0].id;
    voice.release_call(old_id, 1).unwrap();
    voice.receive_access(&registry, &origin(), 2, 3).unwrap();
    let new_id = voice.operation_calls(2)[0].id;
    assert_ne!(old_id, new_id);
    assert!(voice.control(0, phase).is_none());
    voice.transmitted(stale, 3).unwrap();
    assert!(voice.receive_access(&registry, &response(), 4, 3).is_err());
    assert_eq!(voice.operation_calls(4)[0].id, new_id);
    assert!(voice.control(1, phase).is_some());
    voice.control_submitted().unwrap();
    complete_page(&mut voice, 2, 5);
    voice.receive_access(&registry, &response(), 6, 3).unwrap();
    assert!(voice.active().is_some());
}

#[test]
fn cancellation_response_capacity_failure_preserves_pending_call() {
    let (mut voice, registry) = setup();
    voice.receive_access(&registry, &origin(), 0, 0).unwrap();
    let id = voice.operation_calls(1)[0].id;
    for _ in 0..MAX_REJECTIONS - 1 {
        voice
            .reject_access(
                MobileIdentity::Min34(3),
                crate::l3::registration::RejectCause::ServiceCodeNotAvailable,
                0,
            )
            .unwrap();
    }
    assert!(voice.release_call(id, 1).is_err());
    assert!(voice.page().is_some());
    assert_eq!(voice.operation_calls(1)[0].id, id);
    voice.rejections.pop_front();
    voice.release_call(id, 2).unwrap();
    assert!(voice.page().is_none());
    assert_eq!(voice.rejections.len(), MAX_REJECTIONS);
}

#[test]
fn forward_call_responses_are_logged_only_after_tx_completion() {
    for release in [false, true] {
        let (mut voice, _) = setup();
        let log = std::sync::Arc::new(crate::operations::protocol::ProtocolLog::default());
        voice.set_protocol_log(log.clone());
        let cause = crate::l3::registration::RejectCause::VoiceCodecNotAvailable;
        if release {
            voice
                .reject_page_response(MobileIdentity::Min34(2), cause, 0)
                .unwrap();
        } else {
            voice
                .reject_access(MobileIdentity::Min34(1), cause, 0)
                .unwrap();
        }
        assert!(voice.control(0, 0).is_some());
        let completion = voice.control_completion();
        voice.control_submitted().unwrap();
        let air_messages = log.messages();
        assert!(air_messages.is_empty());
        voice.transmitted(completion, 1).unwrap();
        let air_messages = log.messages();
        assert_eq!(air_messages.len(), 1);
        assert_eq!(
            air_messages[0].name,
            if release { "Release" } else { "Reorder" }
        );
        assert!(air_messages[0].decoded.contains("VoiceCodecNotAvailable"));
    }
}

#[test]
fn traffic_assignment_uses_each_handsets_common_protocol_version() {
    let (mut voice, mut registry) = setup();
    for (identity, version) in [(1, ProtocolVersion::Is136Rev0), (2, ProtocolVersion::Pv7)] {
        registry
            .register(
                &set(
                    identity,
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: version,
                    }
                    .encode()
                    .unwrap(),
                ),
                0,
            )
            .unwrap();
    }
    assigned(&mut voice, &registry);
    let active = voice.active().unwrap();
    let caller = crate::l3::designation::decode_prefix(&active.legs[0].designation).unwrap();
    let callee = crate::l3::designation::decode_prefix(&active.legs[1].designation).unwrap();
    assert_eq!(caller.message.protocol_version, ProtocolVersion::Is136Rev0);
    assert_eq!(callee.message.protocol_version, ProtocolVersion::Pv2);
    assert_eq!(
        active.session.protocol_version(Party::Caller),
        Some(caller.message.protocol_version)
    );
    assert_eq!(
        active.session.protocol_version(Party::Callee),
        Some(callee.message.protocol_version)
    );
    registry
        .register(
            &set(
                1,
                AccessMessage::Registration {
                    registration_type: RegistrationType::Periodic,
                    scm: 15,
                    protocol_version: ProtocolVersion::Pv7,
                }
                .encode()
                .unwrap(),
            ),
            1,
        )
        .unwrap();
    assert_eq!(
        voice
            .active()
            .unwrap()
            .session
            .protocol_version(Party::Caller),
        Some(ProtocolVersion::Is136Rev0)
    );
}

#[test]
fn coded_channel_quality_reports_update_phone_measurements_without_call_transition() {
    use crate::l3::legacy_traffic::{ChannelQualityReport, DownlinkQuality};
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    let mut mobile = TrafficTx::try_new(1, 16, 1).unwrap();
    mobile
        .enqueue(&TrafficMessage::ChannelQuality(
            ChannelQualityReport::First {
                current: DownlinkQuality {
                    ber_code: 2,
                    rssi_code: 17,
                },
                neighbor_rssi_codes: [0; 6],
            },
        ))
        .unwrap();
    for interval in [1, 2] {
        let prepared = mobile.prepare(interval).unwrap().clone();
        receive(
            &mut voice,
            Party::Caller,
            interval,
            prepared.data(),
            50 + interval,
        );
        mobile.transmitted(&prepared).unwrap();
    }
    let active = voice.active().unwrap();
    assert_eq!(active.session.state(), CallState::Establishing);
    assert_eq!(
        active.legs[0].measurements.downlink,
        Some(DownlinkQuality {
            ber_code: 2,
            rssi_code: 17
        })
    );
    assert!(active.legs[1].measurements.downlink.is_none());
    let calls = voice.operation_calls(100);
    assert!(
        calls[0].legs[0]
            .measurements
            .iter()
            .any(|metric| metric.name == "Phone reported downlink RSSI"
                && metric.value.as_deref() == Some("-79"))
    );
}

#[test]
fn same_batch_relay_acceptance_is_persisted_before_call_teardown_cancels_attempts() {
    use crate::{
        core::{
            messaging::LocalMessaging,
            store::{NewSms, StateStore},
        },
        l3::{
            legacy_traffic::TrafficRelay,
            relay::RelayMessage,
            sms::{DisplayTime, Metadata, SmsMessage, UserData, Validity},
        },
        runtime::sms::{RelayChannel, SmsRuntime, TrafficPeer},
    };
    let (mut voice, registry) = setup();
    let mut store = StateStore::open(":memory:").unwrap();
    let provisioning: Vec<_> = registry
        .subscribers()
        .iter()
        .map(|subscriber| subscriber.provisioning.clone())
        .collect();
    store.reconcile(&provisioning).unwrap();
    let recipient = MobileIdentity::Min34(2);
    store
        .enqueue(NewSms {
            dedup_key: "same-batch-acceptance",
            sender: MobileIdentity::Min34(1),
            recipient,
            message: &SmsMessage::Deliver {
                metadata: Metadata {
                    message_reference: 1,
                    privacy: 0,
                    urgency: 0,
                    delivery_acknowledgment: false,
                    manual_acknowledgment: false,
                    new_message: true,
                },
                validity: Validity::Indefinite,
                display_time: DisplayTime::Default,
                user_data: UserData::Ira(b"accepted before release".to_vec()),
            },
            created_at_unix_ms: 0,
        })
        .unwrap();
    let mut core = LocalMessaging::from_store(store).unwrap();
    core.register(
        &set(
            2,
            AccessMessage::Registration {
                registration_type: RegistrationType::PowerUp,
                scm: 15,
                protocol_version: ProtocolVersion::Is136RevA,
            }
            .encode()
            .unwrap(),
        ),
        0,
    )
    .unwrap();
    let mut sms = SmsRuntime::try_new(core, voice.allocation, "same-batch".into()).unwrap();
    let peer = TrafficPeer {
        identity: recipient,
        call_id: 7,
        ready: true,
    };
    sms.sync_traffic(vec![peer], 0).unwrap();
    sms.advance(12800).unwrap();
    let offer = sms.traffic_offer(peer, RelayChannel::Sacch).unwrap();
    let TrafficMessage::Relay(TrafficRelay {
        message: RelayMessage::Data { transaction_id, .. },
        ..
    }) = &offer.message
    else {
        panic!()
    };
    sms.traffic_queued(offer.token).unwrap();
    voice.sms_completed.push_back(SmsCompletion {
        token: offer.token,
        completed_ms: 13800,
    });
    voice.sms_inbox.push_back((
        peer,
        TrafficRelay {
            message: RelayMessage::Accept {
                transaction_id: *transaction_id,
            },
            optional_octets: vec![],
        },
    ));
    voice.exchange_sms(&mut sms, 13800, 0).unwrap();
    assert!(
        sms.core()
            .store()
            .pending_for(recipient)
            .unwrap()
            .is_empty()
    );
    assert!(!sms.subscriber_busy(recipient));
}

#[test]
fn shutdown_records_a_pending_status_poll_as_cancelled() {
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    let active = voice.active_mut().unwrap();
    let caller = active.legs[0].assignment.handle.clone();
    let callee = active.legs[1].assignment.handle.clone();
    active.session.bearer_confirmed(&caller, 40).unwrap();
    active.session.bearer_confirmed(&callee, 41).unwrap();
    let alert = active.session.order(Party::Callee).unwrap().clone();
    active
        .session
        .order_transmitted(&callee, &alert, 42)
        .unwrap();
    active
        .session
        .receive(&callee, &TrafficMessage::Connect, 43)
        .unwrap();
    let connect = active.session.response(Party::Callee).unwrap().clone();
    active.session.response_transmitted(&connect, 44).unwrap();
    voice
        .request_status(voice.call_id().unwrap(), Party::Caller, 45)
        .unwrap();
    voice.shutdown(46).unwrap();
    let history = voice.recent_calls();
    let caller = history[0]
        .legs
        .iter()
        .find(|leg| leg.party == "Caller")
        .unwrap();
    assert!(caller.measurements.iter().any(
        |metric| metric.name == "Status query" && metric.value.as_deref() == Some("Cancelled")
    ));
    assert!(voice.call_id().is_none());
}

#[test]
fn shortened_acquisition_binds_sync_dvcc_and_slot_without_confirming_a_full_bearer() {
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    let party = Party::Caller;
    let slot = physical(&voice, party, 1);
    let relative = slot as f64 * 162.0 - 207.0 + 1.5;
    let short =
        crate::phy::shortened::decode(&crate::phy::shortened::encode(SyncWord::Two, 1).unwrap())
            .unwrap();
    let mut wrong = short;
    wrong.color.dvcc = 2;
    voice.receive_shortened(relative, &wrong, 40).unwrap();
    wrong = short;
    wrong.sync = SyncWord::Three;
    voice.receive_shortened(relative, &wrong, 40).unwrap();
    assert!(
        !voice
            .active()
            .unwrap()
            .session
            .physical_control_transmitted(party)
    );
    assert_eq!(
        voice.active().unwrap().legs[0].initial_alignment,
        alignment::InitialAlignment::Shortened
    );
    voice.receive_shortened(relative, &short, 41).unwrap();
    voice.receive_shortened(relative, &short, 41).unwrap();
    let active = voice.active().unwrap();
    assert_eq!(active.legs[0].measurements.shortened_bursts, 1);
    assert_eq!(
        active
            .session
            .physical_control_request(party)
            .unwrap()
            .time_alignment,
        Some(TimeAlignment::OffsetUnits(3))
    );
    assert_eq!(active.session.bearer_state(party), BearerState::Assigning);
    let silent = TransmitFrame::speech(&VselpParameters::default());
    let data = TransmitFrame::interleave(&silent, &silent);
    voice
        .receive_traffic(relative, 0, &fields(party, &data, 1), None, 42)
        .unwrap();
    assert_eq!(
        voice.active().unwrap().session.bearer_state(party),
        BearerState::Assigning
    );
    let active = voice.active_mut().unwrap();
    let handle = active.legs[0].assignment.handle.clone();
    let order = active.session.order(party).unwrap().clone();
    active
        .session
        .order_transmitted(&handle, &order, 43)
        .unwrap();
    let later_slot = physical(&voice, party, 2);
    voice
        .receive_traffic(
            later_slot as f64 * 162.0 - 207.0 + 2.0,
            0,
            &fields(party, &data, 1),
            None,
            44,
        )
        .unwrap();
    assert_eq!(
        voice.active().unwrap().session.bearer_state(party),
        BearerState::Assigning
    );
    receive(&mut voice, party, 2, &data, 45);
    assert_eq!(
        voice.active().unwrap().session.bearer_state(party),
        BearerState::Established
    );
    assert!(!voice.active().unwrap().session.media_ready());
}
#[test]
fn unsupported_page_mode_rejects_the_caller_and_frees_both_unassigned_bearers() {
    use crate::l3::{
        access_call::{CallOptions, LegacyVoice, PageResponseRequest},
        registration::RejectCause,
    };
    let (mut voice, registry) = setup();
    voice.receive_access(&registry, &origin(), 0, 0).unwrap();
    let phase = voice.page().unwrap().phase;
    complete_page(&mut voice, 0, 10);
    let malformed = voice
        .receive_access(&registry, &set(2, vec![8]), 15, 3)
        .unwrap_err();
    assert_eq!(page_response_rejection_cause(malformed.as_ref()), None);
    assert!(voice.rejections.is_empty());
    assert_eq!(voice.paging.reservations().count(), 1);
    assert!(voice.page().is_some());
    let primary = PageResponseRequest {
        message: PageResponse {
            protocol_version: ProtocolVersion::Is136RevA,
            last_try: false,
            scm: 15,
            service_code: ServiceCode::DigitalSpeech,
        },
        options: CallOptions {
            voices: vec![LegacyVoice {
                coder: 3,
                privacy: 0,
            }],
            ..Default::default()
        },
    }
    .encode()
    .unwrap();
    let error = voice
        .receive_access(&registry, &set(2, primary), 20, 3)
        .unwrap_err();
    assert_eq!(
        access_rejection_cause(error.as_ref()),
        RejectCause::VoiceCodecNotAvailable
    );
    assert!(voice.page().is_none());
    assert!(voice.active().is_none());
    assert_eq!(voice.paging.reservations().count(), 0);
    assert_eq!(voice.recent_calls()[0].end_reason, "Call mode rejected");
    assert_eq!(voice.rejections.len(), 1);
    assert_eq!(voice.rejections[0].identity, MobileIdentity::Min34(1));
    assert_eq!(
        voice.rejections[0].bytes,
        crate::l3::call_control::reorder(RejectCause::VoiceCodecNotAvailable)
    );
    let information = *voice.control(0, phase + 1).unwrap();
    let message = crate::link::spach::decode_single(&information).unwrap();
    assert_eq!(message.channel, crate::link::spach::Channel::Arch);
    assert_eq!(message.identity, MobileIdentity::Min34(1));
    assert_eq!(
        message.message,
        crate::l3::call_control::reorder(RejectCause::VoiceCodecNotAvailable)
    );
    voice
        .reject_page_response(
            MobileIdentity::Min34(2),
            RejectCause::VoiceCodecNotAvailable,
            20,
        )
        .unwrap();
    assert_eq!(
        voice.rejections[1].bytes,
        crate::l3::call_control::release(RejectCause::VoiceCodecNotAvailable)
    );
    voice.receive_access(&registry, &origin(), 21, 3).unwrap();
    assert_eq!(voice.paging.reservations().count(), 1);
    assert!(voice.rejections.is_empty());
}
#[test]
fn rejected_or_expired_pages_notify_the_waiting_caller_without_retaining_resources() {
    use crate::l3::registration::RejectCause;
    for rejected in [false, true] {
        let (mut voice, registry) = setup();
        voice.receive_access(&registry, &origin(), 0, 0).unwrap();
        complete_page(&mut voice, 0, 10);
        if rejected {
            let bytes = PageResponse {
                protocol_version: ProtocolVersion::Is136RevA,
                last_try: false,
                scm: 15,
                service_code: ServiceCode::ServiceRejected,
            }
            .encode()
            .unwrap();
            voice
                .receive_access(&registry, &set(2, bytes), 20, 3)
                .unwrap();
        } else {
            let deadline = voice.page().unwrap().request.deadline_monotonic_ms;
            voice.advance(deadline).unwrap();
        }
        assert_eq!(voice.paging.reservations().count(), 0);
        assert!(voice.page().is_none());
        assert_eq!(voice.rejections.len(), 2);
        assert_eq!(voice.rejections[0].identity, MobileIdentity::Min34(1));
        assert_eq!(voice.rejections[1].identity, MobileIdentity::Min34(2));
        assert_eq!(
            voice.rejections[1].bytes,
            crate::l3::call_control::release(RejectCause::ServiceCodeNotAvailable)
        );
        assert_eq!(
            voice.rejections[0].bytes,
            crate::l3::call_control::reorder(RejectCause::ServiceCodeNotAvailable)
        );
    }
}

#[test]
fn unsupported_known_access_services_get_explicit_causes_without_retaining_bearers() {
    use crate::l3::registration::RejectCause;
    for service_code in [ServiceCode::DirectAsyncData, ServiceCode::Wort] {
        let (mut voice, registry) = setup();
        let mut request = crate::l3::origination::decode_prefix(&origin().messages[0])
            .unwrap()
            .message;
        request.service_code = service_code;
        let error = voice
            .receive_access(&registry, &set(1, request.encode().unwrap()), 0, 0)
            .unwrap_err();
        assert_eq!(
            access_rejection_cause(error.as_ref()),
            RejectCause::ServiceCodeNotSupported
        );
        assert_eq!(voice.paging.reservations().count(), 0);
        assert!(voice.page().is_none());
        voice.receive_access(&registry, &origin(), 1, 0).unwrap();
        let phase = voice.page().unwrap().phase;
        complete_page(&mut voice, 0, 10);
        let primary = PageResponse {
            protocol_version: ProtocolVersion::Is136RevA,
            last_try: false,
            scm: 15,
            service_code,
        }
        .encode()
        .unwrap();
        let error = voice
            .receive_access(&registry, &set(2, primary), 20, 3)
            .unwrap_err();
        assert_eq!(
            page_response_rejection_cause(error.as_ref()),
            Some(RejectCause::ServiceCodeNotSupported)
        );
        assert!(voice.page().is_none());
        assert_eq!(voice.paging.reservations().count(), 0);
        voice
            .reject_page_response(
                MobileIdentity::Min34(2),
                RejectCause::ServiceCodeNotSupported,
                20,
            )
            .unwrap();
        assert_eq!(voice.rejections.len(), 2);
        assert_eq!(
            voice.rejections[0].bytes,
            crate::l3::call_control::reorder(RejectCause::ServiceCodeNotSupported)
        );
        assert_eq!(
            voice.rejections[1].bytes,
            crate::l3::call_control::release(RejectCause::ServiceCodeNotSupported)
        );
        assert!(voice.has_repeat(1, phase));
        voice.receive_access(&registry, &origin(), 21, 3).unwrap();
        assert_eq!(voice.paging.reservations().count(), 1);
    }
}

#[test]
fn extended_requests_do_not_upgrade_the_assigned_bearer_or_mutate_legacy_measurements() {
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    let id = voice.call_id().unwrap();
    let targets = crate::l3::legacy_traffic::ExtendedMeasurementTargets::try_new(
        crate::l3::legacy_traffic::ExtendedMeasurementType::WordErrorRate,
        vec![],
    )
    .unwrap();
    let before = voice
        .active()
        .unwrap()
        .session
        .measurement_kind(Party::Caller);
    assert!(
        voice
            .request_extended_measurements(id, Party::Caller, targets.clone(), 40)
            .unwrap_err()
            .to_string()
            .contains("PV2")
    );
    let active = voice.active().unwrap();
    assert_eq!(
        active.session.protocol_version(Party::Caller),
        Some(ProtocolVersion::Is136RevA)
    );
    assert_eq!(active.session.measurement_kind(Party::Caller), before);
    assert!(
        active
            .session
            .extended_measurements(Party::Caller)
            .is_none()
    );
    assert!(
        voice
            .request_extended_measurements(id + 1, Party::Caller, targets, 41)
            .unwrap_err()
            .to_string()
            .contains("no longer active")
    );
}

fn sms_offer_fixture() -> (
    VoiceRuntime,
    super::super::sms::SmsRuntime,
    super::super::sms::TrafficOffer,
) {
    use crate::{
        core::{
            messaging::LocalMessaging,
            store::{NewSms, StateStore},
        },
        l3::sms::{DisplayTime, Metadata, SmsMessage, UserData, Validity},
        runtime::sms::{RelayChannel, SmsRuntime, TrafficPeer},
    };
    let (mut voice, registry) = setup();
    assigned(&mut voice, &registry);
    let mut store = StateStore::open(":memory:").unwrap();
    let provisioning: Vec<_> = registry
        .subscribers()
        .iter()
        .map(|subscriber| subscriber.provisioning.clone())
        .collect();
    store.reconcile(&provisioning).unwrap();
    let recipient = MobileIdentity::Min34(2);
    store
        .enqueue(NewSms {
            dedup_key: "same-batch-acceptance",
            sender: MobileIdentity::Min34(1),
            recipient,
            message: &SmsMessage::Deliver {
                metadata: Metadata {
                    message_reference: 1,
                    privacy: 0,
                    urgency: 0,
                    delivery_acknowledgment: false,
                    manual_acknowledgment: false,
                    new_message: true,
                },
                validity: Validity::Indefinite,
                display_time: DisplayTime::Default,
                user_data: UserData::Ira(b"accepted before release".to_vec()),
            },
            created_at_unix_ms: 0,
        })
        .unwrap();
    let mut core = LocalMessaging::from_store(store).unwrap();
    core.register(
        &set(
            2,
            AccessMessage::Registration {
                registration_type: RegistrationType::PowerUp,
                scm: 15,
                protocol_version: ProtocolVersion::Is136RevA,
            }
            .encode()
            .unwrap(),
        ),
        0,
    )
    .unwrap();
    let mut sms = SmsRuntime::try_new(core, voice.allocation, "same-batch".into()).unwrap();
    let peer = TrafficPeer {
        identity: recipient,
        call_id: voice.call_id().unwrap(),
        ready: true,
    };
    sms.sync_traffic(vec![peer], 0).unwrap();
    sms.advance(12800).unwrap();
    let offer = sms.traffic_offer(peer, RelayChannel::Sacch).unwrap();

    (voice, sms, offer)
}

fn obsolete_offer_drain(channel: super::super::sms::RelayChannel) {
    use super::super::sms::RelayChannel;
    let (mut voice, mut sms, offer) = sms_offer_fixture();
    sms.traffic_queued(offer.token).unwrap();
    sms.traffic_interrupted(offer.token, 12800).unwrap();
    assert!(!sms.traffic_valid(offer.token));
    let leg = &mut voice.active_mut().unwrap().legs[0];
    let facch = match channel {
        RelayChannel::Facch => Some(leg.tx.enqueue_facch(&offer.message).unwrap()),
        RelayChannel::Sacch => None,
    };
    let sacch = match channel {
        RelayChannel::Sacch => Some(leg.tx.enqueue_sacch(&offer.message).unwrap()),
        RelayChannel::Facch => None,
    };
    let first_interval = leg.next_tx;
    let first = leg
        .tx
        .prepare(first_interval, SyncWord::Two)
        .unwrap()
        .clone();
    leg.tx.transmitted(&first).unwrap();
    match channel {
        RelayChannel::Facch => leg.sms_facch = Some((facch.clone().unwrap(), offer)),
        RelayChannel::Sacch => leg.sms_sacch = Some((sacch.clone().unwrap(), offer)),
    }
    voice.exchange_sms(&mut sms, 12800, 0).unwrap();
    let leg = &mut voice.active_mut().unwrap().legs[0];
    match channel {
        RelayChannel::Facch => {
            assert!(leg.sms_facch.is_some());
            assert!(
                leg.tx
                    .cancel_facch(facch.as_ref().unwrap())
                    .unwrap()
                    .in_flight
            );
        }
        RelayChannel::Sacch => {
            assert!(leg.sms_sacch.is_some());
            assert!(
                leg.tx
                    .cancel_sacch(sacch.as_ref().unwrap())
                    .unwrap()
                    .in_flight
            );
        }
    }
    let drain_intervals = (crate::phy::sacch::WORD_SLOTS + SACCH_BITS) as u64;
    for interval in first_interval + 1..=first_interval + drain_intervals {
        let burst = leg.tx.prepare(interval, SyncWord::Two).unwrap().clone();
        leg.tx.transmitted(&burst).unwrap();
    }

    voice.exchange_sms(&mut sms, 12800, 0).unwrap();
    let leg = &voice.active().unwrap().legs[0];
    assert!(leg.sms_facch.is_none());
    assert!(leg.sms_sacch.is_none());
}

#[test]
fn obsolete_sacch_offer_remains_owned_until_encoder_drain() {
    obsolete_offer_drain(super::super::sms::RelayChannel::Sacch);
}
#[test]
fn obsolete_facch_offer_remains_owned_until_encoder_drain() {
    obsolete_offer_drain(super::super::sms::RelayChannel::Facch);
}

fn voice_sms_completion_timestamp(channel: super::super::sms::RelayChannel) {
    use super::super::sms::RelayChannel;
    const CALLBACK_LAG_MS: u64 = 500;
    const TRAFFIC_INTERVAL_MS: u64 = 20;
    let (mut voice, mut sms, offer) = sms_offer_fixture();
    sms.traffic_queued(offer.token).unwrap();
    let token = offer.token;
    let word_count = crate::link::traffic::encode(&offer.message).unwrap().len();
    let call_id = voice.call_id().unwrap();
    let leg = &mut voice.active_mut().unwrap().legs[index(Party::Callee)];
    match channel {
        RelayChannel::Facch => {
            let handle = leg.tx.enqueue_facch(&offer.message).unwrap();
            leg.sms_facch = Some((handle, offer));
        }
        RelayChannel::Sacch => {
            let handle = leg.tx.enqueue_sacch(&offer.message).unwrap();
            leg.sms_sacch = Some((handle, offer));
        }
    }
    let first_interval = leg.next_tx;
    let maximum_intervals = (word_count * crate::phy::sacch::WORD_SLOTS + SACCH_BITS + 1) as u64;
    let mut final_completed_ms = None;
    for offset in 0..maximum_intervals {
        let leg = &mut voice.active_mut().unwrap().legs[index(Party::Callee)];
        let burst = leg
            .tx
            .prepare(first_interval + offset, SyncWord::Three)
            .unwrap()
            .clone();
        let completed_ms = super::super::timers::SMS_ORIGINATION_MS + offset * TRAFFIC_INTERVAL_MS;
        let command = voice
            .traffic_submitted_at(
                voice.call_id().unwrap(),
                Party::Callee,
                &burst,
                completed_ms,
            )
            .unwrap();
        voice
            .transmitted_at(
                Completion::Traffic {
                    party: Party::Callee,
                    call_id,
                    command,
                },
                completed_ms,
                completed_ms + CALLBACK_LAG_MS,
            )
            .unwrap();
        if !voice.sms_completed.is_empty() {
            final_completed_ms = Some(completed_ms);
            break;
        }
    }
    assert_eq!(voice.sms_handoffs.len(), 1);
    assert_eq!(voice.sms_handoffs[0].token, token);
    assert_eq!(Some(voice.sms_handoffs[0].observed_ms), final_completed_ms);
    assert_eq!(voice.sms_completed.len(), 1);
    let completion = voice.sms_completed.pop_front().unwrap();
    assert_eq!(completion.token, token);
    assert_eq!(Some(completion.completed_ms), final_completed_ms);
    assert!(
        voice
            .transmitted_at(
                Completion::None,
                completion.completed_ms + 1,
                completion.completed_ms
            )
            .is_err()
    );
}

#[test]
fn voice_keeps_facch_final_completion_time_independent_of_observation() {
    voice_sms_completion_timestamp(super::super::sms::RelayChannel::Facch);
}
#[test]
fn voice_keeps_sacch_final_completion_time_independent_of_observation() {
    voice_sms_completion_timestamp(super::super::sms::RelayChannel::Sacch);
}

#[test]
fn decoded_sms_waiting_for_dispatch_keeps_receive_ownership_without_an_active_call() {
    let (mut voice, _) = setup();
    let identity = MobileIdentity::Min34(1);
    assert!(!voice.retains_received_signaling(identity));
    voice.sms_inbox.push_back((
        super::super::sms::TrafficPeer {
            identity,
            call_id: 1,
            ready: true,
        },
        crate::l3::legacy_traffic::TrafficRelay {
            message: crate::l3::relay::RelayMessage::Accept { transaction_id: 1 },
            optional_octets: Vec::new(),
        },
    ));
    assert!(voice.retains_received_signaling(identity));
    assert!(!voice.retains_received_signaling(MobileIdentity::Min34(2)));
    voice.sms_inbox.pop_front();
    assert!(!voice.retains_received_signaling(identity));
}

#[test]
fn traffic_acquisition_is_required_only_while_a_bearer_is_allocated() {
    let (mut voice, registry) = setup();
    assert!(!voice.needs_traffic_reception());
    assigned(&mut voice, &registry);
    assert!(voice.needs_traffic_reception());
    voice.shutdown(50).unwrap();
    assert!(!voice.needs_traffic_reception());
}
#[test]
fn raw_analog_only_origination_reports_the_requested_service_without_allocating_a_call() {
    let (mut voice, registry) = setup();
    let message = set(1, vec![0x07, 0x40, 0x18, 0x09, 0x11, 0x11, 0x60]);
    let error = voice.receive_access(&registry, &message, 0, 0).unwrap_err();
    assert!(error.to_string().contains("AnalogSpeech"));
    assert_eq!(
        access_rejection_cause(error.as_ref()),
        RejectCause::ServiceCodeNotSupported
    );
    assert!(voice.page().is_none());
    assert!(!voice.needs_traffic_reception());
}
