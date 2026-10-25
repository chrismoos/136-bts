pub(super) mod auxiliary;
mod incoming;
mod ordinary_restart;
use super::*;
use crate::{
    core::{registry::ProvisionedSubscriber, store::StateStore},
    l3::{
        address::{AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        registration::{AccessMessage, RegistrationType},
        relay::{
            HigherLayerProtocol,
            addressing::{AddressedRelay, RelayAddresses},
        },
        sms::{Metadata, UserData},
    },
    link::spach::SpachReceiver,
};
pub(super) fn runtime() -> SmsRuntime {
    runtime_from(StateStore::open(":memory:").unwrap())
}
fn runtime_from(mut store: StateStore) -> SmsRuntime {
    store
        .reconcile(&[
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(1),
                number: "1001".into(),
            },
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(2),
                number: "1002".into(),
            },
        ])
        .unwrap();
    let core = LocalMessaging::from_store(store).unwrap();
    let config = crate::config::Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) =
        crate::runtime::serving::messages(&config)
            .into_iter()
            .next()
            .unwrap()
    else {
        panic!()
    };
    let mut runtime = SmsRuntime::try_new(
        core,
        PagingAllocation::try_new(&structure).unwrap(),
        "session".into(),
    )
    .unwrap();
    let crate::l3::broadcast::MandatoryBroadcast::AccessParameters(parameters) =
        &crate::runtime::serving::messages(&config)[1]
    else {
        panic!()
    };
    runtime
        .set_access_parameters(parameters, super::super::timers::SUPERFRAME_MS)
        .unwrap();
    for id in [1, 2] {
        runtime
            .core_mut()
            .register(
                &MessageSet {
                    identity: MobileIdentity::Min34(id),
                    messages: vec![
                        AccessMessage::Registration {
                            registration_type: RegistrationType::PowerUp,
                            scm: 15,
                            protocol_version: ProtocolVersion::Is136RevA,
                        }
                        .encode()
                        .unwrap(),
                    ],
                },
                0,
            )
            .unwrap();
    }
    runtime
}
fn submission() -> MessageSet {
    let pdu = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 12,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(b"hello handset".to_vec()),
    }
    .encode()
    .unwrap();
    let message = AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: 4,
            protocol: HigherLayerProtocol::Sms,
            payload: pdu,
        },
        addresses: RelayAddresses {
            destination: Some(AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("1002".into()),
            }),
            ..Default::default()
        },
    }
    .encode(Direction::Reverse)
    .unwrap();
    MessageSet {
        identity: MobileIdentity::Min34(1),
        messages: vec![message],
    }
}
#[test]
fn notification_confirmation_and_relay_accept_gate_sms_delivery() {
    const MEMORY_CAPACITY_EXCEEDED: u8 = 22;
    let mut runtime = runtime();
    let serial = AccessMessage::SerialNumber { esn: 1 }.encode().unwrap();
    let mut origin = submission();
    origin.messages.push(serial.clone());
    runtime.receive(&origin, 0, 0).unwrap();
    runtime.receive(&origin, 1, 1).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
    while runtime.frame(0, 0).is_some() {
        assert!(matches!(runtime.submitted().unwrap(), Completion::None));
    }
    runtime
        .advance(super::super::timers::SMS_ORIGINATION_MS)
        .unwrap();
    let identity = MobileIdentity::Min34(2);
    let phase = runtime.allocation.phase(identity).unwrap();
    let confirmation = MessageSet {
        identity,
        messages: vec![vec![0x0f, 0x68], serial],
    };
    assert!(runtime.receive(&confirmation, 12800, 0).is_err());
    let notification = *runtime.frame(20, phase).unwrap();
    let Completion::Notification {
        identity,
        generation,
    } = runtime.submitted().unwrap()
    else {
        panic!()
    };
    assert!(runtime.receive(&confirmation, 12800, 0).is_err());
    runtime.notification_transmitted(identity, generation);
    runtime.receive(&confirmation, 12801, 0).unwrap();
    assert_eq!(*runtime.frame(21, phase).unwrap(), notification);
    assert!(matches!(runtime.submitted().unwrap(), Completion::None));
    let mut receiver = SpachReceiver::default();
    let mut decoded = None;
    let mut delivery = None;
    while let Some(frame) = runtime.frame(21, (phase + 1) % 32).copied() {
        if let Some(message) = receiver.push(&frame).unwrap() {
            decoded = Some(message)
        }
        if let Completion::Delivery(prepared) = runtime.submitted().unwrap() {
            delivery = Some(prepared)
        }
    }
    let decoded = decoded.unwrap();
    assert_eq!(decoded.channel, Channel::Smsch);
    let relay = relay::addressing::decode(&decoded.message, Direction::Forward).unwrap();
    let RelayMessage::Data {
        transaction_id,
        payload,
        ..
    } = relay.relay.message
    else {
        panic!()
    };
    let SmsMessage::Deliver {
        user_data: UserData::Ira(text),
        ..
    } = sms::decode_prefix(&payload, Direction::Forward)
        .unwrap()
        .message
    else {
        panic!()
    };
    assert_eq!(text, b"hello handset");
    let accepted = MessageSet {
        identity,
        messages: vec![
            RelayMessage::Accept { transaction_id }
                .encode(Direction::Reverse)
                .unwrap(),
        ],
    };
    assert!(runtime.receive(&accepted, 12802, 0).is_err());
    runtime.transmitted(&delivery.unwrap(), 12803).unwrap();
    let view = runtime.operation_messages(10).unwrap();
    assert!(view[0].measurements.iter().any(|metric| {
        metric.name == "Relay attempt"
            && metric
                .value
                .as_deref()
                .is_some_and(|value| value.contains("AwaitingResponse"))
    }));
    assert!(
        view[0]
            .measurements
            .iter()
            .any(|metric| metric.name == "Attempts since startup"
                && metric.value.as_deref() == Some("1"))
    );
    assert!(
        view[0]
            .measurements
            .iter()
            .any(|metric| metric.name == "Response deadline" && metric.value.is_some())
    );
    for reply in [
        RelayMessage::Accept { transaction_id },
        RelayMessage::Reject {
            transaction_id,
            cause: MEMORY_CAPACITY_EXCEEDED,
        },
    ] {
        let invalid = MessageSet {
            identity,
            messages: vec![
                reply.encode(Direction::Reverse).unwrap(),
                AccessMessage::SerialNumber { esn: 1 }.encode().unwrap(),
            ],
        };
        assert!(runtime.receive(&invalid, 12804, 0).is_err());
        assert_eq!(
            runtime.core().store().pending_for(identity).unwrap().len(),
            1
        );
        assert_eq!(runtime.operation_messages(10).unwrap(), view);
    }
    runtime.receive(&accepted, 12805, 0).unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_for(identity)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unconfirmed_notification_expires_without_marking_delivery_and_shutdown_keeps_queue() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    while runtime.frame(0, 0).is_some() {
        runtime.submitted().unwrap();
    }
    runtime.advance(12800).unwrap();
    let identity = MobileIdentity::Min34(2);
    let phase = runtime.allocation.phase(identity).unwrap();
    runtime.frame(20, phase).unwrap();
    let token = runtime.submitted().unwrap();
    let Completion::Notification {
        identity,
        generation,
    } = token
    else {
        panic!()
    };
    runtime.advance(19200).unwrap();
    runtime.notification_transmitted(identity, generation);
    assert!(
        runtime
            .receive(
                &MessageSet {
                    identity,
                    messages: vec![vec![15, 104]]
                },
                19201,
                0
            )
            .is_err()
    );
    assert_eq!(
        runtime.core().store().pending_for(identity).unwrap().len(),
        1
    );
    assert!(runtime.advance(19199).is_err());
    runtime.shutdown(19202).unwrap();
    assert_eq!(
        runtime.core().store().pending_for(identity).unwrap().len(),
        1
    );
}

#[test]
fn malformed_mixed_procedure_does_not_store_a_submission() {
    let mut runtime = runtime();
    let mut request = submission();
    request.messages.push(vec![12]);
    assert!(runtime.receive(&request, 0, 0).is_err());
    assert!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn retry_after_the_mobile_origination_timeout_does_not_store_a_second_sms() {
    let mut runtime = runtime();
    let request = submission();
    runtime.receive(&request, 0, 0).unwrap();
    let retry_at = super::super::timers::SMS_ORIGINATION_MS + super::super::timers::SUPERFRAME_MS;
    runtime.advance(retry_at).unwrap();
    runtime.receive(&request, retry_at, 1).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn invalid_concatenated_sms_primaries_do_not_store_the_first_submission() {
    let mut runtime = runtime();
    let mut set = submission();
    set.messages.push(
        RelayMessage::Accept { transaction_id: 4 }
            .encode(Direction::Reverse)
            .unwrap(),
    );
    assert!(runtime.receive(&set, 1, 1000).is_err());
    assert!(
        runtime
            .core()
            .deliveries_for(MobileIdentity::Min34(2))
            .unwrap()
            .is_empty()
    );
    assert!(runtime.operation_messages(10).unwrap().is_empty());
}

fn peer(identity: u64, call_id: u64, ready: bool) -> TrafficPeer {
    TrafficPeer {
        identity: MobileIdentity::Min34(identity),
        call_id,
        ready,
    }
}
fn traffic_submission() -> crate::l3::legacy_traffic::TrafficRelay {
    crate::l3::legacy_traffic::TrafficRelay::from_control(
        &submission().messages[0],
        Direction::Reverse,
    )
    .unwrap()
}
fn acceptance(offer: &TrafficOffer) -> crate::l3::legacy_traffic::TrafficRelay {
    let crate::l3::legacy_traffic::TrafficMessage::Relay(relay) = &offer.message else {
        panic!()
    };
    let RelayMessage::Data { transaction_id, .. } = relay.message else {
        panic!()
    };
    crate::l3::legacy_traffic::TrafficRelay {
        message: RelayMessage::Accept { transaction_id },
        optional_octets: vec![],
    }
}
#[test]
fn traffic_sms_waits_for_bearer_readiness_and_final_completion_then_uses_sacch_timeout() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    let mut callee = peer(2, 7, false);
    runtime.sync_traffic(vec![caller, callee], 0).unwrap();
    runtime
        .receive_traffic(caller, &traffic_submission(), 0, 0)
        .unwrap();
    let reply = runtime.traffic_offer(caller, RelayChannel::Facch).unwrap();
    assert!(matches!(
        reply.message,
        crate::l3::legacy_traffic::TrafficMessage::Relay(crate::l3::legacy_traffic::TrafficRelay {
            message: RelayMessage::Accept { transaction_id: 4 },
            ..
        })
    ));
    runtime.traffic_queued(reply.token).unwrap();
    assert!(runtime.traffic_transmitted(reply.token, 40).unwrap());
    runtime.advance(12800).unwrap();
    assert!(runtime.traffic_offer(callee, RelayChannel::Sacch).is_none());
    callee.ready = true;
    runtime.sync_traffic(vec![caller, callee], 12801).unwrap();
    runtime.advance(13440).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    let accepted = acceptance(&offer);
    assert!(
        runtime
            .receive_traffic(callee, &accepted, 13441, 0)
            .is_err()
    );
    runtime.traffic_queued(offer.token).unwrap();
    assert!(
        runtime
            .receive_traffic(callee, &accepted, 13442, 0)
            .is_err()
    );
    assert!(runtime.traffic_transmitted(offer.token, 14440).unwrap());
    let view = runtime.operation_messages(10).unwrap();
    assert!(view[0].measurements.iter().any(
        |metric| metric.name == "Response deadline" && metric.value.as_deref() == Some("15640")
    ));
    runtime
        .receive_traffic(callee, &accepted, 15639, 0)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .is_empty()
    );
    assert!(!runtime.traffic_transmitted(offer.token, 15640).unwrap());
}
#[test]
fn traffic_call_replacement_invalidates_old_completions_and_responses_without_losing_content() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 1).unwrap();
    runtime.advance(12800).unwrap();
    let old = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(old.token).unwrap();
    let replacement = peer(2, 8, true);
    runtime.sync_traffic(vec![replacement], 12801).unwrap();
    assert!(!runtime.traffic_transmitted(old.token, 12802).unwrap());
    runtime.advance(12802).unwrap();
    let new = runtime
        .traffic_offer(replacement, RelayChannel::Sacch)
        .unwrap();
    assert_ne!(old.token, new.token);
    assert_ne!(acceptance(&old), acceptance(&new));
    runtime.traffic_queued(new.token).unwrap();
    assert!(runtime.traffic_transmitted(new.token, 13800).unwrap());
    assert!(
        runtime
            .receive_traffic(callee, &acceptance(&old), 13801, 0)
            .is_err()
    );
    assert!(
        runtime
            .receive_traffic(replacement, &acceptance(&old), 13802, 0)
            .is_err()
    );
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(replacement.identity)
            .unwrap()
            .len(),
        1
    );
    runtime
        .receive_traffic(replacement, &acceptance(&new), 13803, 0)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_for(replacement.identity)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn traffic_timeout_boundary_and_interruption_keep_the_durable_message_for_retry() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 1).unwrap();
    runtime.advance(12800).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    runtime.traffic_transmitted(offer.token, 13800).unwrap();
    assert!(
        runtime
            .receive_traffic(callee, &acceptance(&offer), 15000, 0)
            .is_err()
    );
    runtime.advance(15000).unwrap();
    let retry = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(retry.token).unwrap();
    runtime.traffic_interrupted(retry.token, 15001).unwrap();
    assert!(!runtime.traffic_transmitted(retry.token, 15002).unwrap());
    runtime.sync_traffic(vec![], 15003).unwrap();
    runtime.advance(15640).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .len(),
        1
    );
    assert!(
        runtime.pending.iter().any(
            |pending| pending.identity == callee.identity && pending.owner.delivery().is_some()
        )
    );
}
#[test]
fn traffic_and_control_retries_share_submission_history() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    runtime.sync_traffic(vec![caller], 0).unwrap();
    runtime
        .receive_traffic(caller, &traffic_submission(), 1, 0)
        .unwrap();
    runtime
        .receive_traffic(caller, &traffic_submission(), 2, 1)
        .unwrap();
    runtime.receive(&submission(), 3, 2).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(runtime.traffic.pending.len(), 2);
    assert_eq!(runtime.pending.len(), 1);
}

#[test]
fn traffic_rejects_unsupported_protocol_and_invalid_sms_without_storing() {
    use crate::l3::legacy_traffic::{TrafficMessage, TrafficRelay};
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    runtime.sync_traffic(vec![caller], 0).unwrap();
    for (index, (protocol, payload, cause)) in [
        (HigherLayerProtocol::NetworkSpecific, vec![1], 4),
        (
            HigherLayerProtocol::Segmented(crate::l3::relay::Teleservice::Sms),
            vec![1],
            4,
        ),
        (HigherLayerProtocol::Guts, vec![1], 4),
        (HigherLayerProtocol::Ghost, vec![1], 4),
        (HigherLayerProtocol::Sms, vec![0xff], 95),
        (HigherLayerProtocol::Sms, vec![0; 254], 95),
    ]
    .into_iter()
    .enumerate()
    {
        let request = TrafficRelay {
            message: RelayMessage::Data {
                transaction_id: 7,
                protocol,
                payload,
            },
            optional_octets: vec![],
        };
        runtime
            .receive_traffic(caller, &request, index as u64 * 2 + 1, 0)
            .unwrap();
        let offer = runtime.traffic_offer(caller, RelayChannel::Facch).unwrap();
        assert_eq!(
            offer.message,
            TrafficMessage::Relay(TrafficRelay {
                message: RelayMessage::Reject {
                    transaction_id: 7,
                    cause
                },
                optional_octets: vec![],
            })
        );
        runtime.traffic_queued(offer.token).unwrap();
        runtime
            .traffic_transmitted(offer.token, index as u64 * 2 + 2)
            .unwrap();
    }
    assert!(runtime.operation_messages(10).unwrap().is_empty());
}
#[test]
fn expired_traffic_completion_and_late_dcch_completion_cannot_start_a_replacement_attempt() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    runtime.advance(12800).unwrap();
    let identity = MobileIdentity::Min34(2);
    let dcch = runtime
        .pending
        .iter()
        .find_map(|pending| pending.owner.delivery().cloned())
        .unwrap();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 12801).unwrap();
    runtime.advance(12801).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    assert!(runtime.transmitted(&dcch, 12802).is_err());
    assert!(
        runtime
            .receive_traffic(callee, &acceptance(&offer), 12803, 0)
            .is_err()
    );
    runtime.traffic_queued(offer.token).unwrap();
    assert!(!runtime.traffic_transmitted(offer.token, 19000).unwrap());
    assert!(!runtime.delivery.busy(identity));
    assert_eq!(
        runtime.core().store().pending_for(identity).unwrap().len(),
        1
    );
}

#[test]
fn traffic_completion_after_deregistration_keeps_content_without_stopping_the_cell() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 1).unwrap();
    runtime.advance(12800).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    runtime
        .core_mut()
        .register(
            &MessageSet {
                identity: callee.identity,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerDown,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            20,
        )
        .unwrap();
    assert!(!runtime.traffic_transmitted(offer.token, 13800).unwrap());
    assert!(!runtime.delivery.busy(callee.identity));
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .len(),
        1
    );
}

fn long_submission(characters: usize) -> crate::l3::legacy_traffic::TrafficRelay {
    let mut relay = traffic_submission();
    let RelayMessage::Data { payload, .. } = &mut relay.message else {
        panic!()
    };
    let mut sms = sms::decode_prefix(payload, Direction::Reverse)
        .unwrap()
        .message;
    let SmsMessage::Submit { user_data, .. } = &mut sms else {
        panic!()
    };
    *user_data = UserData::Ira(vec![b'x'; characters]);
    *payload = sms.encode().unwrap();
    relay
}
fn full_traffic_submission() -> crate::l3::legacy_traffic::TrafficRelay {
    let mut request = long_submission(sms::MAX_SINGLE_RELAY_IRA_CHARACTERS);
    assert_eq!(request.optional_octets.len(), 5);
    let mut bits: Vec<_> = request
        .optional_octets
        .iter()
        .flat_map(|byte| (0..8).rev().map(move |bit| byte & (1 << bit) != 0))
        .collect();
    bits.truncate(36);
    for (value, width) in [(8u8, 4), (57u8, 8)] {
        bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
    }
    bits.extend(vec![false; 57 * 8]);
    request.optional_octets = bits
        .as_chunks::<8>()
        .0
        .iter()
        .map(|byte| byte.iter().fold(0, |value, bit| value * 2 + u8::from(*bit)))
        .collect();
    assert_eq!(request.optional_octets.len(), 63);
    assert!(request.control_message(Direction::Reverse).is_err());
    request
}
#[test]
fn full_dtc_fields_reach_durable_delivery_without_dcch_conversion_or_truncation() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![caller, callee], 0).unwrap();
    let request = full_traffic_submission();
    let message = crate::l3::legacy_traffic::TrafficMessage::Relay(request.clone());
    let decoded = crate::l3::legacy_traffic::decode_prefix(&message.encode().unwrap())
        .unwrap()
        .message;
    assert_eq!(decoded, message);
    runtime.receive_traffic(caller, &request, 1, 0).unwrap();
    runtime.receive_traffic(caller, &request, 2, 0).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .len(),
        1
    );
    runtime.advance(12800).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    let crate::l3::legacy_traffic::TrafficMessage::Relay(relay) = &offer.message else {
        panic!()
    };
    assert!(relay.control_message(Direction::Forward).is_err());
    let RelayMessage::Data { payload, .. } = &relay.message else {
        panic!()
    };
    assert_eq!(payload.len(), relay::MAX_DATA_PAYLOAD_OCTETS);
    let SmsMessage::Deliver {
        user_data: UserData::Ira(text),
        ..
    } = sms::decode_prefix(payload, Direction::Forward)
        .unwrap()
        .message
    else {
        panic!()
    };
    assert_eq!(text, vec![b'x'; sms::MAX_SINGLE_RELAY_IRA_CHARACTERS]);
    runtime.traffic_queued(offer.token).unwrap();
    assert!(runtime.traffic_transmitted(offer.token, 23000).unwrap());
    runtime
        .receive_traffic(callee, &acceptance(&offer), 23001, 0)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn an_idle_phone_keeps_long_sms_queued_without_blocking_shorter_messages() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![caller], 0).unwrap();
    runtime
        .receive_traffic(
            caller,
            &long_submission(sms::MAX_SINGLE_RELAY_IRA_CHARACTERS),
            1,
            0,
        )
        .unwrap();
    runtime.advance(12800).unwrap();
    assert!(!runtime.delivery.busy(callee.identity));
    assert!(
        runtime.operation_messages(10).unwrap()[0]
            .measurements
            .iter()
            .any(|metric| metric.name == "Delivery channel requirement"
                && metric.value.as_deref() == Some("Active call"))
    );
    assert!(runtime.pending.is_empty());
    let mut short = traffic_submission();
    let RelayMessage::Data { transaction_id, .. } = &mut short.message else {
        panic!()
    };
    *transaction_id += 1;
    runtime.receive_traffic(caller, &short, 12801, 0).unwrap();
    runtime.advance(12801).unwrap();
    let short_id = runtime
        .pending
        .iter()
        .find_map(|pending| pending.owner.delivery())
        .unwrap()
        .id();
    let ids = runtime.core().store().pending_for(callee.identity).unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(short_id, ids[1].id);
    runtime.sync_traffic(vec![caller, callee], 12802).unwrap();
    runtime.advance(12802).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    let crate::l3::legacy_traffic::TrafficMessage::Relay(relay) = &offer.message else {
        panic!()
    };
    assert!(relay.control_message(Direction::Forward).is_err());
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn oversize_deliver_translation_and_invalid_long_addressing_are_rejected_before_storage() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    runtime.sync_traffic(vec![caller], 0).unwrap();
    let request = long_submission(sms::MAX_SINGLE_RELAY_IRA_CHARACTERS + 1);
    runtime.receive_traffic(caller, &request, 1, 0).unwrap();
    let rejected = runtime.traffic_offer(caller, RelayChannel::Facch).unwrap();
    assert!(matches!(
        rejected.message,
        crate::l3::legacy_traffic::TrafficMessage::Relay(crate::l3::legacy_traffic::TrafficRelay {
            message: RelayMessage::Reject { cause: 3, .. },
            ..
        })
    ));
    runtime.traffic_queued(rejected.token).unwrap();
    runtime.traffic_transmitted(rejected.token, 2).unwrap();
    let mut malformed = long_submission(sms::MAX_SINGLE_RELAY_IRA_CHARACTERS);
    malformed.optional_octets = vec![0x20, 0xff];
    runtime.receive_traffic(caller, &malformed, 3, 0).unwrap();
    let rejected = runtime.traffic_offer(caller, RelayChannel::Facch).unwrap();
    assert!(matches!(
        rejected.message,
        crate::l3::legacy_traffic::TrafficMessage::Relay(crate::l3::legacy_traffic::TrafficRelay {
            message: RelayMessage::Reject { cause: 95, .. },
            ..
        })
    ));
    assert!(runtime.operation_messages(10).unwrap().is_empty());
}

#[test]
fn full_size_traffic_delivery_receipt_uses_the_same_persistent_receipt_rules() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![caller, callee], 0).unwrap();
    let mut request = traffic_submission();
    let RelayMessage::Data { payload, .. } = &mut request.message else {
        panic!()
    };
    let mut submitted = sms::decode_prefix(payload, Direction::Reverse)
        .unwrap()
        .message;
    let SmsMessage::Submit { metadata, .. } = &mut submitted else {
        panic!()
    };
    metadata.delivery_acknowledgment = true;
    *payload = submitted.encode().unwrap();
    runtime.receive_traffic(caller, &request, 1, 0).unwrap();
    runtime.advance(12800).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    runtime.traffic_transmitted(offer.token, 13800).unwrap();
    runtime
        .receive_traffic(callee, &acceptance(&offer), 13801, 0)
        .unwrap();
    let receipt = AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: 33,
            protocol: HigherLayerProtocol::Sms,
            payload: SmsMessage::DeliveryAck {
                message_reference: 12,
                user_data: UserData::Ira(vec![b'r'; 250]),
            }
            .encode()
            .unwrap(),
        },
        addresses: RelayAddresses {
            destination: Some(AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("1001".into()),
            }),
            ..Default::default()
        },
    };
    let receipt = crate::l3::legacy_traffic::TrafficRelay::from_addressed(&receipt).unwrap();
    assert!(receipt.control_message(Direction::Reverse).is_err());
    runtime.receive_traffic(callee, &receipt, 13802, 0).unwrap();
    runtime.advance(14400).unwrap();
    let returned = runtime.traffic_offer(caller, RelayChannel::Sacch).unwrap();
    let crate::l3::legacy_traffic::TrafficMessage::Relay(relay) = &returned.message else {
        panic!()
    };
    let RelayMessage::Data { payload, .. } = &relay.message else {
        panic!()
    };
    assert_eq!(
        sms::decode_prefix(payload, Direction::Forward)
            .unwrap()
            .message,
        SmsMessage::DeliveryAck {
            message_reference: 12,
            user_data: UserData::Ira(vec![b'r'; 250])
        }
    );
    runtime.traffic_queued(returned.token).unwrap();
    runtime.traffic_transmitted(returned.token, 24000).unwrap();
    runtime
        .receive_traffic(caller, &acceptance(&returned), 24001, 0)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_for(caller.identity)
            .unwrap()
            .is_empty()
    );
    assert!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mobile_originated_reply_uses_sms_orig_budget_instead_of_mt_facch_timeout() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    runtime.sync_traffic(vec![caller], 0).unwrap();
    runtime
        .receive_traffic(caller, &traffic_submission(), 1, 0)
        .unwrap();
    let offer = runtime.traffic_offer(caller, RelayChannel::Facch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    assert!(runtime.traffic_transmitted(offer.token, 5000).unwrap());
    runtime
        .receive_traffic(caller, &traffic_submission(), 6000, 0)
        .unwrap();
    let retry = runtime.traffic_offer(caller, RelayChannel::Facch).unwrap();
    runtime.traffic_queued(retry.token).unwrap();
    assert!(!runtime.traffic_transmitted(retry.token, 18800).unwrap());
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn non_arq_sms_remains_in_progress_until_its_last_fragment() {
    let mut runtime = runtime();
    runtime
        .enqueue(MobileIdentity::Min34(1), vec![9; 40], None, 0)
        .unwrap();
    runtime
        .enqueue(MobileIdentity::Min34(2), vec![10], None, 0)
        .unwrap();
    assert!(!runtime.in_progress());
    let mut receiver = SpachReceiver::default();
    let mut message = None;
    let mut fragments = 0;
    while let Some(frame) = runtime.frame(0, 0).copied() {
        let decoded = receiver.push(&frame).unwrap();
        runtime.submitted().unwrap();
        fragments += 1;
        if let Some(decoded) = decoded {
            message = Some(decoded);
            break;
        }
        assert!(runtime.in_progress());
    }
    assert!(fragments > 1);
    assert!(!runtime.in_progress());
    let message = message.unwrap();
    assert_eq!(message.identity, MobileIdentity::Min34(1));
    assert_eq!(message.message, vec![9; 40]);
    let next = *runtime.frame(0, 0).unwrap();
    assert_eq!(
        receiver.push(&next).unwrap().unwrap().identity,
        MobileIdentity::Min34(2)
    );
}
#[test]
fn serial_extension_version_is_checked_before_sms_submission_changes_state() {
    let mut runtime = runtime();
    let mut set = submission();
    set.messages.push(
        crate::l3::registration::SerialNumber {
            esn: 42,
            esnx: Some(0),
            s_esn: Some(12),
            ignored_bits: Vec::new(),
            unparsed_bits: vec![],
        }
        .encode()
        .unwrap(),
    );
    assert!(runtime.receive(&set, 0, 0).is_err());
    assert!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .is_empty()
    );
    runtime
        .core_mut()
        .register(
            &MessageSet {
                identity: set.identity,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::Periodic,
                        scm: 15,
                        protocol_version: ProtocolVersion::Pv5,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            1,
        )
        .unwrap();
    runtime.receive(&set, 1, 1).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
    assert!(
        runtime
            .core()
            .registry()
            .subscribers()
            .iter()
            .find(|subscriber| subscriber.provisioning.identity == set.identity)
            .unwrap()
            .reported_serial
            .is_none()
    );
}

fn arq_delivery() -> (SmsRuntime, ArqMessage) {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    runtime
        .advance(super::super::timers::SMS_ORIGINATION_MS)
        .unwrap();
    let pending = runtime
        .pending
        .iter_mut()
        .find(|pending| pending.owner.delivery().is_some())
        .unwrap();
    pending.confirmed = true;
    let message = runtime.arq_offer(|_| true).unwrap();
    runtime.arq_admitted(message.clone());
    (runtime, message)
}
fn arq_accept(message: &ArqMessage) -> MessageSet {
    let RelayMessage::Data { transaction_id, .. } =
        relay::decode_prefix(&message.bytes, Direction::Forward)
            .unwrap()
            .message
    else {
        panic!()
    };
    MessageSet {
        identity: message.identity,
        messages: vec![
            RelayMessage::Accept { transaction_id }
                .encode(Direction::Reverse)
                .unwrap(),
        ],
    }
}
#[test]
fn arq_relay_response_requires_submission_but_can_precede_host_and_status_callbacks() {
    let (mut runtime, message) = arq_delivery();
    let accepted = arq_accept(&message);
    let now = super::super::timers::SMS_ORIGINATION_MS;
    assert!(runtime.receive(&accepted, now, 0).is_err());
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(message.identity)
            .unwrap()
            .len(),
        1
    );
    runtime.arq_submitted(&message);
    let mut wrong = accepted.clone();
    let RelayMessage::Accept { transaction_id } =
        relay::decode_prefix(&wrong.messages[0], Direction::Reverse)
            .unwrap()
            .message
    else {
        panic!()
    };
    wrong.messages[0] = RelayMessage::Accept {
        transaction_id: transaction_id ^ 1,
    }
    .encode(Direction::Reverse)
    .unwrap();
    assert!(runtime.receive(&wrong, now + 1, 0).is_err());
    assert!(runtime.arq_current(&message));
    runtime.receive(&accepted, now + 2, 0).unwrap();
    assert!(!runtime.arq_current(&message));
    assert!(
        runtime
            .core()
            .store()
            .pending_for(message.identity)
            .unwrap()
            .is_empty()
    );
    runtime.arq_transmitted(&message, now + 3).unwrap();
    runtime.arq_transmitted(&message, now + 4).unwrap();
    runtime
        .arq_ended(message.clone(), ArqOutcome::Failed, now + 5)
        .unwrap();
    assert!(!runtime.arq_current(&message));
    assert!(
        runtime
            .core()
            .store()
            .pending_for(message.identity)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn arq_host_completion_starts_relay_timer_once_and_l2_ack_does_not_deliver_sms() {
    let (mut runtime, message) = arq_delivery();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    let prepared = message.owner.delivery().unwrap();
    runtime.arq_submitted(&message);
    runtime.arq_transmitted(&message, now + 1).unwrap();
    let deadline = (now + 1 + super::super::timers::SPACH_MS).to_string();
    runtime.arq_transmitted(&message, now + 100).unwrap();
    assert!(
        runtime.operation_messages(10).unwrap()[0]
            .measurements
            .iter()
            .any(|metric| {
                metric.name == "Response deadline" && metric.value.as_deref() == Some(&deadline)
            })
    );
    runtime
        .arq_ended(message.clone(), ArqOutcome::Confirmed, now + 101)
        .unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(message.identity)
            .unwrap()
            .len(),
        1
    );
    assert!(runtime.delivery.is_active(prepared));
    runtime
        .receive(&arq_accept(&message), now + 102, 0)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_for(message.identity)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn failed_arq_keeps_sms_durable_and_stale_owner_cannot_cancel_replacement() {
    let (mut runtime, message) = arq_delivery();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.arq_submitted(&message);
    runtime
        .arq_ended(message.clone(), ArqOutcome::Failed, now + 1)
        .unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(message.identity)
            .unwrap()
            .len(),
        1
    );
    runtime
        .advance(now + super::super::timers::SUPERFRAME_MS)
        .unwrap();
    let pending = runtime
        .pending
        .iter_mut()
        .find(|pending| pending.owner.delivery().is_some())
        .unwrap();
    pending.confirmed = true;
    let replacement = runtime.arq_offer(|_| true).unwrap();
    runtime.arq_admitted(replacement.clone());
    assert_ne!(message.generation, replacement.generation);
    assert!(
        !runtime
            .delivery
            .submitted(message.owner.delivery().unwrap())
    );
    assert!(
        !runtime
            .delivery
            .cancel_prepared(
                message.owner.delivery().unwrap(),
                now + super::super::timers::SUPERFRAME_MS
            )
            .unwrap()
    );
    runtime
        .arq_ended(
            message,
            ArqOutcome::Cancelled,
            now + super::super::timers::SUPERFRAME_MS,
        )
        .unwrap();
    assert!(
        runtime
            .delivery
            .is_active(replacement.owner.delivery().unwrap())
    );
    assert!(runtime.arq_current(&replacement));
}

#[test]
fn entering_traffic_invalidates_control_arq_without_losing_or_reviving_delivery() {
    let (mut runtime, message) = arq_delivery();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.arq_submitted(&message);
    runtime
        .sync_traffic(vec![peer(2, 7, true)], now + 1)
        .unwrap();
    assert!(!runtime.arq_current(&message));
    runtime
        .advance(now + super::super::timers::SUPERFRAME_MS)
        .unwrap();
    let prepared = runtime
        .traffic
        .pending
        .front()
        .unwrap()
        .owner
        .delivery()
        .unwrap()
        .clone();
    runtime
        .arq_transmitted(&message, now + super::super::timers::SUPERFRAME_MS)
        .unwrap();
    runtime
        .arq_ended(
            message.clone(),
            ArqOutcome::Expired,
            now + super::super::timers::SUPERFRAME_MS,
        )
        .unwrap();
    assert!(runtime.delivery.is_active(&prepared));
    assert!(
        runtime
            .receive(
                &arq_accept(&message),
                now + super::super::timers::SUPERFRAME_MS,
                0
            )
            .is_err()
    );
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(message.identity)
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn active_control_arq_counts_against_the_shared_traffic_and_control_queue_limit() {
    let (mut runtime, message) = arq_delivery();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    let bytes = RelayMessage::Accept { transaction_id: 3 }
        .encode(Direction::Forward)
        .unwrap();
    for _ in 1..MAX_PENDING {
        runtime
            .enqueue(message.identity, bytes.clone(), None, now)
            .unwrap();
    }
    assert!(matches!(
        runtime.enqueue(message.identity, bytes.clone(), None, now),
        Err(Failure::QueueFull)
    ));
    assert!(matches!(
        runtime.enqueue_route(message.identity, bytes, None, Route::Traffic(7), now),
        Err(Failure::QueueFull)
    ));
}

#[test]
fn early_relay_success_then_a_new_sms_survives_old_arq_receipts_and_failure() {
    let (mut runtime, old) = arq_delivery();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.arq_submitted(&old);
    runtime.receive(&arq_accept(&old), now + 1, 0).unwrap();
    let mut next = relay::addressing::decode(&submission().messages[0], Direction::Reverse)
        .unwrap()
        .relay;
    let RelayMessage::Data {
        transaction_id,
        payload,
        ..
    } = &mut next.message
    else {
        panic!()
    };
    *transaction_id += 1;
    let SmsMessage::Submit {
        metadata,
        user_data,
    } = sms::decode_prefix(payload, Direction::Reverse)
        .unwrap()
        .message
    else {
        panic!()
    };
    *payload = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: metadata.message_reference + 1,
            ..metadata
        },
        user_data,
    }
    .encode()
    .unwrap();
    runtime
        .receive(
            &MessageSet {
                identity: MobileIdentity::Min34(1),
                messages: vec![next.encode(Direction::Reverse).unwrap()],
            },
            now + 2,
            0,
        )
        .unwrap();
    runtime.advance(now + 3).unwrap();
    let replacement = runtime
        .pending
        .iter()
        .find_map(|pending| pending.owner.delivery().cloned())
        .unwrap();
    assert_ne!(old.owner.delivery().unwrap().id(), replacement.id());
    runtime.arq_transmitted(&old, now + 4).unwrap();
    runtime.arq_ended(old, ArqOutcome::Failed, now + 5).unwrap();
    assert!(runtime.delivery.is_active(&replacement));
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(replacement.recipient())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn international_and_opaque_iso_submissions_preserve_durable_forward_payloads() {
    for user_data in [
        UserData::from_text("café").unwrap(),
        UserData::from_text("短信 Ελληνικά").unwrap(),
        UserData::International {
            encoding: sms::InternationalEncoding::Iso8859_8,
            data: vec![0xe0, 0xe1],
        },
    ] {
        let mut runtime = runtime();
        let mut addressed =
            relay::addressing::decode(&submission().messages[0], Direction::Reverse)
                .unwrap()
                .relay;
        let RelayMessage::Data { payload, .. } = &mut addressed.message else {
            panic!()
        };
        let SmsMessage::Submit { metadata, .. } = sms::decode_prefix(payload, Direction::Reverse)
            .unwrap()
            .message
        else {
            panic!()
        };
        *payload = SmsMessage::Submit {
            metadata,
            user_data: user_data.clone(),
        }
        .encode()
        .unwrap();
        runtime
            .receive(
                &MessageSet {
                    identity: MobileIdentity::Min34(1),
                    messages: vec![addressed.encode(Direction::Reverse).unwrap()],
                },
                0,
                0,
            )
            .unwrap();
        runtime
            .advance(super::super::timers::SMS_ORIGINATION_MS)
            .unwrap();
        let pending = runtime
            .pending
            .iter()
            .find(|pending| pending.owner.delivery().is_some())
            .unwrap();
        let RelayMessage::Data { payload, .. } = &pending.owner.delivery().unwrap().relay().message
        else {
            panic!()
        };
        let SmsMessage::Deliver {
            user_data: forwarded,
            ..
        } = sms::decode_prefix(payload, Direction::Forward)
            .unwrap()
            .message
        else {
            panic!()
        };
        assert_eq!(forwarded, user_data);
        assert_eq!(
            runtime.operation_messages(10).unwrap()[0].text,
            user_data.text()
        );
        assert_eq!(
            runtime
                .core()
                .store()
                .pending_for(MobileIdentity::Min34(2))
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn traffic_power_down_keeps_durable_sms_and_ignores_stale_ownership() {
    let mut runtime = runtime();
    let identity = MobileIdentity::Min34(2);
    runtime.receive(&submission(), 0, 0).unwrap();
    let old = runtime
        .core()
        .registry()
        .registration_handle(identity)
        .unwrap();
    let registration = MessageSet {
        identity,
        messages: vec![
            AccessMessage::Registration {
                registration_type: RegistrationType::PowerUp,
                scm: 15,
                protocol_version: ProtocolVersion::Is136RevA,
            }
            .encode()
            .unwrap(),
        ],
    };
    runtime.core_mut().register(&registration, 0).unwrap();
    assert!(!runtime.power_down(&old, 1).unwrap());
    assert!(
        runtime
            .core()
            .registry()
            .by_number("1002")
            .unwrap()
            .registration
            .is_some()
    );
    let current = runtime
        .core()
        .registry()
        .registration_handle(identity)
        .unwrap();
    assert!(runtime.power_down(&current, 2).unwrap());
    assert!(!runtime.power_down(&current, 3).unwrap());
    assert!(
        runtime
            .core()
            .registry()
            .by_number("1002")
            .unwrap()
            .registration
            .is_none()
    );
    assert!(
        runtime
            .core()
            .registry()
            .by_number("1001")
            .unwrap()
            .registration
            .is_some()
    );
    assert_eq!(
        runtime.core().store().pending_for(identity).unwrap().len(),
        1
    );
    assert!(
        runtime
            .pending
            .iter()
            .all(|pending| pending.identity != identity)
    );
    runtime.core_mut().register(&registration, 1).unwrap();
    assert_eq!(
        runtime.core().store().pending_for(identity).unwrap().len(),
        1
    );
}

#[test]
fn power_down_cancels_inflight_arq_without_accepting_late_receipts() {
    let (mut runtime, message) = arq_delivery();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.arq_submitted(&message);
    let owner = runtime
        .core()
        .registry()
        .registration_handle(message.identity)
        .unwrap();
    assert!(runtime.arq_current(&message));
    assert!(runtime.power_down(&owner, now + 1).unwrap());
    assert!(!runtime.arq_current(&message));
    runtime.arq_submitted(&message);
    runtime
        .arq_ended(message.clone(), ArqOutcome::Confirmed, now + 2)
        .unwrap();
    let error = runtime
        .receive(&arq_accept(&message), now + 3, (now + 3) as i64)
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<DeliveryError>(),
        Some(DeliveryError::StaleTransmission)
    ));
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(message.identity)
            .unwrap()
            .len(),
        1
    );
    assert!(
        runtime
            .pending
            .iter()
            .all(|pending| pending.identity != message.identity)
    );
}
#[test]
fn segmented_queue_records_remain_queued_without_blocking_supported_deliveries() {
    let mut runtime = runtime();
    let destination = AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd("1002".into()),
    };
    let message = |user_data| {
        SmsMessage::Submit {
            metadata: Metadata {
                message_reference: 12,
                privacy: 0,
                urgency: 0,
                delivery_acknowledgment: false,
                manual_acknowledgment: false,
                new_message: true,
            },
            user_data: UserData::Ira(user_data),
        }
        .encode()
        .unwrap()
    };
    let long = runtime
        .core_mut()
        .submit_complete_sms(
            MobileIdentity::Min34(1),
            &destination,
            &message(vec![b'x'; 254]),
            "segmented",
            1000,
        )
        .unwrap();
    let now_ms = crate::runtime::timers::SMS_ORIGINATION_MS + crate::runtime::timers::SUPERFRAME_MS;
    for transport in [DeliveryTransport::Control, DeliveryTransport::Traffic] {
        assert!(
            runtime
                .delivery
                .prepare_for(MobileIdentity::Min34(2), now_ms, transport)
                .unwrap()
                .is_none()
        );
    }
    let view = runtime.operation_messages(10).unwrap();
    assert!(
        view[0]
            .measurements
            .iter()
            .any(|metric| metric.name == "Delivery channel requirement"
                && metric.value.as_deref() == Some("PV1+ segmented SMS"))
    );
    assert_eq!(
        runtime.core().store().state(long.id).unwrap(),
        crate::core::store::QueueState::Queued
    );
    let short = runtime
        .core_mut()
        .submit_complete_sms(
            MobileIdentity::Min34(1),
            &destination,
            &message(b"short".to_vec()),
            "supported",
            1001,
        )
        .unwrap();
    let prepared = runtime
        .delivery
        .prepare(MobileIdentity::Min34(2), now_ms)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.id(), short.id);
    assert_eq!(
        runtime.core().store().state(long.id).unwrap(),
        crate::core::store::QueueState::Queued
    );
}
#[test]
fn auxiliary_acceptance_is_routed_without_completing_a_concurrent_sms() {
    use crate::core::delivery::{AuxiliaryState, AuxiliaryTransport};
    let mut runtime = runtime();
    let peer = MobileIdentity::Min34(2);
    let destination = AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd("1002".into()),
    };
    let pdu = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 12,
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
    let stored = runtime
        .core_mut()
        .submit_complete_sms(
            MobileIdentity::Min34(1),
            &destination,
            &pdu,
            "simultaneous",
            1000,
        )
        .unwrap();
    let now = crate::runtime::timers::SMS_ORIGINATION_MS + crate::runtime::timers::SUPERFRAME_MS;
    let sms = runtime.delivery.prepare(peer, now).unwrap().unwrap();
    runtime.delivery.transmitted(&sms, now).unwrap();
    let auxiliary = runtime
        .delivery
        .prepare_auxiliary(
            peer,
            AddressedRelay {
                message: RelayMessage::Data {
                    transaction_id: 0,
                    protocol: HigherLayerProtocol::Segmented(crate::l3::relay::Teleservice::Sms),
                    payload: vec![0x18, 0, 1, 0],
                },
                addresses: RelayAddresses::default(),
            },
            AuxiliaryTransport::Control,
            now,
        )
        .unwrap();
    runtime
        .delivery
        .auxiliary_submitted(&auxiliary, now, now)
        .unwrap();
    runtime
        .delivery
        .auxiliary_transmitted(&auxiliary, now, now, super::super::timers::SPACH_MS)
        .unwrap();
    let bytes = RelayMessage::Accept {
        transaction_id: auxiliary.transaction_id(),
    }
    .encode(Direction::Reverse)
    .unwrap();
    runtime
        .receive_relay(peer, &bytes, Route::Control, now + 1, 1000)
        .unwrap();
    assert_eq!(
        runtime.delivery.auxiliary_state(&auxiliary).unwrap(),
        AuxiliaryState::Accepted
    );
    assert!(runtime.delivery.busy(peer));
    assert_eq!(
        runtime.core().store().state(stored.id).unwrap(),
        crate::core::store::QueueState::Queued
    );
    let RelayMessage::Data { transaction_id, .. } = sms.relay().message else {
        panic!("expected data");
    };
    let bytes = RelayMessage::Accept { transaction_id }
        .encode(Direction::Reverse)
        .unwrap();
    runtime
        .receive_relay(peer, &bytes, Route::Control, now + 2, 1000)
        .unwrap();
    assert_eq!(
        runtime.core().store().state(stored.id).unwrap(),
        crate::core::store::QueueState::Delivered
    );
}
#[test]
fn interrupted_reply_does_not_cancel_a_concurrent_owned_traffic_delivery() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![caller, callee], 0).unwrap();
    runtime
        .receive_traffic(caller, &traffic_submission(), 0, 0)
        .unwrap();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.advance(now).unwrap();
    let sms = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    let prepared = runtime
        .traffic
        .pending
        .iter()
        .find(|pending| pending.offer.token == sms.token)
        .unwrap()
        .owner
        .delivery()
        .unwrap()
        .clone();
    runtime
        .enqueue_route(
            callee.identity,
            RelayMessage::Accept { transaction_id: 91 }
                .encode(Direction::Forward)
                .unwrap(),
            None,
            Route::Traffic(callee.call_id),
            now,
        )
        .unwrap();
    let reply = runtime.traffic_offer(callee, RelayChannel::Facch).unwrap();
    runtime.traffic_queued(reply.token).unwrap();
    runtime.traffic_interrupted(reply.token, now + 1).unwrap();
    assert!(!runtime.traffic_valid(reply.token));
    assert!(runtime.traffic_valid(sms.token));
    assert!(runtime.delivery.is_active(&prepared));
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .len(),
        1
    );
    runtime.traffic_queued(sms.token).unwrap();
    assert!(runtime.traffic_transmitted(sms.token, now + 2).unwrap());
    runtime
        .receive_traffic(callee, &acceptance(&sms), now + 3, 0)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn stale_traffic_interruption_cannot_cancel_a_replacement_attempt_for_the_same_sms() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![caller, callee], 0).unwrap();
    runtime
        .receive_traffic(caller, &traffic_submission(), 0, 0)
        .unwrap();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.advance(now).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    let old = runtime
        .traffic
        .pending
        .iter()
        .find(|pending| pending.offer.token == offer.token)
        .unwrap()
        .owner
        .delivery()
        .unwrap()
        .clone();
    runtime.traffic_queued(offer.token).unwrap();
    assert!(runtime.delivery.cancel_prepared(&old, now + 1).unwrap());
    let replacement = runtime
        .delivery
        .prepare_for(callee.identity, now + 1, DeliveryTransport::Traffic)
        .unwrap()
        .unwrap();
    assert_eq!(replacement.id(), old.id());
    runtime.traffic_interrupted(offer.token, now + 2).unwrap();
    assert!(runtime.delivery.is_active(&replacement));
    assert!(!runtime.traffic_valid(offer.token));
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn expired_control_queue_owner_cannot_cancel_a_replacement_attempt_for_the_same_sms() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.advance(now).unwrap();
    let pending = runtime
        .pending
        .iter()
        .find(|pending| pending.owner.delivery().is_some())
        .unwrap();
    let old = pending.owner.delivery().unwrap().clone();
    let deadline = pending.deadline_ms;
    assert!(runtime.delivery.cancel_prepared(&old, now + 1).unwrap());
    let replacement = runtime
        .delivery
        .prepare(old.recipient(), now + 1)
        .unwrap()
        .unwrap();
    assert_eq!(replacement.id(), old.id());
    runtime.advance(deadline).unwrap();
    assert!(runtime.delivery.is_active(&replacement));
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(old.recipient())
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn queue_admission_binds_delivery_owner_to_exact_recipient_and_wire_message() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.advance(now).unwrap();
    let prepared = runtime
        .pending
        .iter()
        .find_map(|pending| pending.owner.delivery().cloned())
        .unwrap();
    let bytes = prepared.relay().encode(Direction::Forward).unwrap();
    let pending_count = runtime.pending.len();
    assert!(matches!(
        runtime.enqueue(
            MobileIdentity::Min34(1),
            bytes.clone(),
            Some(prepared.clone()),
            now
        ),
        Err(Failure::Wire)
    ));
    let mut changed = bytes;
    *changed.last_mut().unwrap() ^= 1;
    assert!(matches!(
        runtime.enqueue(prepared.recipient(), changed, Some(prepared.clone()), now),
        Err(Failure::Wire)
    ));
    let relay = crate::l3::legacy_traffic::TrafficRelay::from_addressed(prepared.relay()).unwrap();
    assert!(matches!(
        runtime.enqueue_traffic(
            MobileIdentity::Min34(1),
            7,
            relay.clone(),
            Some(prepared.clone()),
            now
        ),
        Err(Failure::Wire)
    ));
    let mut changed = relay;
    if let RelayMessage::Data {
        ref mut payload, ..
    } = changed.message
    {
        *payload.last_mut().unwrap() ^= 1;
    }
    assert!(matches!(
        runtime.enqueue_traffic(
            prepared.recipient(),
            7,
            changed,
            Some(prepared.clone()),
            now
        ),
        Err(Failure::Wire)
    ));
    assert_eq!(runtime.pending.len(), pending_count);
    assert!(runtime.traffic.pending.is_empty());
    assert!(runtime.delivery.is_active(&prepared));
}
#[test]
fn stale_traffic_completion_does_not_fail_or_start_a_replacement_response_timer() {
    let mut runtime = runtime();
    let caller = peer(1, 7, true);
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![caller, callee], 0).unwrap();
    runtime
        .receive_traffic(caller, &traffic_submission(), 0, 0)
        .unwrap();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.advance(now).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    let old = runtime
        .traffic
        .pending
        .iter()
        .find(|pending| pending.offer.token == offer.token)
        .unwrap()
        .owner
        .delivery()
        .unwrap()
        .clone();
    runtime.traffic_queued(offer.token).unwrap();
    runtime.delivery.cancel_prepared(&old, now + 1).unwrap();
    let replacement = runtime
        .delivery
        .prepare_for(callee.identity, now + 1, DeliveryTransport::Traffic)
        .unwrap()
        .unwrap();
    assert!(!runtime.traffic_transmitted(offer.token, now + 2).unwrap());
    assert!(runtime.delivery.is_active(&replacement));
    let RelayMessage::Data { transaction_id, .. } = replacement.relay().message else {
        panic!("expected data");
    };
    let bytes = RelayMessage::Accept { transaction_id }
        .encode(Direction::Reverse)
        .unwrap();
    assert!(matches!(
        runtime.delivery.receive(callee.identity, &bytes, now + 3),
        Err(DeliveryError::Attempt(
            crate::core::relay_attempt::AttemptError::NotTransmitted
        ))
    ));
    assert!(runtime.delivery.is_active(&replacement));
    runtime.sync_traffic(Vec::new(), now + 4).unwrap();
    assert!(runtime.delivery.is_active(&replacement));
    assert!(runtime.traffic.attempts.is_empty());
}

fn has_deadline(runtime: &SmsRuntime, deadline_ms: u64) -> bool {
    let deadline = deadline_ms.to_string();
    runtime
        .operation_messages(10)
        .unwrap()
        .iter()
        .any(|message| {
            message.measurements.iter().any(|metric| {
                metric.name == "Response deadline" && metric.value.as_deref() == Some(&deadline)
            })
        })
}

#[test]
fn delayed_control_completion_keeps_the_original_response_cutoff() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.advance(now).unwrap();
    let prepared = runtime
        .pending
        .iter()
        .find_map(|pending| pending.owner.delivery())
        .unwrap()
        .clone();
    runtime
        .delivery
        .advance(prepared.recipient(), now + 40)
        .unwrap();
    runtime
        .transmitted_at(&prepared, now + 10, now + 100)
        .unwrap();
    assert!(has_deadline(
        &runtime,
        now + 10 + super::super::timers::SPACH_MS
    ));
}

#[test]
fn delayed_arq_completion_uses_final_burst_time_and_rejects_future_receipts() {
    let (mut runtime, message) = arq_delivery();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.arq_submitted(&message);
    assert!(
        runtime
            .arq_transmitted_at(&message, now + 101, now + 100)
            .is_err()
    );
    assert!(
        runtime
            .arq_transmitted_at(&message, now - 1, now + 100)
            .is_err()
    );
    runtime
        .arq_transmitted_at(&message, now + 10, now + 100)
        .unwrap();
    runtime
        .arq_transmitted_at(&message, now + 110, now + 110)
        .unwrap();
    assert!(has_deadline(
        &runtime,
        now + 10 + super::super::timers::SPACH_MS
    ));
}

#[test]
fn delayed_traffic_completion_uses_final_burst_time() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 1).unwrap();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.advance(now).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    assert!(
        runtime
            .traffic_transmitted_at(offer.token, now + 10, now + 100)
            .unwrap()
    );
    let deadline = now + 10 + traffic::SACCH_RESPONSE_MS;
    assert!(has_deadline(&runtime, deadline));
    assert!(
        runtime
            .receive_traffic(callee, &acceptance(&offer), deadline, 0)
            .is_err()
    );
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn late_observation_does_not_discard_an_on_time_traffic_transmission() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 1).unwrap();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.advance(now).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    let observed = now + super::super::timers::SMS_ORIGINATION_MS;
    assert!(
        runtime
            .traffic_transmitted_at(offer.token, now + 10, observed)
            .unwrap()
    );
    assert!(
        runtime
            .receive_traffic(callee, &acceptance(&offer), observed, 0)
            .is_err()
    );
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(callee.identity)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn overdue_arq_completion_can_be_cleaned_up_without_reopening_delivery() {
    let (mut runtime, message) = arq_delivery();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    let observed = now + 11 + super::super::timers::SPACH_MS;
    runtime.arq_submitted(&message);
    runtime
        .arq_transmitted_at(&message, now + 10, observed)
        .unwrap();
    runtime
        .arq_ended(message.clone(), ArqOutcome::Failed, observed)
        .unwrap();
    assert!(!runtime.arq_current(&message));
    assert!(runtime.receive(&arq_accept(&message), observed, 0).is_err());
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(message.identity)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn invalid_traffic_completion_keeps_the_owner_for_a_valid_receipt() {
    let mut runtime = runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 1).unwrap();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.advance(now).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    assert!(
        runtime
            .traffic_transmitted_at(offer.token, now - 1, now + 100)
            .is_err()
    );
    assert!(runtime.traffic_valid(offer.token));
    assert!(
        runtime
            .traffic_transmitted_at(offer.token, now + 101, now + 100)
            .is_err()
    );
    assert!(runtime.traffic_valid(offer.token));
    assert!(
        runtime
            .traffic_transmitted_at(offer.token, now + 10, now + 100)
            .unwrap()
    );
    assert!(has_deadline(
        &runtime,
        now + 10 + traffic::SACCH_RESPONSE_MS
    ));
}

#[test]
fn delayed_access_retry_does_not_store_a_second_sms() {
    const LATE_RETRY_MS: u64 = 60_000;
    let mut runtime = runtime();
    let request = submission();
    runtime.receive(&request, 0, 0).unwrap();
    runtime.advance(LATE_RETRY_MS).unwrap();
    runtime.receive(&request, LATE_RETRY_MS, 1).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn access_retry_guard_expires_at_its_original_boundary() {
    const EXPECTED_GUARD_MS: u64 = 163_760;
    let mut runtime = runtime();
    let request = submission();
    runtime.receive(&request, 0, 0).unwrap();
    runtime.advance(EXPECTED_GUARD_MS - 1).unwrap();
    runtime.receive(&request, EXPECTED_GUARD_MS - 1, 1).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
    runtime.advance(EXPECTED_GUARD_MS).unwrap();
    runtime.receive(&request, EXPECTED_GUARD_MS, 2).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn arq_relay_rejection_stops_further_delivery_retries() {
    let (mut runtime, message) = arq_delivery();
    let now = super::super::timers::SMS_ORIGINATION_MS;
    runtime.arq_submitted(&message);
    let accepted = arq_accept(&message);
    let RelayMessage::Accept { transaction_id } =
        relay::decode_prefix(&accepted.messages[0], Direction::Reverse)
            .unwrap()
            .message
    else {
        panic!()
    };
    let rejected = MessageSet {
        identity: message.identity,
        messages: vec![
            RelayMessage::Reject {
                transaction_id,
                cause: 4,
            }
            .encode(Direction::Reverse)
            .unwrap(),
        ],
    };
    assert!(runtime.arq_current(&message));
    runtime.receive(&rejected, now + 1, 0).unwrap();
    assert!(!runtime.arq_current(&message));
}

#[test]
fn replies_wait_until_their_state_is_on_disk() {
    let path = std::env::temp_dir().join(format!(
        "136bts-sms-durability-{}.sqlite3",
        std::process::id()
    ));
    let mut runtime = runtime_from(StateStore::open_replicated(&path).unwrap());
    runtime.receive(&submission(), 0, 0).unwrap();
    assert!(
        runtime.frame(0, 0).is_none(),
        "nothing is sent before the pass stamps its durability mark"
    );
    runtime.core_mut().store_mut().flush().unwrap();
    runtime.stamp_durability();
    assert!(
        runtime
            .core_mut()
            .store_mut()
            .drain(std::time::Duration::from_secs(5))
            .unwrap()
    );
    assert!(runtime.frame(0, 0).is_some());
    drop(runtime);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
