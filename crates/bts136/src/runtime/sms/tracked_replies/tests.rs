use super::*;
use crate::{
    core::store::incoming_journal::{IncomingBinding, JournalLimits},
    l3::{
        broadcast::ProtocolVersion,
        registration::{AccessMessage, RegistrationType},
        relay::{HigherLayerProtocol, Teleservice},
        tsar::{Segment, SegmentKind},
    },
    link::rach::MessageSet,
};
const PEER: MobileIdentity = MobileIdentity::Min34(2);
const NOW_MS: u64 = 100;
fn runtime() -> SmsRuntime {
    let mut runtime = super::super::tests::runtime();
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
        .core_mut()
        .store_mut()
        .begin_incoming_journal(
            IncomingBinding {
                peer: PEER,
                protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
                transaction_id: 3,
                version: ProtocolVersion::Pv1,
                context: Vec::new(),
            },
            JournalLimits {
                maximum_events: 32,
                maximum_context_octets: 32,
            },
            0,
        )
        .unwrap();
    runtime
}
fn incoming() -> RelayMessage {
    RelayMessage::Data {
        transaction_id: 11,
        protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
        payload: Segment {
            transaction_id: 3,
            sequence_number: 0,
            segment_data: vec![1],
            kind: SegmentKind::Begin {
                message_length: None,
                services: None,
            },
        }
        .encode(ProtocolVersion::Pv1)
        .unwrap(),
    }
}
fn enqueue(runtime: &mut SmsRuntime, route: Route) -> ReplyPacket {
    runtime
        .enqueue_response(
            PEER,
            &incoming(),
            RelayMessage::Accept { transaction_id: 11 }
                .encode(Direction::Forward)
                .unwrap(),
            route,
            NOW_MS,
        )
        .unwrap();
    runtime.tracked_replies.jobs.last().unwrap().packet.clone()
}
#[test]
fn control_reply_keeps_private_owner_without_logging_and_separates_handoff_tail_and_fence() {
    let mut runtime = runtime();
    let packet = enqueue(&mut runtime, Route::Control);
    assert_eq!(packet.status().unwrap(), ReplyStatus::default());
    let phase = runtime.allocation.phase(PEER).unwrap();
    assert!(runtime.frame(20, phase).is_some());
    let Completion::Reply(selected) = runtime.completion() else {
        panic!()
    };
    assert_eq!(selected, packet);
    assert!(
        matches!(runtime.submitted_at(NOW_MS).unwrap(),Completion::Reply(owner) if owner==packet)
    );
    assert_eq!(
        packet.status().unwrap().submitted_monotonic_ms,
        Some(NOW_MS)
    );
    assert_eq!(packet.status().unwrap().completed_monotonic_ms, None);
    runtime
        .reply_transmitted(&packet, NOW_MS + 1, NOW_MS + 2)
        .unwrap();
    let (mut carrier, voice) = super::super::tests::auxiliary::radio(&runtime);
    runtime
        .settle_reply_fences(&carrier, &voice, NOW_MS + 2)
        .unwrap();
    assert_eq!(
        packet.status().unwrap(),
        ReplyStatus {
            submitted_monotonic_ms: Some(NOW_MS),
            completed_monotonic_ms: Some(NOW_MS + 1),
            fenced: true
        }
    );
    carrier.stop().unwrap();
}
#[test]
fn cancellation_fences_an_unsent_reply_without_inventing_a_transmission() {
    let mut runtime = runtime();
    let packet = enqueue(&mut runtime, Route::Control);
    runtime
        .sync_traffic(
            vec![TrafficPeer {
                identity: PEER,
                call_id: 7,
                ready: true,
            }],
            NOW_MS + 1,
        )
        .unwrap();
    assert!(
        runtime
            .traffic
            .pending
            .iter()
            .all(|pending| pending.owner.tracked_reply().is_none())
    );
    let (mut carrier, voice) = super::super::tests::auxiliary::radio(&runtime);
    runtime
        .settle_reply_fences(&carrier, &voice, NOW_MS + 1)
        .unwrap();
    assert_eq!(
        packet.status().unwrap(),
        ReplyStatus {
            fenced: true,
            ..ReplyStatus::default()
        }
    );
    carrier.stop().unwrap();
}
#[test]
fn equal_wire_replies_keep_distinct_owners_and_foreign_completion_cannot_change_them() {
    let mut first = runtime();
    let old = enqueue(&mut first, Route::Control);
    let replacement = enqueue(&mut first, Route::Control);
    assert_ne!(old, replacement);
    assert_eq!(old.bytes(), replacement.bytes());
    first.reply_handed_off(&old, NOW_MS, NOW_MS).unwrap();
    let mut other = runtime();
    let foreign = enqueue(&mut other, Route::Control);
    other
        .reply_transmitted(&old, NOW_MS + 1, NOW_MS + 1)
        .unwrap();
    assert_eq!(old.status().unwrap().completed_monotonic_ms, None);
    assert_eq!(foreign.status().unwrap(), ReplyStatus::default());
    assert!(
        first
            .reply_transmitted(&replacement, NOW_MS + 1, NOW_MS + 1)
            .is_err()
    );
    assert!(
        first
            .reply_transmitted(&old, NOW_MS - 1, NOW_MS + 1)
            .is_err()
    );
    assert_eq!(replacement.status().unwrap(), ReplyStatus::default());
}
#[test]
fn traffic_completion_requires_exact_token_and_does_not_fence_retained_sources() {
    let mut runtime = runtime();
    let peer = TrafficPeer {
        identity: PEER,
        call_id: 7,
        ready: true,
    };
    runtime.sync_traffic(vec![peer], 0).unwrap();
    let packet = enqueue(&mut runtime, Route::Traffic(peer.call_id));
    let offer = runtime.traffic_offer(peer, RelayChannel::Facch).unwrap();
    runtime.traffic_queued(offer.token).unwrap();
    let (mut carrier, voice) = super::super::tests::auxiliary::radio(&runtime);
    runtime
        .settle_reply_fences(&carrier, &voice, NOW_MS)
        .unwrap();
    assert!(!packet.status().unwrap().fenced);
    runtime
        .traffic_handed_off(offer.token, NOW_MS, NOW_MS + 1)
        .unwrap();
    runtime
        .traffic_transmitted_at(offer.token, NOW_MS + 2, NOW_MS + 3)
        .unwrap();
    assert_eq!(
        packet.status().unwrap().completed_monotonic_ms,
        Some(NOW_MS + 2)
    );
    assert!(!packet.status().unwrap().fenced);
    runtime
        .settle_reply_fences(&carrier, &voice, NOW_MS + 3)
        .unwrap();
    assert!(packet.status().unwrap().fenced);
    carrier.stop().unwrap();
}
#[test]
fn queued_sources_prevent_a_fence_and_shutdown_preserves_their_unsent_status() {
    let mut runtime = runtime();
    let packet = enqueue(&mut runtime, Route::Control);
    let (mut carrier, voice) = super::super::tests::auxiliary::radio(&runtime);
    runtime
        .settle_reply_fences(&carrier, &voice, NOW_MS)
        .unwrap();
    assert!(!packet.status().unwrap().fenced);
    runtime.shutdown(NOW_MS + 1).unwrap();
    carrier.stop().unwrap();
    runtime
        .settle_reply_fences(&carrier, &voice, NOW_MS + 1)
        .unwrap();
    assert_eq!(
        packet.status().unwrap(),
        ReplyStatus {
            fenced: true,
            ..ReplyStatus::default()
        }
    );
}

#[test]
fn a_reply_fence_waits_for_the_actual_carrier_queue_after_software_source_removal() {
    use crate::radio::{PreparedRadio, duplex::Duplex, receiver, timed_tx};
    let mut runtime = runtime();
    let packet = enqueue(&mut runtime, Route::Control);
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
                max_queued_blocks: 4,
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
        4,
    )
    .unwrap();
    carrier
        .start_continuous(observed.hardware_tick + settings.tx_tick_rate_hz * 2, None)
        .unwrap();
    carrier
        .submit_slot(0, &[false; crate::phy::traffic::SLOT_BITS], packet.clone())
        .unwrap();
    let phase = runtime.allocation.phase(PEER).unwrap();
    assert!(runtime.frame(20, phase).is_some());
    runtime.submitted_at(NOW_MS).unwrap();
    let (mut helper, voice) = super::super::tests::auxiliary::radio(&runtime);
    helper.stop().unwrap();
    runtime
        .settle_reply_fences(&carrier, &voice, NOW_MS)
        .unwrap();
    assert!(!packet.status().unwrap().fenced);
    assert!(runtime.pending.is_empty());
    carrier.stop().unwrap();
    runtime
        .settle_reply_fences(&carrier, &voice, NOW_MS + 1)
        .unwrap();
    assert!(packet.status().unwrap().fenced);
    assert_eq!(packet.status().unwrap().completed_monotonic_ms, None);
}

#[test]
fn an_unknown_outgoing_response_cannot_borrow_an_incoming_stream_owner_with_the_same_id() {
    let mut runtime = runtime();
    let message = RelayMessage::Data {
        transaction_id: 11,
        protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
        payload: crate::l3::tsar::Response {
            transaction_id: 3,
            status: crate::l3::tsar::Status::Received,
            acknowledgments: crate::l3::tsar::AcknowledgmentMap::try_new(vec![true]).unwrap(),
            optional_services_support: None,
        }
        .encode(ProtocolVersion::Pv1)
        .unwrap(),
    };
    assert!(runtime.reply_group(PEER, &message).unwrap().is_none());
    runtime
        .enqueue_response(
            PEER,
            &message,
            RelayMessage::Reject {
                transaction_id: 11,
                cause: 3,
            }
            .encode(Direction::Forward)
            .unwrap(),
            Route::Control,
            NOW_MS,
        )
        .unwrap();
    assert!(runtime.tracked_replies.jobs.is_empty());
    assert!(matches!(runtime.pending[0].owner, ForwardOwner::Reply));
}
