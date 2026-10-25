use bts136::{
    core::{
        delivery::{
            AuxiliaryError, AuxiliaryPacket, AuxiliaryState, AuxiliaryTransport, DeliveryError,
            DeliveryPolicy, SmsDelivery,
        },
        messaging::LocalMessaging,
        registry::ProvisionedSubscriber,
        relay_ids::IdentifierError,
        store::{QueueState, StateStore},
    },
    l3::{
        address::{AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        registration::{AccessMessage, RegistrationType},
        relay::{
            Direction, HigherLayerProtocol, RelayMessage,
            addressing::{AddressedRelay, RelayAddresses},
        },
        sms::{Metadata, SmsMessage, UserData},
    },
    link::rach::{MessageSet, MobileIdentity},
};
const PEER: MobileIdentity = MobileIdentity::Min34(2);
fn dispatcher() -> SmsDelivery {
    let mut store = StateStore::open(":memory:").unwrap();
    store
        .reconcile(&[
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(1),
                number: "1001".into(),
            },
            ProvisionedSubscriber {
                identity: PEER,
                number: "1002".into(),
            },
        ])
        .unwrap();
    let mut core = LocalMessaging::from_store(store).unwrap();
    core.register(
        &MessageSet {
            identity: PEER,
            messages: vec![
                AccessMessage::Registration {
                    registration_type: RegistrationType::PowerUp,
                    scm: 21,
                    protocol_version: ProtocolVersion::Pv1,
                }
                .encode()
                .unwrap(),
            ],
        },
        0,
    )
    .unwrap();
    SmsDelivery::after_restart(
        core,
        DeliveryPolicy {
            response_timeout_ms: 50,
            reuse_guard_ms: 100,
        },
        0,
    )
    .unwrap()
}
fn relay() -> AddressedRelay {
    AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: 77,
            protocol: HigherLayerProtocol::Segmented(bts136::l3::relay::Teleservice::Sms),
            payload: vec![0x18, 0, 1, 0],
        },
        addresses: RelayAddresses::default(),
    }
}
fn prepare(delivery: &mut SmsDelivery, now: u64) -> AuxiliaryPacket {
    delivery
        .prepare_auxiliary(PEER, relay(), AuxiliaryTransport::Control, now)
        .unwrap()
}
fn accept(packet: &AuxiliaryPacket) -> Vec<u8> {
    RelayMessage::Accept {
        transaction_id: packet.transaction_id(),
    }
    .encode(Direction::Reverse)
    .unwrap()
}
#[test]
fn auxiliary_and_sms_ids_share_one_bank_and_auxiliary_acceptance_never_marks_sms_delivered() {
    let mut delivery = dispatcher();
    let payload = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 42,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(b"ordinary SMS".to_vec()),
    }
    .encode()
    .unwrap();
    let stored = delivery
        .core_mut()
        .submit_complete_sms(
            MobileIdentity::Min34(1),
            &AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("1002".into()),
            },
            &payload,
            "ordinary",
            1000,
        )
        .unwrap();
    let sms = delivery.prepare(PEER, 100).unwrap().unwrap();
    let RelayMessage::Data {
        transaction_id: sms_id,
        ..
    } = sms.relay().message
    else {
        panic!("expected data");
    };
    let auxiliary = prepare(&mut delivery, 100);
    assert_ne!(sms_id, auxiliary.transaction_id());
    delivery.auxiliary_submitted(&auxiliary, 101, 101).unwrap();
    delivery
        .auxiliary_transmitted(&auxiliary, 102, 102, 50)
        .unwrap();
    assert_eq!(
        delivery
            .receive_auxiliary(PEER, &accept(&auxiliary), 103)
            .unwrap()
            .unwrap()
            .state,
        AuxiliaryState::Accepted
    );
    assert_eq!(
        delivery.core().store().state(stored.id).unwrap(),
        QueueState::Queued
    );
    let sms_accept = RelayMessage::Accept {
        transaction_id: sms_id,
    }
    .encode(Direction::Reverse)
    .unwrap();
    assert!(
        delivery
            .receive_auxiliary(PEER, &sms_accept, 103)
            .unwrap()
            .is_none()
    );
    delivery.transmitted(&sms, 104).unwrap();
    delivery.receive(PEER, &sms_accept, 105).unwrap();
    assert_eq!(
        delivery.core().store().state(stored.id).unwrap(),
        QueueState::Delivered
    );
}
#[test]
fn response_interval_starts_on_final_rf_completion_and_accept_requires_full_handoff() {
    let mut delivery = dispatcher();
    let packet = prepare(&mut delivery, 100);
    assert!(matches!(
        delivery.receive_auxiliary(PEER, &accept(&packet), 100),
        Err(DeliveryError::Auxiliary(AuxiliaryError::NotSubmitted))
    ));
    assert!(matches!(
        delivery.auxiliary_transmitted(&packet, 100, 100, 50),
        Err(DeliveryError::Auxiliary(AuxiliaryError::NotSubmitted))
    ));
    delivery.auxiliary_submitted(&packet, 100, 100).unwrap();
    assert_eq!(
        delivery.advance_auxiliary(&packet, 200).unwrap(),
        AuxiliaryState::Submitted
    );
    delivery
        .auxiliary_transmitted(&packet, 201, 201, 50)
        .unwrap();
    assert_eq!(
        delivery.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::AwaitingResponse {
            deadline_monotonic_ms: 251
        }
    );
    assert_eq!(
        delivery.advance_auxiliary(&packet, 250).unwrap(),
        AuxiliaryState::AwaitingResponse {
            deadline_monotonic_ms: 251
        }
    );
    assert_eq!(
        delivery.advance_auxiliary(&packet, 251).unwrap(),
        AuxiliaryState::Expired
    );
    assert_eq!(
        delivery
            .receive_auxiliary(PEER, &accept(&packet), 252)
            .unwrap()
            .unwrap()
            .state,
        AuxiliaryState::Expired
    );
}
#[test]
fn peer_proof_after_handoff_can_precede_delayed_local_tx_completion() {
    let mut delivery = dispatcher();
    let packet = prepare(&mut delivery, 100);
    delivery.auxiliary_submitted(&packet, 101, 101).unwrap();
    assert_eq!(
        delivery
            .receive_auxiliary(PEER, &accept(&packet), 102)
            .unwrap()
            .unwrap()
            .state,
        AuxiliaryState::Accepted
    );
    assert!(matches!(
        delivery.auxiliary_transmitted(&packet, 103, 103, 50),
        Err(DeliveryError::Auxiliary(AuxiliaryError::Closed))
    ));
    assert_eq!(
        delivery
            .receive_auxiliary(PEER, &accept(&packet), 104)
            .unwrap()
            .unwrap()
            .state,
        AuxiliaryState::Accepted
    );
    let reject = RelayMessage::Reject {
        transaction_id: packet.transaction_id(),
        cause: 4,
    }
    .encode(Direction::Reverse)
    .unwrap();
    assert!(matches!(
        delivery.receive_auxiliary(PEER, &reject, 104),
        Err(DeliveryError::Auxiliary(
            AuxiliaryError::ConflictingResponse
        ))
    ));
}
#[test]
fn exhausted_ids_reuse_only_after_guard_and_old_generation_callbacks_cannot_modify_replacement() {
    let mut delivery = dispatcher();
    let packets: Vec<_> = (0..=u8::MAX).map(|_| prepare(&mut delivery, 100)).collect();
    assert_eq!(
        packets
            .iter()
            .map(AuxiliaryPacket::transaction_id)
            .collect::<Vec<_>>(),
        (0..=u8::MAX).collect::<Vec<_>>()
    );
    assert!(matches!(
        delivery.prepare_auxiliary(PEER, relay(), AuxiliaryTransport::Control, 100),
        Err(DeliveryError::Identifier(IdentifierError::Exhausted))
    ));
    delivery.cancel_auxiliary_fenced(&packets[0], 101).unwrap();
    assert!(matches!(
        delivery.prepare_auxiliary(PEER, relay(), AuxiliaryTransport::Control, 200),
        Err(DeliveryError::Identifier(IdentifierError::Exhausted))
    ));
    let replacement = prepare(&mut delivery, 201);
    assert_eq!(replacement.transaction_id(), packets[0].transaction_id());
    assert!(matches!(
        delivery.auxiliary_submitted(&packets[0], 201, 201),
        Err(DeliveryError::Auxiliary(AuxiliaryError::StalePacket))
    ));
    assert!(matches!(
        delivery.cancel_auxiliary_fenced(&packets[0], 201),
        Err(DeliveryError::Auxiliary(AuxiliaryError::StalePacket))
    ));
    assert_eq!(
        delivery.auxiliary_state(&replacement).unwrap(),
        AuxiliaryState::Prepared
    );
}
#[test]
fn foreign_owners_wrong_peers_and_unmatched_wire_ids_cannot_complete_an_exchange() {
    let mut delivery = dispatcher();
    let mut other = dispatcher();
    let own = prepare(&mut delivery, 100);
    let foreign = prepare(&mut other, 100);
    assert!(matches!(
        delivery.auxiliary_submitted(&foreign, 100, 100),
        Err(DeliveryError::Auxiliary(AuxiliaryError::StalePacket))
    ));
    delivery.auxiliary_submitted(&own, 101, 101).unwrap();
    assert!(
        delivery
            .receive_auxiliary(MobileIdentity::Min34(1), &accept(&own), 101)
            .unwrap()
            .is_none()
    );
    let wrong_id = RelayMessage::Accept {
        transaction_id: own.transaction_id() + 1,
    }
    .encode(Direction::Reverse)
    .unwrap();
    assert!(
        delivery
            .receive_auxiliary(PEER, &wrong_id, 101)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        delivery.auxiliary_state(&own).unwrap(),
        AuxiliaryState::Submitted
    );
}
#[test]
fn rejected_response_is_terminal_and_keeps_its_identifier_owned() {
    let mut delivery = dispatcher();
    let packet = prepare(&mut delivery, 100);
    delivery.auxiliary_submitted(&packet, 101, 101).unwrap();
    let reject = RelayMessage::Reject {
        transaction_id: packet.transaction_id(),
        cause: 4,
    }
    .encode(Direction::Reverse)
    .unwrap();
    let outcome = delivery
        .receive_auxiliary(PEER, &reject, 102)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.packet.transaction_id(), packet.transaction_id());
    assert_eq!(outcome.state, AuxiliaryState::Rejected { cause: 4 });
    assert!(matches!(
        delivery.cancel_auxiliary_fenced(&packet, 103),
        Err(DeliveryError::Auxiliary(AuxiliaryError::Closed))
    ));
    assert_ne!(
        prepare(&mut delivery, 103).transaction_id(),
        packet.transaction_id()
    );
}
#[test]
fn invalid_payload_does_not_reserve_an_id_and_transport_validation_is_exact() {
    let mut delivery = dispatcher();
    let mut invalid = relay();
    if let RelayMessage::Data {
        ref mut payload, ..
    } = invalid.message
    {
        *payload = vec![0; 255];
    }
    assert!(matches!(
        delivery.prepare_auxiliary(PEER, invalid, AuxiliaryTransport::Traffic, 100),
        Err(DeliveryError::Auxiliary(AuxiliaryError::Wire))
    ));
    let mut traffic_only = relay();
    if let RelayMessage::Data {
        ref mut payload, ..
    } = traffic_only.message
    {
        *payload = vec![0; 254];
    }
    assert!(matches!(
        delivery.prepare_auxiliary(PEER, traffic_only.clone(), AuxiliaryTransport::Control, 100),
        Err(DeliveryError::Auxiliary(AuxiliaryError::Wire))
    ));
    let packet = delivery
        .prepare_auxiliary(PEER, traffic_only, AuxiliaryTransport::Traffic, 100)
        .unwrap();
    assert_eq!(packet.transaction_id(), 0);
    assert_eq!(packet.transport(), AuxiliaryTransport::Traffic);
    assert_eq!(packet.recipient(), PEER);
    assert!(packet.relay().encode(Direction::Forward).is_err());
}
#[test]
fn restart_guard_clock_checks_and_timer_overflow_preserve_active_ownership() {
    let mut delivery = dispatcher();
    assert!(matches!(
        delivery.prepare_auxiliary(PEER, relay(), AuxiliaryTransport::Control, 99),
        Err(DeliveryError::Identifier(IdentifierError::Exhausted))
    ));
    let packet = prepare(&mut delivery, 100);
    assert!(matches!(
        delivery.auxiliary_submitted(&packet, 99, 99),
        Err(DeliveryError::ClockRegression)
    ));
    delivery.auxiliary_submitted(&packet, 100, 100).unwrap();
    assert!(matches!(
        delivery.auxiliary_transmitted(&packet, 100, 100, 0),
        Err(DeliveryError::Auxiliary(AuxiliaryError::TimeoutDuration))
    ));
    assert!(matches!(
        delivery.auxiliary_transmitted(&packet, 100, 100, u64::MAX),
        Err(DeliveryError::Auxiliary(AuxiliaryError::TimeOverflow))
    ));
    assert_eq!(
        delivery.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Submitted
    );
    delivery
        .auxiliary_transmitted(&packet, 101, 101, 50)
        .unwrap();
}

#[test]
fn delayed_completion_receipt_expires_at_the_actual_air_time_and_invalid_receipts_do_not_start_a_timer()
 {
    let mut delivery = dispatcher();
    let packet = prepare(&mut delivery, 100);
    assert!(matches!(
        delivery.auxiliary_submitted(&packet, 99, 100),
        Err(DeliveryError::Auxiliary(AuxiliaryError::ReceiptTime))
    ));
    assert!(matches!(
        delivery.auxiliary_submitted(&packet, 101, 100),
        Err(DeliveryError::Auxiliary(AuxiliaryError::ReceiptTime))
    ));
    delivery.auxiliary_submitted(&packet, 100, 110).unwrap();
    assert!(matches!(
        delivery.auxiliary_transmitted(&packet, 99, 110, 50),
        Err(DeliveryError::Auxiliary(AuxiliaryError::ReceiptTime))
    ));
    assert!(matches!(
        delivery.auxiliary_transmitted(&packet, 111, 110, 50),
        Err(DeliveryError::Auxiliary(AuxiliaryError::ReceiptTime))
    ));
    assert_eq!(
        delivery.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Submitted
    );
    assert_eq!(
        delivery
            .auxiliary_transmitted(&packet, 105, 160, 50)
            .unwrap(),
        AuxiliaryState::Expired
    );
    assert_eq!(
        delivery
            .receive_auxiliary(PEER, &accept(&packet), 161)
            .unwrap()
            .unwrap()
            .state,
        AuxiliaryState::Expired
    );
}
#[test]
fn accepted_and_rejected_ids_remain_reserved_until_explicit_fence_then_full_guard() {
    for rejected in [false, true] {
        let mut delivery = dispatcher();
        let packets: Vec<_> = (0..=u8::MAX).map(|_| prepare(&mut delivery, 100)).collect();
        let packet = &packets[0];
        delivery.auxiliary_submitted(packet, 100, 100).unwrap();
        let response = if rejected {
            RelayMessage::Reject {
                transaction_id: packet.transaction_id(),
                cause: 4,
            }
            .encode(Direction::Reverse)
            .unwrap()
        } else {
            accept(packet)
        };
        let expected = if rejected {
            AuxiliaryState::Rejected { cause: 4 }
        } else {
            AuxiliaryState::Accepted
        };
        assert_eq!(
            delivery
                .receive_auxiliary(PEER, &response, 101)
                .unwrap()
                .unwrap()
                .state,
            expected
        );
        assert!(matches!(
            delivery.prepare_auxiliary(PEER, relay(), AuxiliaryTransport::Control, 500),
            Err(DeliveryError::Identifier(IdentifierError::Exhausted))
        ));
        assert_eq!(delivery.auxiliary_state(packet).unwrap(), expected);
        delivery.retire_auxiliary_fenced(packet, 500).unwrap();
        assert!(matches!(
            delivery.retire_auxiliary_fenced(packet, 500),
            Err(DeliveryError::Auxiliary(AuxiliaryError::AlreadyRetired))
        ));
        assert!(matches!(
            delivery.prepare_auxiliary(PEER, relay(), AuxiliaryTransport::Control, 599),
            Err(DeliveryError::Identifier(IdentifierError::Exhausted))
        ));
        let replacement = prepare(&mut delivery, 600);
        assert_eq!(replacement.transaction_id(), packet.transaction_id());
        assert_eq!(packet, &packet.clone());
        assert_ne!(&replacement, packet);
        assert!(matches!(
            delivery.retire_auxiliary_fenced(packet, 600),
            Err(DeliveryError::Auxiliary(AuxiliaryError::StalePacket))
        ));
        assert_eq!(
            delivery.auxiliary_state(&replacement).unwrap(),
            AuxiliaryState::Prepared
        );
    }
}
#[test]
fn timeout_holds_its_identifier_and_cannot_retire_an_active_or_foreign_exchange() {
    let mut delivery = dispatcher();
    let packets: Vec<_> = (0..=u8::MAX).map(|_| prepare(&mut delivery, 100)).collect();
    let packet = &packets[0];
    assert!(matches!(
        delivery.retire_auxiliary_fenced(packet, 100),
        Err(DeliveryError::Auxiliary(AuxiliaryError::NotTerminal))
    ));
    delivery.auxiliary_submitted(packet, 100, 100).unwrap();
    assert!(matches!(
        delivery.retire_auxiliary_fenced(packet, 100),
        Err(DeliveryError::Auxiliary(AuxiliaryError::NotTerminal))
    ));
    delivery
        .auxiliary_transmitted(packet, 101, 101, 50)
        .unwrap();
    assert!(matches!(
        delivery.retire_auxiliary_fenced(packet, 101),
        Err(DeliveryError::Auxiliary(AuxiliaryError::NotTerminal))
    ));
    assert_eq!(
        delivery.advance_auxiliary(packet, 151).unwrap(),
        AuxiliaryState::Expired
    );
    assert!(matches!(
        delivery.prepare_auxiliary(PEER, relay(), AuxiliaryTransport::Control, 500),
        Err(DeliveryError::Identifier(IdentifierError::Exhausted))
    ));
    let mut foreign = dispatcher();
    let other = prepare(&mut foreign, 100);
    assert!(matches!(
        delivery.retire_auxiliary_fenced(&other, 500),
        Err(DeliveryError::Auxiliary(AuxiliaryError::StalePacket))
    ));
    delivery.retire_auxiliary_fenced(packet, 500).unwrap();
    assert!(matches!(
        delivery.prepare_auxiliary(PEER, relay(), AuxiliaryTransport::Control, 599),
        Err(DeliveryError::Identifier(IdentifierError::Exhausted))
    ));
    assert_eq!(
        prepare(&mut delivery, 600).transaction_id(),
        packet.transaction_id()
    );
}

#[test]
fn retirement_overflow_does_not_release_a_terminal_identifier() {
    let mut delivery = dispatcher();
    let packet = prepare(&mut delivery, 100);
    delivery.auxiliary_submitted(&packet, 100, 100).unwrap();
    delivery
        .receive_auxiliary(PEER, &accept(&packet), 101)
        .unwrap();
    assert!(matches!(
        delivery.retire_auxiliary_fenced(&packet, u64::MAX),
        Err(DeliveryError::Identifier(IdentifierError::TimeOverflow))
    ));
    assert_eq!(
        delivery.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Accepted
    );
    for expected in 1..=u8::MAX {
        assert_eq!(prepare(&mut delivery, u64::MAX).transaction_id(), expected);
    }
    assert!(matches!(
        delivery.prepare_auxiliary(PEER, relay(), AuxiliaryTransport::Control, u64::MAX),
        Err(DeliveryError::Identifier(IdentifierError::Exhausted))
    ));
}
