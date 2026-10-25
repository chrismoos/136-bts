mod retirement_cases;
use super::*;
use crate::{
    core::store::{QueueState, SmsId},
    l3::{
        relay::Teleservice,
        tsar::{self, AcknowledgmentMap, Status},
    },
    link::tsar::SenderState,
};
use crate::{
    core::{registry::ProvisionedSubscriber, store::StateStore},
    l3::{
        registration::{AccessMessage, RegistrationType},
        sms::{Metadata, UserData},
    },
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
const PEER: MobileIdentity = MobileIdentity::Min34(2);
const NOW_MS: u64 = crate::runtime::timers::SMS_ORIGINATION_MS;
const UNIX_MS: i64 = 1000;
const SERVICE: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(PathBuf::from(format!(
            "/var/tmp/136bts-outgoing-serving-{}-{}.sqlite3",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn register(runtime: &mut SmsRuntime, version: ProtocolVersion) {
    runtime
        .core_mut()
        .register(
            &MessageSet {
                identity: PEER,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: version,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            0,
        )
        .unwrap();
}
fn fresh(database: Option<&Database>, unix: i64) -> SmsRuntime {
    let mut runtime = super::super::tests::runtime();
    if let Some(database) = database {
        let mut store = StateStore::open(&database.0).unwrap();
        store
            .seed_once(&[
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
        runtime = SmsRuntime::try_new(
            LocalMessaging::from_store(store).unwrap(),
            runtime.allocation,
            "outgoing".into(),
        )
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
                                protocol_version: ProtocolVersion::Pv1,
                            }
                            .encode()
                            .unwrap(),
                        ],
                    },
                    0,
                )
                .unwrap();
        }
    }
    register(&mut runtime, ProtocolVersion::Pv1);
    let config = crate::config::Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let crate::l3::broadcast::MandatoryBroadcast::AccessParameters(parameters) =
        &crate::runtime::serving::messages(&config)[1]
    else {
        panic!()
    };
    runtime
        .set_access_parameters(parameters, crate::runtime::timers::SUPERFRAME_MS)
        .unwrap();
    runtime.start_incoming_sms(0, unix).unwrap();
    runtime.start_outgoing_sms(0).unwrap();
    runtime
}
fn queue(runtime: &mut SmsRuntime) -> SmsId {
    queue_with_key(runtime, "outgoing")
}
fn queue_with_key(runtime: &mut SmsRuntime, key: &str) -> SmsId {
    let payload = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 42,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(vec![b'x'; 254]),
    }
    .encode()
    .unwrap();
    runtime
        .core_mut()
        .submit_complete_sms(
            MobileIdentity::Min34(1),
            &AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("1002".into()),
            },
            &payload,
            key,
            0,
        )
        .unwrap()
        .id
}
fn current(runtime: &SmsRuntime) -> AuxiliaryPacket {
    runtime.outgoing_sms.jobs.last().unwrap().packet.clone()
}
fn receipt(runtime: &mut SmsRuntime, packet: &AuxiliaryPacket, now: u64) {
    runtime.auxiliary_handed_off(packet, now, now).unwrap();
    runtime
        .auxiliary_transmitted(packet, now + 1, now + 1)
        .unwrap();
    runtime
        .receive(
            &MessageSet {
                identity: PEER,
                messages: vec![
                    RelayMessage::Accept {
                        transaction_id: packet.transaction_id(),
                    }
                    .encode(Direction::Reverse)
                    .unwrap(),
                ],
            },
            now + 2,
            -1,
        )
        .unwrap();
}
fn response(runtime: &SmsRuntime, lower_id: u8) -> MessageSet {
    let stream = &runtime.outgoing_sms.streams[0];
    MessageSet {
        identity: PEER,
        messages: vec![
            RelayMessage::Data {
                transaction_id: lower_id,
                protocol: SERVICE,
                payload: tsar::Response {
                    transaction_id: stream.snapshot.plan.segments()[0].transaction_id,
                    status: Status::Received,
                    acknowledgments: AcknowledgmentMap::try_new(vec![
                        true;
                        stream
                            .snapshot
                            .plan
                            .segments()
                            .len()
                    ])
                    .unwrap(),
                    optional_services_support: None,
                }
                .encode(ProtocolVersion::Pv1)
                .unwrap(),
            }
            .encode(Direction::Reverse)
            .unwrap(),
        ],
    }
}
#[test]
fn outgoing_long_sms_advances_on_matching_lower_receipts_and_completes_on_peer_response() {
    let mut runtime = fresh(None, UNIX_MS);
    let id = queue(&mut runtime);
    runtime.dispatch_outgoing_sms(0).unwrap();
    assert_eq!(runtime.outgoing_sms.streams[0].snapshot.revision(), 0);
    assert!(runtime.outgoing_sms.jobs.is_empty());
    runtime.dispatch_outgoing_sms(NOW_MS).unwrap();
    assert_eq!(runtime.outgoing_sms.streams[0].snapshot.revision(), 1);
    assert!(runtime.core().deliveries_for(PEER).unwrap().is_empty());
    let (mut carrier, voice) = super::super::tests::auxiliary::radio(&runtime);
    let mut now = NOW_MS + 1;
    let count = runtime.outgoing_sms.streams[0]
        .snapshot
        .plan
        .segments()
        .len();
    let mut reconstructed = Vec::new();
    for sequence in 0..count {
        let packet = current(&runtime);
        let RelayMessage::Data {
            protocol, payload, ..
        } = &packet.relay().message
        else {
            panic!()
        };
        assert_eq!(*protocol, SERVICE);
        let segment = tsar::decode_segment(payload, ProtocolVersion::Pv1)
            .unwrap()
            .segment;
        assert_eq!(segment.sequence_number as usize, sequence);
        reconstructed.extend(segment.segment_data);
        assert!(packet.relay().addresses.originating.is_some());
        assert!(runtime.take_auxiliary_outcome().is_none());
        receipt(&mut runtime, &packet, now);
        runtime
            .settle_auxiliary_fences(&carrier, &voice, now + 3)
            .unwrap();
        runtime.dispatch_outgoing_sms(now + 3).unwrap();
        assert!(!runtime.core().store().relay_accepted(id).unwrap());
        now += 4;
    }
    assert_eq!(
        runtime.outgoing_sms.streams[0].sender.state(),
        SenderState::AwaitingResponse
    );
    assert_eq!(
        reconstructed,
        runtime.core().store().pending_for(PEER).unwrap()[0].sms_pdu
    );
    let final_response = response(&runtime, 222);
    runtime.receive(&final_response, now, -1).unwrap();
    assert!(runtime.core().store().relay_accepted(id).unwrap());
    assert_eq!(
        runtime.core().store().state(id).unwrap(),
        QueueState::Delivered
    );
    let revision = runtime.outgoing_sms.streams[0].snapshot.revision();
    let retry = response(&runtime, 223);
    runtime.receive(&retry, now + 1, -1).unwrap();
    assert_eq!(
        runtime.outgoing_sms.streams[0].snapshot.revision(),
        revision
    );
    assert!(runtime.take_auxiliary_outcome().is_none());
    carrier.stop().unwrap();
}
#[test]
fn registration_change_retries_same_logical_fragment_only_after_old_owner_is_fenced() {
    let mut runtime = fresh(None, UNIX_MS);
    queue(&mut runtime);
    runtime.dispatch_outgoing_sms(NOW_MS).unwrap();
    let packet = current(&runtime);
    let deadline = runtime.outgoing_sms.streams[0]
        .sender
        .deadline_monotonic_ms();
    let original = runtime.outgoing_sms.streams[0].snapshot.clone();
    register(&mut runtime, ProtocolVersion::Pv1);
    runtime.advance_auxiliary_jobs(NOW_MS + 1).unwrap();
    runtime.dispatch_outgoing_sms(NOW_MS + 1).unwrap();
    assert_eq!(runtime.outgoing_sms.jobs.len(), 1);
    let (mut carrier, voice) = super::super::tests::auxiliary::radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 2)
        .unwrap();
    runtime.dispatch_outgoing_sms(NOW_MS + 2).unwrap();
    let replacement = current(&runtime);
    assert_ne!(replacement, packet);
    let RelayMessage::Data { payload: old, .. } = &packet.relay().message else {
        panic!()
    };
    let RelayMessage::Data { payload: new, .. } = &replacement.relay().message else {
        panic!()
    };
    assert_eq!(old, new);
    assert_eq!(
        runtime.outgoing_sms.streams[0].snapshot.revision(),
        original.revision()
    );
    assert_eq!(
        runtime.outgoing_sms.streams[0]
            .sender
            .deadline_monotonic_ms(),
        deadline
    );
    carrier.stop().unwrap();
}
#[test]
fn lower_timeout_waits_for_tsar_interval_without_failing_the_message_or_resetting_timer() {
    let mut runtime = fresh(None, UNIX_MS);
    let id = queue(&mut runtime);
    runtime.dispatch_outgoing_sms(NOW_MS).unwrap();
    let packet = current(&runtime);
    runtime
        .auxiliary_handed_off(&packet, NOW_MS + 1, NOW_MS + 1)
        .unwrap();
    runtime
        .auxiliary_transmitted(&packet, NOW_MS + 2, NOW_MS + 2)
        .unwrap();
    let deadline = runtime.outgoing_sms.streams[0]
        .sender
        .deadline_monotonic_ms()
        .unwrap();
    let lower_expired = NOW_MS + 2 + crate::runtime::timers::SPACH_MS;
    runtime.advance_auxiliary_jobs(lower_expired).unwrap();
    let (mut carrier, voice) = super::super::tests::auxiliary::radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, lower_expired)
        .unwrap();
    runtime.dispatch_outgoing_sms(lower_expired).unwrap();
    assert!(runtime.outgoing_sms.jobs.is_empty());
    runtime.dispatch_outgoing_sms(lower_expired + 1000).unwrap();
    assert!(runtime.outgoing_sms.jobs.is_empty());
    assert_eq!(
        runtime.outgoing_sms.streams[0]
            .sender
            .deadline_monotonic_ms(),
        Some(deadline)
    );
    assert_eq!(
        runtime.core().store().state(id).unwrap(),
        QueueState::Queued
    );
    runtime
        .dispatch_outgoing_sms(deadline - UNIX_MS as u64)
        .unwrap();
    assert_eq!(runtime.outgoing_sms.streams[0].snapshot.revision(), 3);
    assert_eq!(runtime.outgoing_sms.jobs.len(), 1);
    assert_eq!(runtime.outgoing_sms.jobs[0].submission_ordinal, 2);
    carrier.stop().unwrap();
}
#[test]
fn restart_recovers_same_fragment_and_deadline_with_fresh_lower_ownership() {
    let database = Database::new();
    let mut runtime = fresh(Some(&database), UNIX_MS);
    queue(&mut runtime);
    runtime.dispatch_outgoing_sms(NOW_MS).unwrap();
    let old = current(&runtime);
    let before = runtime.outgoing_sms.streams[0].snapshot.clone();
    let deadline = runtime.outgoing_sms.streams[0]
        .sender
        .deadline_monotonic_ms();
    drop(runtime);
    let mut recovered = fresh(Some(&database), UNIX_MS + NOW_MS as i64 + 1000);
    assert_eq!(
        recovered.outgoing_sms.streams[0].snapshot.handle,
        before.handle
    );
    recovered.dispatch_outgoing_sms(NOW_MS).unwrap();
    let packet = current(&recovered);
    assert_ne!(packet, old);
    assert_eq!(
        recovered.outgoing_sms.streams[0].snapshot.revision(),
        before.revision()
    );
    assert_eq!(
        recovered.outgoing_sms.streams[0]
            .sender
            .deadline_monotonic_ms(),
        deadline
    );
    let RelayMessage::Data {
        payload: old_payload,
        ..
    } = &old.relay().message
    else {
        panic!()
    };
    let RelayMessage::Data { payload, .. } = &packet.relay().message else {
        panic!()
    };
    assert_eq!(payload, old_payload);
}
#[test]
fn failed_submission_journal_write_withdraws_all_new_software_sources() {
    let database = Database::new();
    let mut runtime = fresh(Some(&database), UNIX_MS);
    queue(&mut runtime);
    let sql = rusqlite::Connection::open(&database.0).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_event BEFORE INSERT ON tsar_outgoing_events BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
    assert!(runtime.dispatch_outgoing_sms(NOW_MS).is_err());
    assert!(
        runtime
            .pending
            .iter()
            .all(|pending| pending.owner.auxiliary().is_none())
    );
    assert!(
        runtime
            .traffic
            .pending
            .iter()
            .all(|pending| pending.owner.auxiliary().is_none())
    );
    assert!(runtime.outgoing_sms.jobs.is_empty());
    assert_eq!(
        runtime
            .core_mut()
            .store_mut()
            .outgoing_journals(0, 1)
            .unwrap()[0]
            .revision(),
        0
    );
    assert_eq!(
        runtime.outgoing_sms.streams[0].sender.state(),
        SenderState::Prepared
    );
}
#[test]
fn unsolicited_or_legacy_response_cannot_complete_an_outgoing_message() {
    let mut runtime = fresh(None, UNIX_MS);
    let id = queue(&mut runtime);
    runtime.dispatch_outgoing_sms(NOW_MS).unwrap();
    let premature = response(&runtime, 222);
    runtime.receive(&premature, NOW_MS + 1, 0).unwrap();
    assert_eq!(runtime.outgoing_sms.streams[0].snapshot.revision(), 1);
    assert!(!runtime.core().store().relay_accepted(id).unwrap());
    register(&mut runtime, ProtocolVersion::Is136RevA);
    let invalid = response(&runtime, 223);
    runtime.receive(&invalid, NOW_MS + 2, 0).unwrap();
    assert_eq!(runtime.outgoing_sms.streams[0].snapshot.revision(), 1);
    assert!(!runtime.core().store().relay_accepted(id).unwrap());
}

#[test]
fn final_peer_receipt_withdraws_last_queued_fragment_but_keeps_owner_until_fenced() {
    let mut runtime = fresh(None, UNIX_MS);
    let id = queue(&mut runtime);
    runtime.dispatch_outgoing_sms(NOW_MS).unwrap();
    let (mut carrier, voice) = super::super::tests::auxiliary::radio(&runtime);
    let count = runtime.outgoing_sms.streams[0]
        .snapshot
        .plan
        .segments()
        .len();
    let mut now = NOW_MS + 1;
    for _ in 1..count {
        let packet = current(&runtime);
        receipt(&mut runtime, &packet, now);
        runtime
            .settle_auxiliary_fences(&carrier, &voice, now + 3)
            .unwrap();
        runtime.dispatch_outgoing_sms(now + 3).unwrap();
        now += 4;
    }
    let last = current(&runtime);
    assert!(
        runtime
            .pending
            .iter()
            .any(|pending| pending.owner.auxiliary() == Some(&last))
    );
    runtime.receive(&response(&runtime, 222), now, 0).unwrap();
    assert!(runtime.core().store().relay_accepted(id).unwrap());
    assert!(
        runtime
            .pending
            .iter()
            .all(|pending| pending.owner.auxiliary() != Some(&last))
    );
    assert!(runtime.owns_outgoing_fragment(&last));
    assert!(runtime.take_auxiliary_outcome().is_none());
    runtime
        .settle_auxiliary_fences(&carrier, &voice, now + 1)
        .unwrap();
    runtime.dispatch_outgoing_sms(now + 1).unwrap();
    assert!(!runtime.owns_outgoing_fragment(&last));
    assert!(runtime.take_auxiliary_outcome().is_none());
    carrier.stop().unwrap();
}

#[test]
fn a_smaller_reselected_route_holds_the_frozen_plan_until_a_compatible_route_returns() {
    let mut runtime = fresh(None, UNIX_MS);
    queue(&mut runtime);
    runtime
        .sync_traffic(
            vec![TrafficPeer {
                identity: PEER,
                call_id: 7,
                ready: true,
            }],
            0,
        )
        .unwrap();
    runtime.dispatch_outgoing_sms(NOW_MS).unwrap();
    let first = current(&runtime);
    assert_eq!(first.transport(), AuxiliaryTransport::Traffic);
    assert!(first.relay().encode(Direction::Forward).is_err());
    let before = runtime.outgoing_sms.streams[0].snapshot.clone();
    let deadline = runtime.outgoing_sms.streams[0]
        .sender
        .deadline_monotonic_ms();
    runtime.sync_traffic(Vec::new(), NOW_MS + 1).unwrap();
    let (mut carrier, voice) = super::super::tests::auxiliary::radio(&runtime);
    runtime
        .settle_auxiliary_fences(&carrier, &voice, NOW_MS + 1)
        .unwrap();
    runtime.dispatch_outgoing_sms(NOW_MS + 1).unwrap();
    assert!(runtime.outgoing_sms.jobs.is_empty());
    assert_eq!(runtime.outgoing_sms.streams[0].snapshot.plan, before.plan);
    assert_eq!(
        runtime.outgoing_sms.streams[0].snapshot.revision(),
        before.revision()
    );
    runtime
        .sync_traffic(
            vec![TrafficPeer {
                identity: PEER,
                call_id: 8,
                ready: true,
            }],
            NOW_MS + 2,
        )
        .unwrap();
    runtime.dispatch_outgoing_sms(NOW_MS + 2).unwrap();
    assert_ne!(current(&runtime), first);
    assert_eq!(
        runtime.outgoing_sms.streams[0].snapshot.revision(),
        before.revision()
    );
    assert_eq!(
        runtime.outgoing_sms.streams[0]
            .sender
            .deadline_monotonic_ms(),
        deadline
    );
    carrier.stop().unwrap();
}
