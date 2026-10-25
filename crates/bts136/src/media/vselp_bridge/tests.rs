use super::*;
use crate::{
    codec::vselp::{SUBFRAMES, VselpSubframe},
    core::{
        call_session::{EndReason, SessionPolicy},
        calls::CallReservations,
        registry::{LocalRegistry, ProvisionedSubscriber},
    },
    l3::{
        broadcast::ProtocolVersion,
        designation::AssignedSlots,
        registration::{AccessMessage, RegistrationType},
    },
    link::{
        rach::{MessageSet, MobileIdentity},
        traffic,
        traffic_order::OrderPolicy,
    },
    phy::{channel::Band, speech},
    resources::TrafficResources,
};
fn parameters(r0: u8, code: u8) -> VselpParameters {
    VselpParameters::try_new(
        r0,
        [32, 16, 16, 8, 8, 4, 4, 4, 4, 2],
        [VselpSubframe::try_new(21, code, code ^ 0x7f, 170).unwrap(); SUBFRAMES],
    )
    .unwrap()
}
fn session() -> (CallSession, [BearerHandle; 2]) {
    let mut registry = LocalRegistry::try_new(
        (1..=2)
            .map(|id| ProvisionedSubscriber {
                identity: MobileIdentity::Min34(id),
                number: format!("100{id}"),
            })
            .collect(),
    )
    .unwrap();
    let registration = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 21,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    for id in 1..=2 {
        registry
            .register(
                &MessageSet {
                    identity: MobileIdentity::Min34(id),
                    messages: vec![registration.clone()],
                },
                0,
            )
            .unwrap();
    }
    let mut calls = CallReservations::new(
        TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
    );
    let reservation = calls
        .reserve(&registry, MobileIdentity::Min34(1), "1002")
        .unwrap();
    let order = OrderPolicy {
        response_timeout_ms: 100,
        overall_timeout_ms: 500,
        max_transmissions: 3,
    };
    let mut session = CallSession::try_new(
        reservation,
        SessionPolicy {
            establishment_timeout_ms: 500,
            answer_timeout_ms: 1000,
            release_timeout_ms: 500,
            alert: order,
            release: order,
            measurements: order,
            status: order,
            capabilities: order,
            physical_control: order,
        },
        0,
    )
    .unwrap();
    let handles = [
        session.assignment(Party::Caller).unwrap().handle,
        session.assignment(Party::Callee).unwrap().handle,
    ];
    for handle in &handles {
        session.assignment_started(handle, 10).unwrap();
    }
    (session, handles)
}
fn connect(session: &mut CallSession, handles: &[BearerHandle; 2]) {
    for handle in handles {
        session.bearer_confirmed(handle, 20).unwrap();
    }
    let alert = session.order(Party::Callee).unwrap().clone();
    session.order_transmitted(&handles[1], &alert, 30).unwrap();
    session
        .receive(&handles[1], &TrafficMessage::Connect, 40)
        .unwrap();
    let ack = session.response(Party::Callee).unwrap().clone();
    session.response_transmitted(&ack, 50).unwrap();
}
fn bridge(session: &CallSession) -> VselpBridge {
    VselpBridge::new(
        session,
        LegConfig {
            dvcc: 10,
            preceding: VselpParameters::default(),
        },
        LegConfig {
            dvcc: 42,
            preceding: VselpParameters::default(),
        },
    )
}
fn slot(parameters: &VselpParameters) -> [bool; DATA_BITS] {
    let coded = speech::encode(&parameters.speech_bits());
    speech::interleave(&coded, &coded)
}
#[test]
fn each_direction_preserves_received_codec_parameters_and_has_independent_interval_tracking() {
    let (mut session, handles) = session();
    connect(&mut session, &handles);
    let mut bridge = bridge(&session);
    for (source, parameters, interval) in [
        (Party::Caller, parameters(20, 37), 100),
        (Party::Callee, parameters(21, 54), 200),
    ] {
        let data = slot(&parameters);
        let handle = &handles[index(source)];
        assert!(
            bridge
                .push(&session, handle, interval, Some(&data))
                .unwrap()
                .is_none()
        );
        let output = bridge
            .push(&session, handle, interval + 1, Some(&data))
            .unwrap()
            .unwrap();
        assert_eq!(output.source, source);
        assert_eq!(output.destination, opposite(source));
        assert_eq!(output.source_interval, interval);
        assert_eq!(output.disposition, MediaDisposition::Speech);
        assert_eq!(output.received.parameters, Some(parameters.clone()));
        assert_eq!(output.transmit, TransmitFrame::speech(&parameters));
        assert_eq!(output.signaling, Ok(None));
    }
}
#[test]
fn connection_boundary_does_not_forward_preanswer_audio_and_release_mutes_pending_audio() {
    let (mut session, handles) = session();
    let mut bridge = bridge(&session);
    let parameters = parameters(20, 37);
    let data = slot(&parameters);
    bridge.push(&session, &handles[0], 0, Some(&data)).unwrap();
    connect(&mut session, &handles);
    let first = bridge
        .push(&session, &handles[0], 1, Some(&data))
        .unwrap()
        .unwrap();
    assert_eq!(first.disposition, MediaDisposition::Silence);
    assert_eq!(
        first.transmit,
        TransmitFrame::speech(&VselpParameters::default())
    );
    let second = bridge
        .push(&session, &handles[0], 2, Some(&data))
        .unwrap()
        .unwrap();
    assert_eq!(second.disposition, MediaDisposition::Speech);
    session.end(EndReason::Cancelled, 60).unwrap();
    let muted = bridge
        .push(&session, &handles[0], 3, Some(&data))
        .unwrap()
        .unwrap();
    assert_eq!(muted.disposition, MediaDisposition::Silence);
    assert_eq!(
        muted.transmit,
        TransmitFrame::speech(&VselpParameters::default())
    );
}
#[test]
fn private_facch_is_reassembled_for_its_source_and_replaced_by_peer_filler() {
    let (mut session, handles) = session();
    connect(&mut session, &handles);
    let mut bridge = bridge(&session);
    let message = TrafficMessage::Release {
        information: crate::l3::legacy_traffic::ReleaseInformation {
            unparsed_bits: ([1, 2, 3, 4, 5, 6, 7, 8, 9])
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
    let frames: Vec<_> = traffic::encode(&message)
        .unwrap()
        .iter()
        .map(|word| TransmitFrame::facch(word, 10))
        .collect();
    let padding = TransmitFrame::speech(&VselpParameters::default());
    let mut previous = padding.clone();
    let mut messages = Vec::new();
    for (interval, current) in frames.iter().chain(std::iter::once(&padding)).enumerate() {
        let data = TransmitFrame::interleave(&previous, current);
        if let Some(output) = bridge
            .push(&session, &handles[0], interval as u64, Some(&data))
            .unwrap()
        {
            assert_eq!(output.disposition, MediaDisposition::Concealment);
            assert_eq!(
                output.transmit,
                TransmitFrame::facch(&SignalingWord::filler(), 42)
            );
            if let Some(message) = output.signaling.unwrap() {
                messages.push(message);
            }
        }
        previous = current.clone();
    }
    assert_eq!(messages, [message]);
}
#[test]
fn missing_slot_affects_two_frames_and_failed_crc_is_never_reencoded_as_valid_speech() {
    let (mut session, handles) = session();
    connect(&mut session, &handles);
    let mut bridge = bridge(&session);
    let data = slot(&parameters(20, 37));
    bridge.push(&session, &handles[0], 0, Some(&data)).unwrap();
    for (interval, data) in [(1, None), (2, Some(&data))] {
        let output = bridge
            .push(&session, &handles[0], interval, data)
            .unwrap()
            .unwrap();
        assert_eq!(output.received.kind, FrameKind::Missing);
        assert_eq!(output.disposition, MediaDisposition::Concealment);
        assert_eq!(
            output.transmit,
            TransmitFrame::facch(&SignalingWord::filler(), 42)
        );
    }
    let invalid = "10000000011100100111000110011010100011111110001101100101000111111100011011010110001111111000101001011010010100101101001010010110100101101010001111111000110111010100011111110001101101001000111111100011011010011101011000000000010110100101001011010010100101101001";
    let coded: [bool; DATA_BITS] = invalid
        .bytes()
        .map(|bit| bit == b'1')
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let data = speech::interleave(&coded, &coded);
    let mut fresh = VselpBridge::new(
        &session,
        LegConfig {
            dvcc: 10,
            preceding: VselpParameters::default(),
        },
        LegConfig {
            dvcc: 42,
            preceding: VselpParameters::default(),
        },
    );
    fresh.push(&session, &handles[0], 0, Some(&data)).unwrap();
    let output = fresh
        .push(&session, &handles[0], 1, Some(&data))
        .unwrap()
        .unwrap();
    assert!(matches!(
        output.received.kind,
        FrameKind::Speech {
            crc: CrcStatus::Failed,
            ..
        }
    ));
    assert!(output.received.parameters.is_some());
    assert_eq!(output.disposition, MediaDisposition::Concealment);
}
#[test]
fn foreign_sessions_bearers_and_bad_intervals_cannot_advance_receiver_state() {
    let (mut session, handles) = session();
    connect(&mut session, &handles);
    let (foreign, foreign_handles) = self::session();
    let mut bridge = bridge(&session);
    let data = slot(&parameters(20, 37));
    assert_eq!(
        bridge.push(&foreign, &foreign_handles[0], 0, Some(&data)),
        Err(BridgeError::Call)
    );
    assert_eq!(
        bridge.push(&session, &foreign_handles[0], 0, Some(&data)),
        Err(BridgeError::Session(SessionError::StaleBearer))
    );
    assert_eq!(
        bridge.push(&session, &handles[0], u64::MAX, Some(&data)),
        Err(BridgeError::IntervalOverflow)
    );
    assert!(
        bridge
            .push(&session, &handles[0], 100, Some(&data))
            .unwrap()
            .is_none()
    );
    let mut expected = bridge.clone();
    for interval in [100, 102] {
        assert!(matches!(
            bridge.push(&session, &handles[0], interval, Some(&data)),
            Err(BridgeError::Interval { expected: 101, .. })
        ));
    }
    assert_eq!(
        bridge
            .push(&session, &handles[0], 101, Some(&data))
            .unwrap(),
        expected
            .push(&session, &handles[0], 101, Some(&data))
            .unwrap()
    );
}

#[test]
fn stopped_bearers_cannot_submit_more_media_even_with_an_original_matching_handle() {
    let (mut session, handles) = session();
    connect(&mut session, &handles);
    let mut bridge = bridge(&session);
    let data = slot(&parameters(20, 37));
    bridge.push(&session, &handles[0], 0, Some(&data)).unwrap();
    session
        .end(EndReason::RadioFailure(Party::Caller), 60)
        .unwrap();
    let stop = session.stop_request(Party::Caller).unwrap().clone();
    session.bearer_stopped(&stop, 70).unwrap();
    assert_eq!(
        bridge.push(&session, &handles[0], 1, Some(&data)),
        Err(BridgeError::InactiveBearer)
    );
}
#[test]
fn unsupported_private_signaling_reports_an_error_without_interrupting_peer_media() {
    use crate::l3::legacy_traffic::TrafficMessageError;
    let (mut session, handles) = session();
    connect(&mut session, &handles);
    let mut bridge = bridge(&session);
    let mut word = SignalingWord::filler();
    word.contents[..2].fill(false);
    word.contents[2..10].fill(true);
    let unknown = TransmitFrame::facch(&word, 10);
    let speech = TransmitFrame::speech(&parameters(20, 37));
    let first = TransmitFrame::interleave(&speech, &unknown);
    let second = TransmitFrame::interleave(&unknown, &speech);
    let third = TransmitFrame::interleave(&speech, &speech);
    bridge.push(&session, &handles[0], 0, Some(&first)).unwrap();
    let output = bridge
        .push(&session, &handles[0], 1, Some(&second))
        .unwrap()
        .unwrap();
    assert_eq!(output.disposition, MediaDisposition::Concealment);
    assert_eq!(
        output.signaling,
        Err(TrafficLinkError::Message(
            TrafficMessageError::UnsupportedType(0xff)
        ))
    );
    let recovered = bridge
        .push(&session, &handles[0], 2, Some(&third))
        .unwrap()
        .unwrap();
    assert_eq!(recovered.disposition, MediaDisposition::Speech);
    assert_eq!(recovered.signaling, Ok(None));
}
