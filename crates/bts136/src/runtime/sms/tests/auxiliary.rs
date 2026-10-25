use super::*;
use crate::{
    core::delivery::AuxiliaryError,
    l3::{legacy_traffic::TrafficRelay, relay::Teleservice},
    radio::{PreparedRadio, carrier::Carrier, duplex::Duplex, receiver, timed_tx},
    runtime::voice::VoiceRuntime,
};
const NOW_MS: u64 = crate::runtime::timers::SMS_ORIGINATION_MS;
const PEER: MobileIdentity = MobileIdentity::Min34(2);
const QUEUED_SLOTS: usize = 4;

fn auxiliary_runtime() -> SmsRuntime {
    let mut runtime = super::runtime();
    runtime
        .core_mut()
        .register(
            &MessageSet {
                identity: PEER,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: ProtocolVersion::Pv1,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            0,
        )
        .unwrap();
    runtime
}
fn relay() -> AddressedRelay {
    AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: 77,
            protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
            payload: crate::l3::tsar::Response {
                transaction_id: 0,
                status: crate::l3::tsar::Status::Received,
                acknowledgments: crate::l3::tsar::AcknowledgmentMap::try_new(vec![true; 8])
                    .unwrap(),
                optional_services_support: None,
            }
            .encode(ProtocolVersion::Pv1)
            .unwrap(),
        },
        addresses: RelayAddresses::default(),
    }
}
fn accept(packet: &AuxiliaryPacket) -> MessageSet {
    MessageSet {
        identity: packet.recipient(),
        messages: vec![
            RelayMessage::Accept {
                transaction_id: packet.transaction_id(),
            }
            .encode(Direction::Reverse)
            .unwrap(),
        ],
    }
}
pub(in crate::runtime::sms) fn radio(
    runtime: &SmsRuntime,
) -> (Carrier<AuxiliaryPacket>, VoiceRuntime) {
    let config = crate::config::Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let prepared = PreparedRadio::open(&config).unwrap();
    let settings = prepared.settings();
    let mut streams = prepared
        .into_streams(
            timed_tx::Limits {
                max_queued_blocks: QUEUED_SLOTS,
                max_block_samples: crate::phy::traffic::SLOT_BITS / 2
                    * crate::config::SAMPLES_PER_SYMBOL,
            },
            receiver::Limits {
                max_block_samples: 64,
                timeout_us: 1000,
            },
        )
        .unwrap();
    let observed = streams.tx.prepared_observation().unwrap();
    let mut carrier = Carrier::try_new(
        Duplex::try_new(streams, crate::config::SAMPLES_PER_SYMBOL).unwrap(),
        QUEUED_SLOTS,
    )
    .unwrap();
    carrier
        .start_continuous(observed.hardware_tick + settings.tx_tick_rate_hz / 10, None)
        .unwrap();
    let voice = VoiceRuntime::try_new(
        config.prepare().unwrap().traffic,
        runtime.allocation,
        config.cell.dvcc,
        config.cell.traffic_power_level,
        &config.calls,
    )
    .unwrap();
    (carrier, voice)
}
fn confirm(runtime: &mut SmsRuntime, packet: &AuxiliaryPacket) {
    let phase = runtime.allocation.phase(packet.recipient()).unwrap();
    assert!(runtime.frame(20, phase).is_some());
    let Completion::Notification {
        identity,
        generation,
    } = runtime.submitted_at(NOW_MS).unwrap()
    else {
        panic!()
    };
    runtime.notification_transmitted(identity, generation);
    runtime
        .receive(
            &MessageSet {
                identity: packet.recipient(),
                messages: vec![vec![0x0f, 0x68]],
            },
            NOW_MS + 1,
            0,
        )
        .unwrap();
}

#[test]
fn control_auxiliary_handoff_completion_and_acceptance_preserve_exact_owner() {
    let mut runtime = auxiliary_runtime();
    let log = std::sync::Arc::new(crate::operations::protocol::ProtocolLog::default());
    runtime.set_protocol_log(log.clone());
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    assert!(runtime.receive(&accept(&packet), NOW_MS, 0).is_err());
    confirm(&mut runtime, &packet);
    let phase = (runtime.allocation.phase(PEER).unwrap() + 1) % 32;
    let mut completed = None;
    while runtime.frame(21, phase).is_some() {
        if let Completion::Auxiliary(owner) = runtime.submitted_at(NOW_MS + 2).unwrap() {
            completed = Some(owner);
        }
    }
    assert_eq!(completed.as_ref(), Some(&packet));
    assert_eq!(
        runtime.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Submitted
    );
    runtime
        .auxiliary_transmitted(&packet, NOW_MS + 3, NOW_MS + 4)
        .unwrap();
    assert_eq!(
        runtime.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::AwaitingResponse {
            deadline_monotonic_ms: NOW_MS + 3 + crate::runtime::timers::SPACH_MS
        }
    );
    let air_messages = log.messages();
    assert!(
        air_messages
            .iter()
            .any(|message| message.direction == "Forward"
                && message.channel == "SMSCH"
                && message.identity == "Min34(2)"
                && message.raw == packet.relay().encode(Direction::Forward).unwrap())
    );
    runtime.receive(&accept(&packet), NOW_MS + 5, 0).unwrap();
    assert_eq!(
        runtime.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Accepted
    );
    assert!(runtime.core().store().pending_for(PEER).unwrap().is_empty());
    let (mut carrier, voice) = radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 6)
        .unwrap();
    let outcome = runtime.take_auxiliary_outcome().unwrap();
    assert_eq!(outcome.reply.packet, packet);
    assert_eq!(outcome.reply.state, AuxiliaryState::Accepted);
    assert_eq!(outcome.observed_monotonic_ms, NOW_MS + 5);
    assert!(runtime.take_auxiliary_outcome().is_none());
    carrier.stop().unwrap();
}

#[test]
fn control_arq_auxiliary_can_be_accepted_after_handoff_before_rf_callback() {
    let mut runtime = auxiliary_runtime();
    let mut data = relay();
    let RelayMessage::Data { payload, .. } = &mut data.message else {
        panic!()
    };
    payload.resize(80, 0);
    let packet = runtime.send_auxiliary(PEER, data, NOW_MS).unwrap();
    confirm(&mut runtime, &packet);
    let message = runtime.arq_offer(|_| true).unwrap();
    assert_eq!(message.owner.auxiliary(), Some(&packet));
    runtime.arq_admitted(message.clone());
    runtime.arq_submitted_at(&message, NOW_MS + 2).unwrap();
    runtime.arq_submitted_at(&message, NOW_MS + 3).unwrap();
    runtime.receive(&accept(&packet), NOW_MS + 4, 0).unwrap();
    runtime
        .arq_transmitted_at(&message, NOW_MS + 3, NOW_MS + 5)
        .unwrap();
    assert!(!runtime.arq_current(&message));
    let (mut carrier, voice) = radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 5)
        .unwrap();
    assert!(runtime.take_auxiliary_outcome().is_none());
    runtime
        .arq_ended(message, ArqOutcome::Cancelled, NOW_MS + 6)
        .unwrap();
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 6)
        .unwrap();
    assert_eq!(
        runtime.take_auxiliary_outcome().unwrap().reply.state,
        AuxiliaryState::Accepted
    );
    carrier.stop().unwrap();
}

#[test]
fn canceled_auxiliary_keeps_its_id_until_queued_radio_work_is_fenced() {
    let mut runtime = auxiliary_runtime();
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    let (mut carrier, voice) = radio(&runtime);
    carrier
        .submit_slot(0, &[false; crate::phy::traffic::SLOT_BITS], packet.clone())
        .unwrap();
    runtime.cancel_auxiliary(&packet, NOW_MS + 1).unwrap();
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 2)
        .unwrap();
    assert_eq!(
        runtime.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Prepared
    );
    assert!(runtime.take_auxiliary_outcome().is_none());
    carrier.stop().unwrap();
    assert!(carrier.stop_acknowledged());
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 3)
        .unwrap();
    assert_eq!(
        runtime.take_auxiliary_outcome().unwrap().reply.state,
        AuxiliaryState::Canceled
    );
    assert!(matches!(
        runtime.cancel_auxiliary(&packet, NOW_MS + 4),
        Err(Failure::Delivery(DeliveryError::Auxiliary(
            AuxiliaryError::StalePacket
        )))
    ));
}

#[test]
fn traffic_auxiliary_requires_all_frame_handoff_and_uses_sacch_response_time() {
    let mut runtime = auxiliary_runtime();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 0).unwrap();
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    assert_eq!(packet.transport(), AuxiliaryTransport::Traffic);
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    assert_eq!(
        TrafficRelay::from_addressed(packet.relay()).unwrap(),
        match &offer.message {
            crate::l3::legacy_traffic::TrafficMessage::Relay(relay) => relay.clone(),
            _ => panic!(),
        }
    );
    runtime.traffic_queued(offer.token).unwrap();
    assert!(
        runtime
            .receive_traffic(
                callee,
                &TrafficRelay::from_control(&accept(&packet).messages[0], Direction::Reverse)
                    .unwrap(),
                NOW_MS + 1,
                0
            )
            .is_err()
    );
    runtime
        .traffic_handed_off(offer.token, NOW_MS + 1, NOW_MS + 2)
        .unwrap();
    runtime
        .traffic_transmitted_at(offer.token, NOW_MS + 3, NOW_MS + 4)
        .unwrap();
    assert_eq!(
        runtime.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::AwaitingResponse {
            deadline_monotonic_ms: NOW_MS + 3 + traffic::SACCH_RESPONSE_MS
        }
    );
    let response = TrafficRelay {
        message: RelayMessage::Accept {
            transaction_id: packet.transaction_id(),
        },
        optional_octets: vec![],
    };
    runtime
        .receive_traffic(callee, &response, NOW_MS + 5, 0)
        .unwrap();
    assert_eq!(
        runtime.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Accepted
    );
}

#[test]
fn route_change_withdraws_auxiliary_instead_of_moving_its_owner_to_a_new_bearer() {
    let mut runtime = auxiliary_runtime();
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    runtime
        .sync_traffic(vec![peer(2, 7, true)], NOW_MS + 1)
        .unwrap();
    runtime.advance_auxiliary_jobs(NOW_MS + 1).unwrap();
    assert!(!runtime.auxiliary_active(&packet));
    assert!(
        runtime
            .traffic
            .pending
            .iter()
            .all(|pending| pending.owner.auxiliary().is_none())
    );
    let (mut carrier, voice) = radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 2)
        .unwrap();
    assert_eq!(
        runtime.take_auxiliary_outcome().unwrap().reply.state,
        AuxiliaryState::Canceled
    );
    carrier.stop().unwrap();
}

#[test]
fn auxiliary_admission_and_unconsumed_outcomes_are_bounded() {
    let mut runtime = auxiliary_runtime();
    for _ in 0..MAX_PENDING {
        runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    }
    assert!(matches!(
        runtime.send_auxiliary(PEER, relay(), NOW_MS),
        Err(Failure::QueueFull)
    ));
    runtime.shutdown(NOW_MS + 1).unwrap();
    let (mut carrier, voice) = radio(&runtime);
    carrier.stop().unwrap();
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 2)
        .unwrap();
    assert!(matches!(
        runtime.send_auxiliary(PEER, relay(), NOW_MS + 2),
        Err(Failure::QueueFull)
    ));
    for _ in 0..MAX_PENDING {
        assert_eq!(
            runtime.take_auxiliary_outcome().unwrap().reply.state,
            AuxiliaryState::Canceled
        );
    }
    assert!(runtime.send_auxiliary(PEER, relay(), NOW_MS + 3).is_ok());
}

#[test]
fn segmentation_is_rejected_for_a_legacy_peer_before_any_identifier_is_reserved() {
    let mut legacy = super::runtime();
    assert!(matches!(
        legacy.send_auxiliary(PEER, relay(), NOW_MS),
        Err(Failure::Wire)
    ));
    let mut unsegmented = relay();
    let RelayMessage::Data { protocol, .. } = &mut unsegmented.message else {
        panic!()
    };
    *protocol = HigherLayerProtocol::Sms;
    let packet = legacy.send_auxiliary(PEER, unsegmented, NOW_MS).unwrap();
    assert_eq!(packet.transaction_id(), 0);
}

#[test]
fn registration_replacement_and_wrong_route_cannot_accept_an_old_exchange() {
    let mut runtime = auxiliary_runtime();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 0).unwrap();
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    runtime
        .traffic_handed_off(offer.token, NOW_MS, NOW_MS)
        .unwrap();
    assert!(runtime.receive(&accept(&packet), NOW_MS + 1, 0).is_err());
    runtime
        .core_mut()
        .register(
            &MessageSet {
                identity: PEER,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: ProtocolVersion::Pv1,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            0,
        )
        .unwrap();
    let response = TrafficRelay {
        message: RelayMessage::Accept {
            transaction_id: packet.transaction_id(),
        },
        optional_octets: vec![],
    };
    assert!(
        runtime
            .receive_traffic(callee, &response, NOW_MS + 2, 0)
            .is_err()
    );
    runtime.advance_auxiliary_jobs(NOW_MS + 2).unwrap();
    assert!(!runtime.auxiliary_active(&packet));
    let (mut carrier, voice) = radio(&runtime);
    carrier.stop().unwrap();
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 3)
        .unwrap();
    assert_eq!(
        runtime.take_auxiliary_outcome().unwrap().reply.state,
        AuxiliaryState::Canceled
    );
}

#[test]
fn auxiliary_admission_binds_wire_bytes_recipient_and_transport() {
    let mut runtime = auxiliary_runtime();
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    let bytes = packet.relay().encode(Direction::Forward).unwrap();
    assert!(matches!(
        runtime.enqueue_owned(
            MobileIdentity::Min34(1),
            bytes,
            ForwardOwner::Auxiliary(packet.clone()),
            NOW_MS
        ),
        Err(Failure::Wire)
    ));
    assert!(matches!(
        runtime.enqueue_owned(
            PEER,
            vec![0],
            ForwardOwner::Auxiliary(packet.clone()),
            NOW_MS
        ),
        Err(Failure::Wire)
    ));
    assert!(matches!(
        runtime.enqueue_traffic_owned(
            PEER,
            7,
            TrafficRelay::from_addressed(packet.relay()).unwrap(),
            ForwardOwner::Auxiliary(packet),
            NOW_MS
        ),
        Err(Failure::Wire)
    ));
    assert_eq!(runtime.pending.len(), 1);
}

#[test]
fn auxiliary_and_ordinary_sms_use_distinct_ids_in_the_same_recipient_bank() {
    let mut runtime = auxiliary_runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 0).unwrap();
    runtime.advance(NOW_MS).unwrap();
    let ordinary = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    let crate::l3::legacy_traffic::TrafficMessage::Relay(TrafficRelay {
        message: RelayMessage::Data { transaction_id, .. },
        ..
    }) = &ordinary.message
    else {
        panic!()
    };
    let sms_id = *transaction_id;
    runtime.traffic_queued(ordinary.token).unwrap();
    runtime
        .traffic_transmitted_at(ordinary.token, NOW_MS, NOW_MS)
        .unwrap();
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    assert_ne!(packet.transaction_id(), sms_id);
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    runtime
        .traffic_handed_off(offer.token, NOW_MS, NOW_MS)
        .unwrap();
    runtime
        .receive_traffic(
            callee,
            &TrafficRelay {
                message: RelayMessage::Accept {
                    transaction_id: packet.transaction_id(),
                },
                optional_octets: vec![],
            },
            NOW_MS + 1,
            0,
        )
        .unwrap();
    assert_eq!(
        runtime.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Accepted
    );
    assert_eq!(runtime.core().store().pending_for(PEER).unwrap().len(), 1);
}

#[test]
fn late_observation_preserves_actual_auxiliary_response_expiry() {
    let mut runtime = auxiliary_runtime();
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    confirm(&mut runtime, &packet);
    let message = runtime.arq_offer(|_| true).unwrap();
    runtime.arq_admitted(message.clone());
    runtime.arq_submitted_at(&message, NOW_MS + 2).unwrap();
    let observed = NOW_MS + crate::runtime::timers::SPACH_MS + 10;
    runtime
        .arq_transmitted_at(&message, NOW_MS + 3, observed)
        .unwrap();
    assert_eq!(
        runtime.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Expired
    );
    runtime
        .arq_ended(message, ArqOutcome::Confirmed, observed)
        .unwrap();
    let (mut carrier, voice) = radio(&runtime);
    carrier.stop().unwrap();
    runtime
        .settle_auxiliary_fences(&carrier, &voice, observed)
        .unwrap();
    assert_eq!(
        runtime.take_auxiliary_outcome().unwrap().reply.state,
        AuxiliaryState::Expired
    );
}

#[test]
fn completion_after_the_auxiliary_transmission_budget_cancels_without_arming_a_response() {
    let mut runtime = auxiliary_runtime();
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    confirm(&mut runtime, &packet);
    let message = runtime.arq_offer(|_| true).unwrap();
    runtime.arq_admitted(message.clone());
    runtime.arq_submitted_at(&message, NOW_MS + 2).unwrap();
    let late = NOW_MS + crate::runtime::timers::SPACH_MS;
    runtime.arq_transmitted_at(&message, late, late).unwrap();
    assert_eq!(
        runtime.auxiliary_state(&packet).unwrap(),
        AuxiliaryState::Submitted
    );
    assert!(!runtime.arq_current(&message));
    assert!(runtime.receive(&accept(&packet), late, 0).is_err());
    runtime
        .arq_ended(message, ArqOutcome::Failed, late)
        .unwrap();
    let (mut carrier, voice) = radio(&runtime);
    carrier.stop().unwrap();
    runtime
        .settle_auxiliary_fences(&carrier, &voice, late)
        .unwrap();
    assert_eq!(
        runtime.take_auxiliary_outcome().unwrap().reply.state,
        AuxiliaryState::Canceled
    );
}

#[test]
fn ordinary_sms_expiry_does_not_remove_a_concurrent_auxiliary_offer() {
    let mut runtime = auxiliary_runtime();
    runtime.receive(&submission(), 0, 0).unwrap();
    let callee = peer(2, 7, true);
    runtime.sync_traffic(vec![callee], 0).unwrap();
    runtime.advance(NOW_MS).unwrap();
    let ordinary = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    runtime.traffic_queued(ordinary.token).unwrap();
    runtime
        .traffic_transmitted_at(ordinary.token, NOW_MS, NOW_MS)
        .unwrap();
    let packet = runtime.send_auxiliary(PEER, relay(), NOW_MS).unwrap();
    runtime
        .advance(NOW_MS + traffic::SACCH_RESPONSE_MS)
        .unwrap();
    let offer = runtime.traffic_offer(callee, RelayChannel::Sacch).unwrap();
    let crate::l3::legacy_traffic::TrafficMessage::Relay(actual) = &offer.message else {
        panic!()
    };
    assert_eq!(
        *actual,
        TrafficRelay::from_addressed(packet.relay()).unwrap()
    );
    assert!(runtime.auxiliary_active(&packet));
}

fn durable_response(
    runtime: &mut SmsRuntime,
    version: ProtocolVersion,
) -> crate::core::store::incoming_journal::JournalHandle {
    use crate::core::store::incoming_journal::{IncomingBinding, JournalEvent, JournalLimits};
    let journal = runtime
        .core_mut()
        .store_mut()
        .begin_incoming_journal(
            IncomingBinding {
                peer: PEER,
                protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
                transaction_id: 0,
                version,
                context: Vec::new(),
            },
            JournalLimits {
                maximum_events: 32,
                maximum_context_octets: 1,
            },
            0,
        )
        .unwrap();
    let response = crate::l3::tsar::Response {
        transaction_id: 0,
        status: crate::l3::tsar::Status::InterSegmentTimeout,
        acknowledgments: crate::l3::tsar::AcknowledgmentMap::try_new(vec![true]).unwrap(),
        optional_services_support: None,
    };
    runtime
        .core_mut()
        .store_mut()
        .append_incoming_transition(
            &journal.handle,
            0,
            &JournalEvent::Advance,
            0,
            Some(&response),
        )
        .unwrap();
    journal.handle
}

#[test]
fn durable_response_is_settled_only_after_its_exact_auxiliary_radio_fence() {
    let mut runtime = auxiliary_runtime();
    durable_response(&mut runtime, ProtocolVersion::Pv1);
    runtime.dispatch_incoming_responses(NOW_MS, 0).unwrap();
    let packet = runtime
        .pending
        .back()
        .unwrap()
        .owner
        .auxiliary()
        .unwrap()
        .clone();
    confirm(&mut runtime, &packet);
    let phase = (runtime.allocation.phase(PEER).unwrap() + 1) % 32;
    while runtime.frame(21, phase).is_some() {
        runtime.submitted_at(NOW_MS + 2).unwrap();
    }
    runtime
        .auxiliary_transmitted(&packet, NOW_MS + 3, NOW_MS + 3)
        .unwrap();
    runtime.receive(&accept(&packet), NOW_MS + 4, 0).unwrap();
    runtime.dispatch_incoming_responses(NOW_MS + 4, 0).unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
            .len(),
        1
    );
    let (mut carrier, voice) = radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 5)
        .unwrap();
    assert!(runtime.take_auxiliary_outcome().is_none());
    runtime.dispatch_incoming_responses(NOW_MS + 5, 0).unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
            .is_empty()
    );
    assert!(runtime.incoming_response_jobs.is_empty());
    carrier.stop().unwrap();
}

#[test]
fn canceled_durable_response_retries_only_after_the_old_owner_is_fenced() {
    let mut runtime = auxiliary_runtime();
    durable_response(&mut runtime, ProtocolVersion::Pv1);
    runtime.dispatch_incoming_responses(NOW_MS, 0).unwrap();
    let packet = runtime
        .pending
        .back()
        .unwrap()
        .owner
        .auxiliary()
        .unwrap()
        .clone();
    runtime
        .sync_traffic(vec![peer(2, 7, true)], NOW_MS + 1)
        .unwrap();
    runtime.advance_auxiliary_jobs(NOW_MS + 1).unwrap();
    runtime.dispatch_incoming_responses(NOW_MS + 1, 0).unwrap();
    assert_eq!(runtime.incoming_response_jobs.len(), 1);
    let (mut carrier, voice) = radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 2)
        .unwrap();
    assert!(runtime.take_auxiliary_outcome().is_none());
    runtime
        .dispatch_incoming_responses(NOW_MS + RETRY_INTERVAL_MS, 0)
        .unwrap();
    assert_eq!(runtime.incoming_response_jobs.len(), 1);
    let replacement = runtime
        .traffic
        .pending
        .back()
        .unwrap()
        .owner
        .auxiliary()
        .unwrap();
    assert_ne!(*replacement, packet);
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
            .len(),
        1
    );
    carrier.stop().unwrap();
}

#[test]
fn durable_response_waits_for_a_compatible_peer_without_duplicating_jobs() {
    let mut runtime = super::runtime();
    durable_response(&mut runtime, ProtocolVersion::Pv2);
    runtime.dispatch_incoming_responses(NOW_MS, 0).unwrap();
    assert!(runtime.incoming_response_jobs.is_empty());
    runtime
        .core_mut()
        .register(
            &MessageSet {
                identity: PEER,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: ProtocolVersion::Pv1,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            1,
        )
        .unwrap();
    runtime
        .dispatch_incoming_responses(NOW_MS + RETRY_INTERVAL_MS, 0)
        .unwrap();
    assert!(runtime.incoming_response_jobs.is_empty());
    runtime
        .core_mut()
        .register(
            &MessageSet {
                identity: PEER,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: ProtocolVersion::Pv2,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            2,
        )
        .unwrap();
    runtime
        .dispatch_incoming_responses(NOW_MS + RETRY_INTERVAL_MS * 2, 0)
        .unwrap();
    runtime
        .dispatch_incoming_responses(NOW_MS + RETRY_INTERVAL_MS * 3, 0)
        .unwrap();
    assert_eq!(runtime.incoming_response_jobs.len(), 1);
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn durable_response_settlement_retry_recovers_an_uncertain_sql_commit() {
    use crate::core::store::incoming_journal::ResponseOutcome;
    let mut runtime = auxiliary_runtime();
    durable_response(&mut runtime, ProtocolVersion::Pv1);
    runtime.dispatch_incoming_responses(NOW_MS, 0).unwrap();
    let handle = runtime
        .core()
        .store()
        .pending_incoming_responses(32)
        .unwrap()[0]
        .handle
        .clone();
    let packet = runtime
        .pending
        .back()
        .unwrap()
        .owner
        .auxiliary()
        .unwrap()
        .clone();
    confirm(&mut runtime, &packet);
    let phase = (runtime.allocation.phase(PEER).unwrap() + 1) % 32;
    while runtime.frame(21, phase).is_some() {
        runtime.submitted_at(NOW_MS + 2).unwrap();
    }
    runtime
        .auxiliary_transmitted(&packet, NOW_MS + 3, NOW_MS + 3)
        .unwrap();
    runtime.receive(&accept(&packet), NOW_MS + 4, 0).unwrap();
    let (mut carrier, voice) = radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 5)
        .unwrap();
    runtime
        .core_mut()
        .store_mut()
        .settle_incoming_response(&handle, ResponseOutcome::Accepted, 0)
        .unwrap();
    runtime.dispatch_incoming_responses(NOW_MS + 6, 1).unwrap();
    assert!(runtime.incoming_response_jobs.is_empty());
    assert!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
            .is_empty()
    );
    carrier.stop().unwrap();
}
