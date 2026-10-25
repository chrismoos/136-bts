use bts136::{
    core::{
        calls::{CallError, CallReservations},
        registry::{LocalRegistry, ProvisionedSubscriber},
    },
    l3::{
        address::{AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        designation::{self, AssignedSlots, Designation, TimeAlignment},
        origination::Origination,
        page_response::ServiceCode,
        registration::{AccessMessage, RegistrationType},
    },
    link::{
        rach::{self, MessageSet, MobileIdentity, Outcome, RachReceiver},
        spach::{Channel, HeaderFlags, SingleMessage, SpachReceiver},
    },
    phy::{
        channel::Band,
        control::{self, LogicalChannel, SharedFeedback},
        rach::{self as phy_rach, BurstFormat},
        sync::SyncWord,
    },
    resources::TrafficResources,
};

const DVCC: u8 = 42;
const DMAC: u8 = 3;
fn access(format: BurstFormat, identity: MobileIdentity, message: &[u8]) -> MessageSet {
    let mut receiver = RachReceiver::default();
    let mut complete = None;
    for frame in rach::encode(format, identity, &[message]).unwrap() {
        let burst = phy_rach::encode(format, SyncWord::One, &frame, DVCC).unwrap();
        let decoded = phy_rach::decode(&burst, format, DVCC).unwrap();
        if let Outcome::Complete(set) = receiver.push(format, &decoded.block.information).unwrap() {
            complete = Some(set);
        }
    }
    complete.unwrap()
}
#[test]
fn registered_reverse_origination_reserves_both_parties_and_caller_assignment_survives_addressed_arch()
 {
    let caller = MobileIdentity::Min34(1);
    let callee = MobileIdentity::Imsi50(2);
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
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
                .register(&access(format, identity, &registration), 100)
                .unwrap();
        }
        let origination = Origination {
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
        let set = access(format, caller, &origination);
        let resources =
            TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap();
        let mut calls = CallReservations::new(resources);
        let admitted = calls.originate(&registry, &set).unwrap();
        assert_eq!(admitted.caller.identity, caller);
        assert_eq!(admitted.callee.identity, callee);
        assert_eq!(admitted.caller.assigned_slots, AssignedSlots::Full25);
        assert_eq!(admitted.callee.assigned_slots, AssignedSlots::Full36);
        let designation = Designation {
            dvcc: DVCC,
            dmac: DMAC,
            channel: admitted.caller.channel,
            assigned_slots: admitted.caller.assigned_slots,
            shortened_burst: false,
            protocol_version: ProtocolVersion::Is136RevA,
            time_alignment: TimeAlignment::OffsetUnits(0),
            delay_interval_compensation: false,
        };
        let addressed = SingleMessage {
            channel: Channel::Arch,
            flags: HeaderFlags::default(),
            identity: caller,
            message: designation.encode().unwrap(),
        };
        let mut receiver = SpachReceiver::default();
        let mut recovered = None;
        for (phase, information) in addressed.encode_fragmented().unwrap().iter().enumerate() {
            let bits = control::encode_forward(
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
            let decoded = control::decode_forward(&bits, LogicalChannel::Spach, DVCC).unwrap();
            if let Some(message) = receiver.push(&decoded.block.information).unwrap() {
                recovered = Some(message);
            }
        }
        let recovered = recovered.unwrap();
        assert_eq!(recovered.identity, caller);
        assert_eq!(
            designation::decode_prefix(&recovered.message)
                .unwrap()
                .message,
            designation
        );
        assert!(
            matches!(calls.originate(&registry, &set), Err(CallError::Busy(identity)) if identity == caller)
        );
        calls.release(&admitted.handle).unwrap();
        let next = calls.originate(&registry, &set).unwrap();
        assert_eq!(next.caller.assigned_slots, admitted.caller.assigned_slots);
        assert_eq!(next.callee.assigned_slots, admitted.callee.assigned_slots);
        assert_ne!(next.handle.id(), admitted.handle.id());
    }
}

#[test]
fn coded_page_response_hands_off_reserved_bearers_without_timeout_releasing_them() {
    use bts136::{
        core::call_paging::{CallPaging, PagingOutcome},
        l3::page_response::PageResponse,
    };
    let caller = MobileIdentity::Min34(1);
    let callee = MobileIdentity::Imsi50(2);
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
    use bts136::runtime::access::{self, Dispatch};
    let mut paging = CallPaging::new(
        TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
    );
    let registration = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 21,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    for identity in [caller, callee] {
        assert!(matches!(
            access::dispatch(
                &access(BurstFormat::Normal, identity, &registration),
                &mut registry,
                &mut paging,
                100,
                0,
                500,
            )
            .unwrap(),
            Dispatch::Registration(_)
        ));
    }
    let origin = Origination {
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

    let Dispatch::Origination(page) = access::dispatch(
        &access(BurstFormat::Normal, caller, &origin),
        &mut registry,
        &mut paging,
        100,
        1000,
        500,
    )
    .unwrap() else {
        panic!("expected paging request");
    };
    assert_eq!(page.recipient, callee);
    paging.transmitted(&page.handle, 1100).unwrap();
    let response = PageResponse {
        protocol_version: ProtocolVersion::Is136RevA,
        last_try: false,
        scm: 21,
        service_code: ServiceCode::DigitalSpeech,
    }
    .encode()
    .unwrap();
    let Dispatch::PageResponse(PagingOutcome::Ready { reservation, .. }) = access::dispatch(
        &access(BurstFormat::Abbreviated, callee, &response),
        &mut registry,
        &mut paging,
        100,
        1200,
        500,
    )
    .unwrap() else {
        panic!("page response must hand off the call")
    };
    for party in [&reservation.caller, &reservation.callee] {
        let designation = Designation {
            dvcc: DVCC,
            dmac: DMAC,
            channel: party.channel,
            assigned_slots: party.assigned_slots,
            shortened_burst: false,
            protocol_version: ProtocolVersion::Is136RevA,
            time_alignment: TimeAlignment::OffsetUnits(0),
            delay_interval_compensation: false,
        };
        let addressed = SingleMessage {
            channel: Channel::Arch,
            flags: HeaderFlags::default(),
            identity: party.identity,
            message: designation.encode().unwrap(),
        };
        let mut receiver = SpachReceiver::default();
        let mut recovered = None;
        for (phase, info) in addressed.encode_fragmented().unwrap().iter().enumerate() {
            let coded = control::encode_forward(
                SyncWord::One,
                phase as u8,
                &SharedFeedback {
                    leading: [false; 12],
                    trailing: [false; 10],
                },
                info,
                LogicalChannel::Spach,
                DVCC,
            )
            .unwrap();
            let decoded = control::decode_forward(&coded, LogicalChannel::Spach, DVCC).unwrap();
            if let Some(message) = receiver.push(&decoded.block.information).unwrap() {
                recovered = Some(message);
            }
        }
        let recovered = recovered.unwrap();
        assert_eq!(recovered.identity, party.identity);
        assert_eq!(
            designation::decode_prefix(&recovered.message)
                .unwrap()
                .message,
            designation
        );
    }
    assert!(paging.advance(2000).unwrap().is_empty());
    assert_eq!(paging.reservations().count(), 1);
    paging.release_stopped(&reservation.handle).unwrap();
    assert_eq!(paging.reservations().count(), 0);
}
