use bts136::{
    codec::vselp::{SUBFRAMES, VselpParameters, VselpSubframe},
    core::{
        call_session::{BearerHandle, CallSession, Party, SessionPolicy},
        calls::CallReservations,
        registry::{LocalRegistry, ProvisionedSubscriber},
    },
    l3::{
        broadcast::ProtocolVersion,
        designation::AssignedSlots,
        legacy_traffic::TrafficMessage,
        registration::{AccessMessage, RegistrationType},
    },
    link::{
        rach::{MessageSet, MobileIdentity},
        traffic::{self, SignalingWord, TrafficMessageReceiver},
        traffic_order::OrderPolicy,
        voice::{FrameKind, TrafficFrameReceiver, TransmitFrame},
    },
    media::vselp_bridge::{LegConfig, MediaDisposition, VselpBridge},
    phy::{
        acquisition::decode_forward_traffic, channel::Band, forward::ForwardModulator,
        speech::CrcStatus, sync::SyncWord, traffic as traffic_phy,
    },
    resources::TrafficResources,
};
const CALLER_DVCC: u8 = 10;
const CALLEE_DVCC: u8 = 42;
const SAMPLES_PER_SYMBOL: usize = 8;
fn parameters(r0: u8, code: u8) -> VselpParameters {
    VselpParameters::try_new(
        r0,
        [32, 16, 16, 8, 8, 4, 4, 4, 4, 2],
        [VselpSubframe::try_new(21, code, code ^ 0x7f, 170).unwrap(); SUBFRAMES],
    )
    .unwrap()
}
fn connected() -> (CallSession, [BearerHandle; 2]) {
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
    for handle in &handles {
        session.bearer_confirmed(handle, 20).unwrap();
    }
    let alert = session.order(Party::Callee).unwrap().clone();
    session.order_transmitted(&handles[1], &alert, 30).unwrap();
    session
        .receive(&handles[1], &TrafficMessage::Connect, 40)
        .unwrap();
    let ack = session.response(Party::Callee).unwrap().clone();
    session.response_transmitted(&ack, 50).unwrap();
    (session, handles)
}
#[test]
fn both_bridge_directions_survive_forward_iq_with_exact_speech_and_private_signaling_isolation() {
    let (session, handles) = connected();
    let mut bridge = VselpBridge::new(
        &session,
        LegConfig {
            dvcc: CALLER_DVCC,
            preceding: VselpParameters::default(),
        },
        LegConfig {
            dvcc: CALLEE_DVCC,
            preceding: VselpParameters::default(),
        },
    );
    let padding = TransmitFrame::speech(&VselpParameters::default());
    for (source, handle, dvcc, peer_dvcc, code) in [
        (Party::Caller, &handles[0], CALLER_DVCC, CALLEE_DVCC, 37),
        (Party::Callee, &handles[1], CALLEE_DVCC, CALLER_DVCC, 54),
    ] {
        let first = parameters(20, code);
        let second = parameters(21, code ^ 7);
        let third = parameters(22, code ^ 3);
        let fourth = parameters(19, code ^ 1);
        let private = TrafficMessage::Connect;
        let private_word = traffic::encode(&private).unwrap().remove(0);
        let frames = [
            TransmitFrame::speech(&first),
            TransmitFrame::speech(&second),
            TransmitFrame::facch(&private_word, dvcc),
            TransmitFrame::speech(&third),
            TransmitFrame::speech(&fourth),
        ];
        let mut previous = padding.clone();
        let mut outputs = Vec::new();
        let mut messages = Vec::new();
        for (interval, current) in frames.iter().chain(std::iter::once(&padding)).enumerate() {
            let data = TransmitFrame::interleave(&previous, current);
            if let Some(output) = bridge
                .push(&session, handle, interval as u64, Some(&data))
                .unwrap()
            {
                assert_eq!(output.source, source);
                assert_eq!(
                    output.destination,
                    if source == Party::Caller {
                        Party::Callee
                    } else {
                        Party::Caller
                    }
                );
                if let Some(message) = output.signaling.clone().unwrap() {
                    messages.push(message);
                }
                outputs.push(output);
            }
            previous = current.clone();
        }
        assert_eq!(outputs.len(), 5);
        assert_eq!(messages, [private]);
        assert_eq!(
            outputs
                .iter()
                .map(|output| output.disposition)
                .collect::<Vec<_>>(),
            [
                MediaDisposition::Speech,
                MediaDisposition::Speech,
                MediaDisposition::Concealment,
                MediaDisposition::Speech,
                MediaDisposition::Speech
            ]
        );
        let mut modulator = ForwardModulator::try_new(SAMPLES_PER_SYMBOL).unwrap();
        let mut samples = Vec::new();
        let mut previous = padding.clone();
        for current in outputs
            .iter()
            .map(|output| &output.transmit)
            .chain(std::iter::once(&padding))
        {
            let data = TransmitFrame::interleave(&previous, current);
            let slot = traffic_phy::encode_forward(
                SyncWord::One,
                &[false; traffic_phy::SACCH_BITS],
                &data,
                peer_dvcc,
                0,
            )
            .unwrap();
            samples.extend(modulator.encode_slot(&slot));
            previous = current.clone();
        }
        samples.extend(modulator.finish());
        let acquired = decode_forward_traffic(&samples, modulator.sample_rate_hz()).unwrap();
        assert_eq!(acquired.len(), outputs.len() + 1);
        let mut receiver = TrafficFrameReceiver::new(peer_dvcc, VselpParameters::default());
        let mut peer_signaling = TrafficMessageReceiver::default();
        assert!(
            receiver
                .push(Some(&acquired[0].fields.fields.data))
                .unwrap()
                .is_none()
        );
        for (burst, output) in acquired[1..].iter().zip(&outputs) {
            let received = receiver
                .push(Some(&burst.fields.fields.data))
                .unwrap()
                .unwrap();
            assert_eq!(received.samples, output.received.samples);
            match received.kind {
                FrameKind::Speech {
                    crc: CrcStatus::Valid,
                    ..
                } => assert_eq!(received.parameters, output.received.parameters),
                FrameKind::Facch(word) => {
                    assert_eq!(output.disposition, MediaDisposition::Concealment);
                    assert_eq!(SignalingWord::from(&word), SignalingWord::filler());
                    assert!(
                        peer_signaling
                            .push(&SignalingWord::from(&word))
                            .unwrap()
                            .is_none()
                    );
                }
                other => panic!("unexpected peer frame: {other:?}"),
            }
        }
    }
}
