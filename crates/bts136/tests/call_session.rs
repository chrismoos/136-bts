use bts136::{
    core::{
        call_paging::{CallPaging, PagingError, PagingOutcome},
        call_session::{CallSession, CallState, EndReason, Party, SessionPolicy},
        calls::CallError,
        registry::{LocalRegistry, ProvisionedSubscriber},
    },
    l3::{
        address::{AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        designation::{self, AssignedSlots, Designation, TimeAlignment},
        legacy_traffic::TrafficMessage,
        origination::Origination,
        page_response::{PageResponse, ServiceCode},
        registration::{AccessMessage, RegistrationType},
    },
    link::{
        rach::{self, MessageSet, MobileIdentity, Outcome, RachReceiver},
        spach::{Channel, HeaderFlags, SingleMessage, SpachReceiver},
        traffic::{self, SignalingWord, TrafficMessageReceiver},
        traffic_order::OrderPolicy,
    },
    phy::{
        channel::Band,
        control::{self, LogicalChannel, SharedFeedback},
        facch,
        rach::{self as phy_rach, BurstFormat},
        sync::SyncWord,
    },
    resources::TrafficResources,
};
const DVCC: u8 = 42;
const DMAC: u8 = 3;
fn access(identity: MobileIdentity, message: &[u8]) -> MessageSet {
    let format = BurstFormat::Normal;
    let mut receiver = RachReceiver::default();
    let mut complete = None;
    for frame in rach::encode(format, identity, &[message]).unwrap() {
        let coded = phy_rach::encode(format, SyncWord::One, &frame, DVCC).unwrap();
        let decoded = phy_rach::decode(&coded, format, DVCC).unwrap();
        if let Outcome::Complete(set) = receiver.push(format, &decoded.block.information).unwrap() {
            complete = Some(set);
        }
    }
    complete.unwrap()
}
fn via_facch(message: &TrafficMessage) -> TrafficMessage {
    via_facch_in(message, bts136::l3::relay::Direction::Reverse)
}
fn via_facch_in(
    message: &TrafficMessage,
    direction: bts136::l3::relay::Direction,
) -> TrafficMessage {
    let mut receiver = TrafficMessageReceiver::new(direction);
    let mut complete = None;
    for word in traffic::encode(message).unwrap() {
        let coded = facch::encode(word.continuation, &word.contents, DVCC);
        let decoded = facch::decode(&coded, DVCC).unwrap();
        if let Some(message) = receiver.push(&SignalingWord::from(&decoded)).unwrap() {
            complete = Some(message);
        }
    }
    complete.unwrap()
}
fn assignment(identity: MobileIdentity, designation: Designation) {
    let addressed = SingleMessage {
        channel: Channel::Arch,
        flags: HeaderFlags::default(),
        identity,
        message: designation.encode().unwrap(),
    };
    let mut receiver = SpachReceiver::default();
    let mut complete = None;
    for (phase, information) in addressed.encode_fragmented().unwrap().iter().enumerate() {
        let coded = control::encode_forward(
            SyncWord::One,
            phase as u8,
            &SharedFeedback {
                leading: [false; 12],
                trailing: [false; 10],
            },
            information,
            LogicalChannel::Spach,
            DVCC,
        )
        .unwrap();
        let decoded = control::decode_forward(&coded, LogicalChannel::Spach, DVCC).unwrap();
        if let Some(message) = receiver.push(&decoded.block.information).unwrap() {
            complete = Some(message);
        }
    }
    let received = complete.unwrap();
    assert_eq!(received.identity, identity);
    assert_eq!(
        designation::decode_prefix(&received.message)
            .unwrap()
            .message,
        designation
    );
}
#[test]
fn coded_access_assignment_and_facch_call_progress_release_resources_only_after_both_stop_reports()
{
    let caller = MobileIdentity::Min34(1);
    let callee = MobileIdentity::Imsi50(2);
    for origin in [Party::Caller, Party::Callee] {
        let mut registry = LocalRegistry::try_new(vec![
            ProvisionedSubscriber {
                identity: caller,
                number: "1001".into(),
            },
            ProvisionedSubscriber {
                identity: callee,
                number: "1002".into(),
            },
        ])
        .unwrap();
        let registration = AccessMessage::Registration {
            registration_type: RegistrationType::PowerUp,
            scm: 21,
            protocol_version: ProtocolVersion::Is136RevA,
        }
        .encode()
        .unwrap();
        for identity in [caller, callee] {
            registry
                .register(&access(identity, &registration), 100)
                .unwrap();
        }
        let origin_message = Origination {
            protocol_version: ProtocolVersion::Is136RevA,
            emergency_call: false,
            called_party: AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("1002".into()),
            },
            last_try: false,
            scm: 21,
            service_code: ServiceCode::DigitalSpeech,
        }
        .encode()
        .unwrap();
        let set = access(caller, &origin_message);
        let mut paging = CallPaging::new(
            TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
        );
        let request = paging.begin(&registry, &set, 0, 500).unwrap();
        paging.transmitted(&request.handle, 10).unwrap();
        let response = PageResponse {
            protocol_version: ProtocolVersion::Is136RevA,
            last_try: false,
            scm: 21,
            service_code: ServiceCode::DigitalSpeech,
        }
        .encode()
        .unwrap();
        let PagingOutcome::Ready { reservation, .. } = paging
            .response(&request.handle, &access(callee, &response), 20)
            .unwrap()
        else {
            panic!("callee response must hand off the reservation")
        };
        let order_policy = OrderPolicy {
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
                alert: order_policy,
                release: order_policy,
                measurements: order_policy,
                status: order_policy,
                capabilities: order_policy,
                physical_control: order_policy,
            },
            20,
        )
        .unwrap();
        let assignments = [
            session.assignment(Party::Caller).unwrap(),
            session.assignment(Party::Callee).unwrap(),
        ];
        for pending in &assignments {
            session.assignment_started(&pending.handle, 30).unwrap();
        }
        for pending in &assignments {
            assignment(
                pending.subscriber.identity,
                Designation {
                    dvcc: DVCC,
                    dmac: DMAC,
                    channel: pending.subscriber.channel,
                    assigned_slots: pending.subscriber.assigned_slots,
                    shortened_burst: false,
                    protocol_version: ProtocolVersion::Is136RevA,
                    time_alignment: TimeAlignment::OffsetUnits(0),
                    delay_interval_compensation: false,
                },
            );
            session.bearer_confirmed(&pending.handle, 40).unwrap();
        }
        let alert = session.order(Party::Callee).unwrap().clone();
        let received_alert = via_facch_in(alert.message(), bts136::l3::relay::Direction::Forward);
        assert!(matches!(
            received_alert,
            TrafficMessage::AlertWithInfo { .. }
        ));
        session
            .order_transmitted(&assignments[1].handle, &alert, 50)
            .unwrap();
        let mobile_ack = via_facch(&TrafficMessage::Ack {
            ack_message_type: received_alert.message_type(),
            information: Default::default(),
        });
        session
            .receive(&assignments[1].handle, &mobile_ack, 60)
            .unwrap();
        assert_eq!(session.state(), CallState::WaitingAnswer);
        session
            .receive(
                &assignments[1].handle,
                &via_facch(&TrafficMessage::Connect),
                70,
            )
            .unwrap();
        assert!(!session.media_ready());
        let ack = session.response(Party::Callee).unwrap().clone();
        assert_eq!(
            via_facch(ack.message()),
            TrafficMessage::Ack {
                ack_message_type: 0xc1,
                information: Default::default()
            }
        );
        session.response_transmitted(&ack, 80).unwrap();
        assert!(session.media_ready());
        let release = via_facch(&TrafficMessage::Release {
            information: Default::default(),
        });
        let origin_index = if origin == Party::Caller { 0 } else { 1 };
        session
            .receive(&assignments[origin_index].handle, &release, 100)
            .unwrap();
        assert!(!session.media_ready());
        let ack = session.response(origin).unwrap().clone();
        assert_eq!(
            via_facch(ack.message()),
            TrafficMessage::Ack {
                ack_message_type: 0xda,
                information: Default::default()
            }
        );
        session.response_transmitted(&ack, 110).unwrap();
        let other = if origin == Party::Caller {
            Party::Callee
        } else {
            Party::Caller
        };
        let other_index = 1 - origin_index;
        let release = session.order(other).unwrap().clone();
        let received = via_facch(release.message());
        session
            .order_transmitted(&assignments[other_index].handle, &release, 110)
            .unwrap();
        let ack = via_facch(&TrafficMessage::Ack {
            ack_message_type: received.message_type(),
            information: Default::default(),
        });
        session
            .receive(&assignments[other_index].handle, &ack, 120)
            .unwrap();
        assert!(session.stopped_handle().is_none());
        let caller_stop = session.stop_request(Party::Caller).unwrap().clone();
        let callee_stop = session.stop_request(Party::Callee).unwrap().clone();
        session.bearer_stopped(&caller_stop, 130).unwrap();
        assert!(session.stopped_handle().is_none());
        assert!(matches!(
            paging.begin(&registry, &set, 130, 500),
            Err(PagingError::Call(CallError::Busy(_)))
        ));
        session.bearer_stopped(&callee_stop, 140).unwrap();
        assert_eq!(
            session.state(),
            CallState::Stopped(EndReason::MobileRelease(origin))
        );
        paging
            .release_stopped(session.stopped_handle().unwrap())
            .unwrap();
        assert_eq!(paging.reservations().count(), 0);
        assert!(paging.begin(&registry, &set, 140, 500).is_ok());
    }
}
