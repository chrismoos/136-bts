use super::*;
use crate::l3::{
    relay::Teleservice,
    tsar::{self, AcknowledgmentWindow, MessagePlan, Segment},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const PEER: MobileIdentity = MobileIdentity::Min34(1);
const RECIPIENT: MobileIdentity = MobileIdentity::Min34(2);
const NOW_MS: u64 = crate::runtime::timers::SMS_ORIGINATION_MS;
const UNIX_ORIGIN_MS: i64 = 1000;
const MAX_CMT_USER_OCTETS: usize = 254;
const REASSEMBLY_MS: u64 = 300_000;
const RESTART_DOWNTIME_MS: u64 = 1000;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(PathBuf::from(format!(
            "/var/tmp/136bts-incoming-serving-{}-{}.sqlite3",
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
fn register(runtime: &mut SmsRuntime, peer: MobileIdentity, version: ProtocolVersion) {
    runtime
        .core_mut()
        .register(
            &MessageSet {
                identity: peer,
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
fn serving(database: &Database, unix_ms: i64) -> SmsRuntime {
    let mut store = StateStore::open(&database.0).unwrap();
    store
        .seed_once(&[
            ProvisionedSubscriber {
                identity: PEER,
                number: "1001".into(),
            },
            ProvisionedSubscriber {
                identity: RECIPIENT,
                number: "1002".into(),
            },
        ])
        .unwrap();
    let mut runtime = SmsRuntime::try_new(
        LocalMessaging::from_store(store).unwrap(),
        super::runtime().allocation,
        "incoming".into(),
    )
    .unwrap();
    runtime.start_incoming_sms(0, unix_ms).unwrap();
    register(&mut runtime, PEER, ProtocolVersion::Pv1);
    register(&mut runtime, RECIPIENT, ProtocolVersion::Pv1);
    runtime
}
fn segments() -> Vec<Segment> {
    let payload = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 42,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(vec![b'x'; MAX_CMT_USER_OCTETS]),
    }
    .encode()
    .unwrap();
    let MessagePlan::Segmented(plan) = tsar::plan_message(
        &payload,
        80,
        3,
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
    )
    .unwrap() else {
        panic!()
    };
    plan.segments().to_vec()
}
fn packet(segment: &Segment, index: usize) -> MessageSet {
    packet_with_id(segment, index, 11 + index as u8)
}
fn packet_with_id(segment: &Segment, index: usize, relay_id: u8) -> MessageSet {
    MessageSet {
        identity: PEER,
        messages: vec![
            AddressedRelay {
                message: RelayMessage::Data {
                    transaction_id: relay_id,
                    protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
                    payload: segment.encode(ProtocolVersion::Pv1).unwrap(),
                },
                addresses: RelayAddresses {
                    destination: (index == 0).then(|| AddressInfo {
                        number_type: 0,
                        numbering_plan: 1,
                        address: AddressValue::Tbcd("1002".into()),
                    }),
                    ..Default::default()
                },
            }
            .encode(Direction::Reverse)
            .unwrap(),
        ],
    }
}
fn journal(runtime: &SmsRuntime) -> crate::core::store::incoming_journal::IncomingJournal {
    runtime
        .core()
        .store()
        .active_incoming_journals(0, 2)
        .unwrap()
        .remove(0)
}
fn submit_all(runtime: &mut SmsRuntime, segments: &[Segment]) {
    for (index, segment) in segments.iter().enumerate() {
        runtime
            .receive(
                &packet(segment, index),
                NOW_MS + index as u64,
                if index == 0 { 1000 } else { 9_000_000 },
            )
            .unwrap();
    }
}

#[test]
fn incoming_segmented_sms_stores_full_cmt_before_final_response_and_accepts_replays_once() {
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    let segments = segments();
    submit_all(&mut runtime, &segments);
    let queued = runtime.core().store().pending_for(RECIPIENT).unwrap();
    assert_eq!(queued.len(), 1);
    let SmsMessage::Deliver {
        user_data: UserData::Ira(body),
        ..
    } = sms::decode_prefix(&queued[0].sms_pdu, Direction::Forward)
        .unwrap()
        .message
    else {
        panic!()
    };
    assert_eq!(body, vec![b'x'; MAX_CMT_USER_OCTETS]);
    let journal = journal(&runtime);
    let history = runtime
        .core()
        .store()
        .incoming_events(&journal.handle)
        .unwrap();
    assert_eq!(journal.revision as usize, segments.len() + 1);
    assert_eq!(
        history.last().unwrap().event,
        crate::core::store::incoming_journal::JournalEvent::ApplicationCommitted(queued[0].id)
    );
    assert_eq!(
        journal.observed_at_unix_ms,
        UNIX_ORIGIN_MS + NOW_MS as i64 + segments.len() as i64 - 1
    );
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()[0]
            .response
            .status,
        tsar::Status::Received
    );
    for pending in &runtime.pending {
        assert!(matches!(
            relay::decode_prefix(&pending.bytes, Direction::Forward)
                .unwrap()
                .message,
            RelayMessage::Accept { .. }
        ));
    }
    runtime
        .receive(
            &packet(segments.last().unwrap(), segments.len() - 1),
            NOW_MS + 10,
            0,
        )
        .unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .incoming_journal(&journal.handle)
            .unwrap()
            .revision,
        journal.revision
    );
    assert_eq!(
        runtime.core().store().pending_for(RECIPIENT).unwrap().len(),
        1
    );
    runtime.dispatch_incoming_responses(NOW_MS + 10, 0).unwrap();
    assert_eq!(runtime.incoming_response_jobs.len(), 1);
}

#[test]
fn incoming_partial_restart_preserves_reassembly_deadline_and_radio_clock_duration() {
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    runtime
        .receive(&packet(&segments()[0], 0), NOW_MS, 0)
        .unwrap();
    let handle = journal(&runtime).handle;
    drop(runtime);
    let mut runtime = serving(&database, UNIX_ORIGIN_MS + NOW_MS as i64 + 1000);
    runtime
        .advance(REASSEMBLY_MS - RESTART_DOWNTIME_MS - 1)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
            .is_empty()
    );
    runtime
        .advance(REASSEMBLY_MS - RESTART_DOWNTIME_MS + RETRY_INTERVAL_MS)
        .unwrap();
    let responses = runtime
        .core()
        .store()
        .pending_incoming_responses(32)
        .unwrap();
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0].response.status,
        tsar::Status::InterSegmentTimeout
    );
    assert_eq!(
        runtime
            .core()
            .store()
            .incoming_journal(&handle)
            .unwrap()
            .revision,
        2
    );
}

#[test]
fn incoming_completed_application_recovers_after_sql_insert_failure() {
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    let segments = segments();
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_sms BEFORE INSERT ON sms_queue BEGIN SELECT RAISE(ABORT,'SMS write failure'); END").unwrap();
    for (index, segment) in segments.iter().enumerate().take(segments.len() - 1) {
        runtime
            .receive(&packet(segment, index), NOW_MS + index as u64, 0)
            .unwrap();
    }
    let last = segments.len() - 1;
    assert!(
        runtime
            .receive(&packet(&segments[last], last), NOW_MS + last as u64, 0)
            .is_err()
    );
    assert!(
        runtime
            .core()
            .store()
            .pending_for(RECIPIENT)
            .unwrap()
            .is_empty()
    );
    assert!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
            .is_empty()
    );
    let handle = journal(&runtime).handle;
    drop(runtime);
    connection.execute_batch("DROP TRIGGER fail_sms").unwrap();
    let runtime = serving(&database, UNIX_ORIGIN_MS + NOW_MS as i64 + 1000);
    assert_eq!(
        runtime.core().store().pending_for(RECIPIENT).unwrap().len(),
        1
    );
    assert_eq!(
        runtime
            .core()
            .store()
            .incoming_journal(&handle)
            .unwrap()
            .revision as usize,
        segments.len() + 1
    );
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
fn incoming_post_commit_restart_reuses_original_sms_after_sender_renumbering() {
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    let segments = segments();
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_application BEFORE INSERT ON tsar_incoming_events WHEN NEW.event_type=2 BEGIN SELECT RAISE(ABORT,'application event failure'); END").unwrap();
    for (index, segment) in segments.iter().enumerate().take(segments.len() - 1) {
        runtime
            .receive(&packet(segment, index), NOW_MS + index as u64, 0)
            .unwrap();
    }
    let last = segments.len() - 1;
    assert!(
        runtime
            .receive(&packet(&segments[last], last), NOW_MS + last as u64, 0)
            .is_err()
    );
    let queued = runtime.core().store().pending_for(RECIPIENT).unwrap();
    assert_eq!(queued.len(), 1);
    let id = queued[0].id;
    runtime
        .core_mut()
        .save_subscriber(ProvisionedSubscriber {
            identity: PEER,
            number: "1003".into(),
        })
        .unwrap();
    drop(runtime);
    connection
        .execute_batch("DROP TRIGGER fail_application")
        .unwrap();
    let runtime = serving(&database, UNIX_ORIGIN_MS + NOW_MS as i64 + 1000);
    let queued = runtime.core().store().pending_for(RECIPIENT).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, id);
    assert_eq!(queued[0].sender_number, "1001");
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
fn incoming_frozen_parties_reject_mid_stream_provisioning_changes() {
    for sender_changed in [false, true] {
        let database = Database::new();
        let mut runtime = serving(&database, UNIX_ORIGIN_MS);
        let segments = segments();
        for (index, segment) in segments.iter().enumerate().take(segments.len() - 1) {
            runtime
                .receive(&packet(segment, index), NOW_MS + index as u64, 0)
                .unwrap();
        }
        let changed = if sender_changed { PEER } else { RECIPIENT };
        runtime
            .core_mut()
            .save_subscriber(ProvisionedSubscriber {
                identity: changed,
                number: "1003".into(),
            })
            .unwrap();
        if sender_changed {
            register(&mut runtime, PEER, ProtocolVersion::Pv1);
        } else {
            runtime
                .core_mut()
                .save_subscriber(ProvisionedSubscriber {
                    identity: MobileIdentity::Min34(3),
                    number: "1002".into(),
                })
                .unwrap();
        }
        let last = segments.len() - 1;
        runtime
            .receive(&packet(&segments[last], last), NOW_MS + last as u64, 0)
            .unwrap();
        assert!(
            runtime
                .core()
                .store()
                .pending_for(RECIPIENT)
                .unwrap()
                .is_empty()
        );
        assert!(
            runtime
                .core()
                .store()
                .pending_for(MobileIdentity::Min34(3))
                .unwrap()
                .is_empty()
        );
        let reply =
            relay::decode_prefix(&runtime.pending.back().unwrap().bytes, Direction::Forward)
                .unwrap()
                .message;
        assert!(matches!(reply, RelayMessage::Reject { .. }));
        assert_eq!(
            runtime
                .core()
                .store()
                .incoming_events(&journal(&runtime).handle)
                .unwrap()
                .last()
                .unwrap()
                .event,
            crate::core::store::incoming_journal::JournalEvent::ApplicationRejected
        );
    }
}

#[test]
fn incoming_legacy_or_unregistered_peer_cannot_create_a_segmented_stream() {
    let mut runtime = super::runtime();
    let segment = packet(&segments()[0], 0);
    runtime.receive(&segment, NOW_MS, UNIX_ORIGIN_MS).unwrap();
    assert!(
        runtime
            .core()
            .store()
            .active_incoming_journals(0, 32)
            .unwrap()
            .is_empty()
    );
    runtime.core_mut().delete_subscriber(PEER).unwrap();
    runtime
        .receive(&packet(&segments()[0], 1), NOW_MS + 1, UNIX_ORIGIN_MS)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .active_incoming_journals(0, 32)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn incoming_admission_requires_begin_destination_and_the_frozen_protocol_version() {
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    let segments = segments();
    runtime
        .receive(&packet(&segments[1], 1), NOW_MS, 0)
        .unwrap();
    runtime
        .receive(&packet(&segments[0], 1), NOW_MS + 1, 0)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .active_incoming_journals(0, 32)
            .unwrap()
            .is_empty()
    );
    register(&mut runtime, PEER, ProtocolVersion::Pv2);
    runtime
        .receive(&packet(&segments[0], 0), NOW_MS + 2, 0)
        .unwrap();
    let handle = journal(&runtime).handle;
    register(&mut runtime, PEER, ProtocolVersion::Pv1);
    runtime
        .receive(&packet(&segments[1], 1), NOW_MS + 3, 0)
        .unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .incoming_journal(&handle)
            .unwrap()
            .revision,
        1
    );
    assert!(matches!(
        relay::decode_prefix(&runtime.pending.back().unwrap().bytes, Direction::Forward)
            .unwrap()
            .message,
        RelayMessage::Reject {
            cause: UNSUPPORTED_PROTOCOL_CAUSE,
            ..
        }
    ));
}

#[test]
fn incoming_late_segment_persists_timeout_before_recovery_and_application_commit() {
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    let segments = segments();
    for (index, segment) in segments.iter().enumerate().take(segments.len() - 1) {
        runtime
            .receive(&packet(segment, index), NOW_MS + index as u64, 0)
            .unwrap();
    }
    let last = segments.len() - 1;
    runtime
        .receive(
            &packet(&segments[last], last),
            NOW_MS + REASSEMBLY_MS + segments.len() as u64,
            0,
        )
        .unwrap();
    let history = runtime
        .core()
        .store()
        .incoming_events(&journal(&runtime).handle)
        .unwrap();
    assert_eq!(
        history[last].event,
        crate::core::store::incoming_journal::JournalEvent::Advance
    );
    assert!(matches!(
        history[last + 1].event,
        crate::core::store::incoming_journal::JournalEvent::Segment(_)
    ));
    assert_eq!(
        runtime.core().store().pending_for(RECIPIENT).unwrap().len(),
        1
    );
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()[0]
            .response
            .status,
        tsar::Status::InterSegmentTimeout
    );
}

#[test]
fn incoming_prohibited_response_timeout_closes_the_stream_without_accepting_late_data() {
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    let mut segments = segments();
    let tsar::SegmentKind::Begin {
        services: Some(services),
        ..
    } = &mut segments[0].kind
    else {
        panic!()
    };
    services.acknowledgment_window = AcknowledgmentWindow::Prohibited;
    runtime
        .receive(&packet(&segments[0], 0), NOW_MS, 0)
        .unwrap();
    let late = NOW_MS + REASSEMBLY_MS + RETRY_INTERVAL_MS;
    runtime.advance(late).unwrap();
    runtime
        .receive(
            &packet(segments.last().unwrap(), segments.len() - 1),
            late + 1,
            0,
        )
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .pending_for(RECIPIENT)
            .unwrap()
            .is_empty()
    );
    assert!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        relay::decode_prefix(&runtime.pending.back().unwrap().bytes, Direction::Forward)
            .unwrap()
            .message,
        RelayMessage::Reject { .. }
    ));
    assert_eq!(journal(&runtime).revision, 2);
}

#[test]
fn incoming_recovery_rejects_future_history_and_never_rebases_a_live_epoch() {
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    runtime
        .receive(&packet(&segments()[0], 0), NOW_MS, 0)
        .unwrap();
    let journal = journal(&runtime);
    assert!(runtime.start_incoming_sms(NOW_MS + 1, 9_000_000).is_err());
    assert_eq!(
        runtime
            .core()
            .store()
            .incoming_journal(&journal.handle)
            .unwrap()
            .revision,
        1
    );
    drop(runtime);
    let core = LocalMessaging::from_store(StateStore::open(&database.0).unwrap()).unwrap();
    let mut runtime =
        SmsRuntime::try_new(core, super::runtime().allocation, "recovery".into()).unwrap();
    assert!(matches!(
        runtime.start_incoming_sms(0, UNIX_ORIGIN_MS),
        Err(IncomingSmsError::Recovery(_))
    ));
    assert_eq!(
        runtime
            .core()
            .store()
            .incoming_journal(&journal.handle)
            .unwrap()
            .revision,
        1
    );
    assert!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn incoming_stream_continues_on_traffic_and_routes_its_response_to_the_same_call() {
    use crate::l3::legacy_traffic::TrafficRelay;
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    let segments = segments();
    runtime
        .receive(&packet(&segments[0], 0), NOW_MS, 0)
        .unwrap();
    let handle = journal(&runtime).handle;
    const CALL_ID: u64 = 7;
    let peer = TrafficPeer {
        identity: PEER,
        call_id: CALL_ID,
        ready: true,
    };
    runtime.sync_traffic(vec![peer], NOW_MS + 1).unwrap();
    for (index, segment) in segments.iter().enumerate().skip(1) {
        let set = packet(segment, index);
        let addressed = relay::addressing::decode(&set.messages[0], Direction::Reverse).unwrap();
        let relay = TrafficRelay::from_addressed(&addressed.relay).unwrap();
        runtime
            .receive_traffic(peer, &relay, NOW_MS + index as u64 + 1, 0)
            .unwrap();
    }
    assert_eq!(journal(&runtime).handle, handle);
    assert_eq!(
        runtime.core().store().pending_for(RECIPIENT).unwrap().len(),
        1
    );
    runtime.dispatch_incoming_responses(NOW_MS + 10, 0).unwrap();
    let offer = runtime.traffic.pending.back().unwrap();
    assert_eq!(offer.offer.token.call_id, CALL_ID);
    let response = offer.owner.auxiliary().unwrap();
    assert_eq!(response.recipient(), PEER);
    let RelayMessage::Data {
        protocol, payload, ..
    } = &response.relay().message
    else {
        panic!()
    };
    assert_eq!(*protocol, HigherLayerProtocol::Segmented(Teleservice::Sms));
    assert_eq!(
        tsar::decode_response(payload, ProtocolVersion::Pv1)
            .unwrap()
            .response
            .status,
        tsar::Status::Received
    );
}

#[test]
fn confirmed_incoming_retirement_waits_for_response_guard_and_receive_drainage() {
    use crate::core::store::incoming_journal::ResponseOutcome;
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    let parts = segments();
    submit_all(&mut runtime, &parts);
    let old = journal(&runtime);
    let (mut carrier, voice) = super::auxiliary::radio(&runtime);
    carrier.stop().unwrap();
    let now = NOW_MS + crate::runtime::timers::SUPERFRAME_MS * 64;
    runtime.settle_reply_fences(&carrier, &voice, now).unwrap();
    runtime
        .settle_incoming_retirements(&carrier, now, |_| false)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .incoming_completion_response(&old.handle)
            .unwrap()
            .is_none()
    );
    let pending = runtime
        .core()
        .store()
        .pending_incoming_responses(32)
        .unwrap();
    assert!(!pending.is_empty());
    for response in pending {
        runtime
            .core_mut()
            .store_mut()
            .settle_incoming_response(
                &response.handle,
                ResponseOutcome::Accepted,
                UNIX_ORIGIN_MS + now as i64,
            )
            .unwrap();
    }
    assert!(
        runtime
            .core()
            .store()
            .incoming_completion_response(&old.handle)
            .unwrap()
            .is_some()
    );
    runtime
        .settle_incoming_retirements(&carrier, now, |_| false)
        .unwrap();
    let end = now + runtime.submission_retry_guard_ms;
    runtime
        .settle_incoming_retirements(&carrier, end - 1, |_| false)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .incoming_journal(&old.handle)
            .unwrap()
            .reuse_after_unix_ms
            .is_none()
    );
    runtime
        .settle_incoming_retirements(&carrier, end, |_| true)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .incoming_journal(&old.handle)
            .unwrap()
            .reuse_after_unix_ms
            .is_none()
    );
    runtime
        .settle_incoming_retirements(&carrier, end, |_| false)
        .unwrap();
    assert_eq!(
        runtime
            .core()
            .store()
            .incoming_journal(&old.handle)
            .unwrap()
            .reuse_after_unix_ms,
        Some(UNIX_ORIGIN_MS + end as i64)
    );
    assert_eq!(
        runtime.core().store().pending_for(RECIPIENT).unwrap().len(),
        1
    );
    assert!(runtime.latest_incoming_reply(&old.handle).is_none());
    for (index, part) in parts.iter().enumerate() {
        runtime
            .receive(&packet(part, index), end + 1 + index as u64, -1)
            .unwrap();
    }
    let new = journal(&runtime);
    assert_ne!(new.handle, old.handle);
    assert_eq!(
        runtime.core().store().pending_for(RECIPIENT).unwrap().len(),
        2
    );
}

#[test]
fn unconfirmed_incoming_completion_keeps_its_id_reserved() {
    use crate::core::store::incoming_journal::ResponseOutcome;
    for outcome in [ResponseOutcome::Rejected, ResponseOutcome::Expired] {
        let database = Database::new();
        let mut runtime = serving(&database, UNIX_ORIGIN_MS);
        submit_all(&mut runtime, &segments());
        let old = journal(&runtime);
        let (mut carrier, voice) = super::auxiliary::radio(&runtime);
        carrier.stop().unwrap();
        let now = NOW_MS + crate::runtime::timers::SUPERFRAME_MS * 64;
        runtime.settle_reply_fences(&carrier, &voice, now).unwrap();
        for response in runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
        {
            runtime
                .core_mut()
                .store_mut()
                .settle_incoming_response(&response.handle, outcome, UNIX_ORIGIN_MS + now as i64)
                .unwrap();
        }
        runtime
            .settle_incoming_retirements(&carrier, now, |_| false)
            .unwrap();
        runtime
            .settle_incoming_retirements(
                &carrier,
                now + runtime.submission_retry_guard_ms * 2,
                |_| false,
            )
            .unwrap();
        assert!(
            runtime
                .core()
                .store()
                .incoming_journal(&old.handle)
                .unwrap()
                .reuse_after_unix_ms
                .is_none()
        );
    }
}

#[test]
fn twelve_incoming_runtime_transfers_reuse_wire_ids_across_database_reopen() {
    use crate::core::store::incoming_journal::ResponseOutcome;
    let database = Database::new();
    let mut epoch = UNIX_ORIGIN_MS;
    let mut now = 0;
    let mut runtime = serving(&database, epoch);
    let parts = segments();
    let mut owners = Vec::new();
    for index in 0..12 {
        if index == 6 {
            epoch += now as i64 + 1;
            drop(runtime);
            runtime = serving(&database, epoch);
            now = 0;
        }
        for (offset, part) in parts.iter().enumerate() {
            now += 1;
            runtime.receive(&packet(part, offset), now, -1).unwrap();
        }
        let old = journal(&runtime);
        assert!(!owners.contains(&old.handle));
        owners.push(old.handle.clone());
        let (mut carrier, voice) = super::auxiliary::radio(&runtime);
        carrier.stop().unwrap();
        now += crate::runtime::timers::SUPERFRAME_MS * 64;
        runtime.settle_reply_fences(&carrier, &voice, now).unwrap();
        for response in runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()
        {
            runtime
                .core_mut()
                .store_mut()
                .settle_incoming_response(
                    &response.handle,
                    ResponseOutcome::Accepted,
                    epoch + now as i64,
                )
                .unwrap();
        }
        runtime
            .settle_incoming_retirements(&carrier, now, |_| false)
            .unwrap();
        now += runtime.submission_retry_guard_ms;
        runtime
            .settle_incoming_retirements(&carrier, now, |_| false)
            .unwrap();
        assert!(
            runtime
                .core()
                .store()
                .incoming_journal(&old.handle)
                .unwrap()
                .reuse_after_unix_ms
                .is_some()
        );
        assert_eq!(
            runtime.core().store().pending_for(RECIPIENT).unwrap().len(),
            index + 1
        );
    }
    assert_eq!(owners.len(), 12);
    for owner in owners {
        let history = runtime.core().store().incoming_snapshot(&owner).unwrap();
        assert!(!history.events.is_empty());
        assert!(history.journal.reuse_after_unix_ms.is_some());
    }
}

#[test]
fn incoming_restart_requires_a_fresh_guard_after_durable_response_closure() {
    use crate::core::store::incoming_journal::ResponseOutcome;
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    submit_all(&mut runtime, &segments());
    let old = journal(&runtime);
    let now = NOW_MS + crate::runtime::timers::SUPERFRAME_MS * 64;
    for response in runtime
        .core()
        .store()
        .pending_incoming_responses(32)
        .unwrap()
    {
        runtime
            .core_mut()
            .store_mut()
            .settle_incoming_response(
                &response.handle,
                ResponseOutcome::Accepted,
                UNIX_ORIGIN_MS + now as i64,
            )
            .unwrap();
    }
    drop(runtime);
    let mut regressed = serving(&database, UNIX_ORIGIN_MS + now as i64 - 1);
    let (mut probe, _) = super::auxiliary::radio(&regressed);
    assert!(matches!(
        regressed.settle_incoming_retirements(&probe, 0, |_| false),
        Err(Failure::Clock)
    ));
    probe.stop().unwrap();
    drop(regressed);
    let mut runtime = serving(&database, UNIX_ORIGIN_MS + now as i64);
    let (mut carrier, _) = super::auxiliary::radio(&runtime);
    let guard = runtime.submission_retry_guard_ms + crate::runtime::timers::SUPERFRAME_MS;
    runtime
        .settle_incoming_retirements(&carrier, 0, |_| false)
        .unwrap();
    runtime
        .settle_incoming_retirements(&carrier, guard, |_| false)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .incoming_journal(&old.handle)
            .unwrap()
            .reuse_after_unix_ms
            .is_none()
    );
    carrier.stop().unwrap();
    runtime
        .settle_incoming_retirements(&carrier, guard, |_| false)
        .unwrap();
    runtime
        .settle_incoming_retirements(&carrier, guard * 2 - 1, |_| false)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .incoming_journal(&old.handle)
            .unwrap()
            .reuse_after_unix_ms
            .is_none()
    );
    runtime
        .settle_incoming_retirements(&carrier, guard * 2, |_| false)
        .unwrap();
    assert!(
        runtime
            .core()
            .store()
            .incoming_journal(&old.handle)
            .unwrap()
            .reuse_after_unix_ms
            .is_some()
    );
}

#[test]
fn completion_proof_waits_for_a_duplicate_final_response_to_be_settled() {
    use crate::core::store::incoming_journal::ResponseOutcome;
    const DUPLICATE_RELAY_ID: u8 = 99;
    let database = Database::new();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    let parts = segments();
    submit_all(&mut runtime, &parts);
    let old = journal(&runtime);
    let accepted_at = NOW_MS + parts.len() as u64;
    for response in runtime
        .core()
        .store()
        .pending_incoming_responses(32)
        .unwrap()
    {
        runtime
            .core_mut()
            .store_mut()
            .settle_incoming_response(
                &response.handle,
                ResponseOutcome::Accepted,
                UNIX_ORIGIN_MS + accepted_at as i64,
            )
            .unwrap();
    }
    let proof = runtime
        .core()
        .store()
        .incoming_completion_response(&old.handle)
        .unwrap()
        .unwrap();
    runtime
        .receive(
            &packet_with_id(&parts[0], 0, DUPLICATE_RELAY_ID),
            accepted_at + 1,
            -1,
        )
        .unwrap();
    let pending = runtime
        .core()
        .store()
        .pending_incoming_responses(32)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        runtime
            .core()
            .store()
            .incoming_completion_response(&old.handle)
            .unwrap()
            .is_none()
    );
    runtime
        .core_mut()
        .store_mut()
        .settle_incoming_response(
            &pending[0].handle,
            ResponseOutcome::Accepted,
            UNIX_ORIGIN_MS + accepted_at as i64 + 2,
        )
        .unwrap();
    let renewed = runtime
        .core()
        .store()
        .incoming_completion_response(&old.handle)
        .unwrap()
        .unwrap();
    assert!(renewed.ordinal > proof.ordinal);
    assert!(renewed.settled_at_unix_ms > proof.settled_at_unix_ms);
}

#[test]
fn external_tsar_reassembly_survives_restart_and_commits_one_hub_job() {
    fn external(segment: &Segment, index: usize) -> MessageSet {
        let mut packet = packet(segment, index);
        if index == 0 {
            let mut addressed =
                crate::l3::relay::addressing::decode(&packet.messages[0], Direction::Reverse)
                    .unwrap()
                    .relay;
            addressed.addresses.destination.as_mut().unwrap().address =
                AddressValue::Tbcd("2000".into());
            packet.messages[0] = addressed.encode(Direction::Reverse).unwrap();
        }
        packet
    }
    let database = Database::new();
    let parts = segments();
    let mut runtime = serving(&database, UNIX_ORIGIN_MS);
    runtime.core_mut().enable_sms_hub(true);
    runtime
        .receive(&external(&parts[0], 0), NOW_MS, UNIX_ORIGIN_MS)
        .unwrap();
    assert_eq!(runtime.core().store().hub_queue_depth().unwrap(), 0);
    drop(runtime);
    let mut runtime = serving(
        &database,
        UNIX_ORIGIN_MS + NOW_MS as i64 + RESTART_DOWNTIME_MS as i64,
    );
    runtime.core_mut().enable_sms_hub(true);
    for (index, part) in parts.iter().enumerate().skip(1) {
        runtime
            .receive(&external(part, index), index as u64, UNIX_ORIGIN_MS)
            .unwrap();
    }
    assert_eq!(runtime.core().store().hub_queue_depth().unwrap(), 1);
    let id = runtime.core().store().operation_messages(10).unwrap()[0].id;
    let last = parts.len() - 1;
    runtime
        .receive(
            &external(&parts[last], last),
            parts.len() as u64 + 1,
            UNIX_ORIGIN_MS,
        )
        .unwrap();
    assert_eq!(runtime.core().store().hub_queue_depth().unwrap(), 1);
    assert_eq!(
        runtime.core().store().operation_messages(10).unwrap()[0].id,
        id
    );
    assert!(
        runtime
            .core()
            .store()
            .pending_for(RECIPIENT)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        runtime
            .core()
            .store()
            .pending_incoming_responses(32)
            .unwrap()[0]
            .response
            .status,
        tsar::Status::Received
    );
}
