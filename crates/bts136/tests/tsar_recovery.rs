use bts136::{
    core::{
        messaging::LocalMessaging,
        registry::ProvisionedSubscriber,
        store::{
            StateStore, StoreError,
            incoming_journal::{IncomingBinding, JournalEvent, JournalHandle, JournalLimits},
        },
    },
    l3::{
        address::{AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        relay::{HigherLayerProtocol, Teleservice},
        sms::{Metadata, SmsMessage, UserData},
        tsar::{
            self, AcknowledgmentWindow, MessagePlan, OptionalServices, Segment, SegmentKind, Status,
        },
    },
    link::{
        rach::MobileIdentity,
        tsar::{ApplicationOutcome, IncomingError, IncomingLimits, ReceiverState},
    },
    runtime::sms::{
        tsar_ingress::{self, Transition},
        tsar_recovery::{self, RecoveryClock},
        tsar_submission::{self, SubmissionContext},
    },
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
const PEER: MobileIdentity = MobileIdentity::Min34(1);
const SERVICE: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
const LIMITS: IncomingLimits = IncomingLimits {
    maximum_message_octets: 1024,
    maximum_reserved_payload_octets: 16 * 1024,
};
const GUARD_MS: u64 = 100;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(PathBuf::from(format!(
            "/var/tmp/136bts-tsar-recovery-{}-{}.sqlite",
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
fn create(store: &mut StateStore, id: u8, time: i64) -> JournalHandle {
    store
        .begin_incoming_journal(
            IncomingBinding {
                peer: PEER,
                protocol: SERVICE,
                transaction_id: id,
                version: ProtocolVersion::Pv1,
                context: b"destination 1002".to_vec(),
            },
            JournalLimits {
                maximum_events: 64,
                maximum_context_octets: 128,
            },
            time,
        )
        .unwrap()
        .handle
}
fn begin(id: u8) -> Segment {
    Segment {
        transaction_id: id,
        sequence_number: 0,
        segment_data: b"ab".to_vec(),
        kind: SegmentKind::Begin {
            message_length: Some(4),
            services: Some(OptionalServices {
                acknowledgment_window: AcknowledgmentWindow::Last,
                ..OptionalServices::default()
            }),
        },
    }
}
fn end(id: u8) -> Segment {
    Segment {
        transaction_id: id,
        sequence_number: 1,
        segment_data: b"cd".to_vec(),
        kind: SegmentKind::End {
            message_length: None,
            crc: None,
        },
    }
}
fn provision(store: &mut StateStore) {
    store
        .reconcile(&[
            ProvisionedSubscriber {
                identity: PEER,
                number: "1001".into(),
            },
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(2),
                number: "1002".into(),
            },
        ])
        .unwrap();
}
fn destination() -> AddressInfo {
    AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd("1002".into()),
    }
}
fn complete_cmt(store: &mut StateStore) -> (JournalHandle, Vec<u8>, u32) {
    provision(store);
    let handle = create(store, 2, 10);
    let pdu = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 1,
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
    let MessagePlan::Segmented(plan) = tsar::plan_message(
        &pdu,
        128,
        2,
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
    )
    .unwrap() else {
        panic!()
    };
    for (index, segment) in plan.segments().iter().enumerate() {
        store
            .append_incoming_event(
                &handle,
                index as u32,
                &JournalEvent::Segment(segment.clone()),
                20 + index as i64,
            )
            .unwrap();
    }
    (handle, pdu, plan.segments().len() as u32)
}
#[test]
fn mixed_stream_histories_restore_with_exact_context_and_restart_guards() {
    let mut store = StateStore::open(":memory:").unwrap();
    let a = create(&mut store, 0, 100);
    store
        .append_incoming_event(&a, 0, &JournalEvent::Segment(begin(0)), 100)
        .unwrap();
    store
        .append_incoming_event(&a, 1, &JournalEvent::Segment(end(0)), 200)
        .unwrap();
    store
        .append_incoming_event(&a, 2, &JournalEvent::ApplicationRejected, 250)
        .unwrap();
    let b = create(&mut store, 1, 10);
    store
        .append_incoming_event(&b, 0, &JournalEvent::Segment(begin(1)), 10)
        .unwrap();
    let mut restored = tsar_recovery::restore(
        &store,
        PEER,
        SERVICE,
        LIMITS,
        GUARD_MS,
        RecoveryClock {
            now_monotonic_ms: 5000,
            now_unix_ms: 1000,
        },
        &[a.clone(), b.clone()],
    )
    .unwrap();
    assert_eq!(restored.transactions[0].journal.handle, a);
    assert_eq!(restored.transactions[1].journal.handle, b);
    assert_eq!(restored.transactions[0].journal.revision, 3);
    assert_eq!(
        restored
            .bank
            .application_outcome(&restored.transactions[0].transaction)
            .unwrap(),
        Some(ApplicationOutcome::Rejected)
    );
    assert_eq!(
        restored
            .bank
            .receiver(&restored.transactions[1].transaction)
            .unwrap()
            .deadline_monotonic_ms(),
        Some(304_010)
    );
    let packet = tsar::DecodedSegment {
        segment: begin(3),
        unparsed_octets: Vec::new(),
    };
    assert!(matches!(
        restored
            .bank
            .begin(PEER, SERVICE, &packet, ProtocolVersion::Pv1, 5000),
        Err(IncomingError::Guarded)
    ));
    assert!(
        restored
            .bank
            .begin(PEER, SERVICE, &packet, ProtocolVersion::Pv1, 5100)
            .is_ok()
    );
    assert_eq!(store.incoming_events(&a).unwrap().len(), 3);
}
#[test]
fn downtime_expiry_and_historical_timer_actions_are_not_rearmed_at_restore() {
    let mut store = StateStore::open(":memory:").unwrap();
    let handle = create(&mut store, 0, 0);
    store
        .append_incoming_event(&handle, 0, &JournalEvent::Segment(begin(0)), 0)
        .unwrap();
    let mut restored = tsar_recovery::restore(
        &store,
        PEER,
        SERVICE,
        LIMITS,
        GUARD_MS,
        RecoveryClock {
            now_monotonic_ms: 500_000,
            now_unix_ms: 300_001,
        },
        std::slice::from_ref(&handle),
    )
    .unwrap();
    let transaction = &restored.transactions[0].transaction;
    assert_eq!(
        restored
            .bank
            .receiver(transaction)
            .unwrap()
            .deadline_monotonic_ms(),
        Some(499_999)
    );
    assert_eq!(
        restored
            .bank
            .advance(transaction, 500_000)
            .unwrap()
            .unwrap()
            .status,
        Status::InterSegmentTimeout
    );
    store
        .append_incoming_event(&handle, 1, &JournalEvent::Advance, 300_001)
        .unwrap();
    store
        .append_incoming_event(&handle, 2, &JournalEvent::Segment(end(0)), 300_002)
        .unwrap();
    let restored = tsar_recovery::restore(
        &store,
        PEER,
        SERVICE,
        LIMITS,
        GUARD_MS,
        RecoveryClock {
            now_monotonic_ms: 600_000,
            now_unix_ms: 400_000,
        },
        &[handle],
    )
    .unwrap();
    assert_eq!(
        restored
            .bank
            .receiver(&restored.transactions[0].transaction)
            .unwrap()
            .state(),
        ReceiverState::Complete
    );
    assert_eq!(
        restored
            .bank
            .pending_message(&restored.transactions[0].transaction)
            .unwrap()
            .unwrap()
            .bytes(),
        b"abcd"
    );
}
#[test]
fn invalid_clock_binding_retired_history_and_duplicate_ids_fail_closed() {
    let mut store = StateStore::open(":memory:").unwrap();
    let handle = create(&mut store, 0, 10);
    let clock = RecoveryClock {
        now_monotonic_ms: 1000,
        now_unix_ms: 1000,
    };
    assert!(matches!(
        tsar_recovery::restore(
            &store,
            PEER,
            SERVICE,
            LIMITS,
            GUARD_MS,
            RecoveryClock {
                now_monotonic_ms: 0,
                ..clock
            },
            std::slice::from_ref(&handle)
        ),
        Err(tsar_recovery::Error::ClockRange)
    ));
    assert!(matches!(
        tsar_recovery::restore(
            &store,
            PEER,
            SERVICE,
            LIMITS,
            GUARD_MS,
            RecoveryClock {
                now_unix_ms: 9,
                ..clock
            },
            std::slice::from_ref(&handle)
        ),
        Err(tsar_recovery::Error::ClockRange)
    ));
    assert!(matches!(
        tsar_recovery::restore(
            &store,
            MobileIdentity::Min34(2),
            SERVICE,
            LIMITS,
            GUARD_MS,
            clock,
            std::slice::from_ref(&handle)
        ),
        Err(tsar_recovery::Error::Binding)
    ));
    assert!(matches!(
        tsar_recovery::restore(
            &store,
            PEER,
            SERVICE,
            LIMITS,
            GUARD_MS,
            clock,
            &[handle.clone(), handle.clone()]
        ),
        Err(tsar_recovery::Error::Ownership(IncomingError::Occupied))
    ));
    store
        .retire_incoming_journal_fenced(&handle, 0, 10, GUARD_MS)
        .unwrap();
    assert!(matches!(
        tsar_recovery::restore(&store, PEER, SERVICE, LIMITS, GUARD_MS, clock, &[handle]),
        Err(tsar_recovery::Error::Retired)
    ));
}
#[test]
fn post_commit_restart_recovers_the_same_sms_and_does_not_redeliver_a_settled_application() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    let (handle, pdu, revision) = complete_cmt(&mut store);
    let mut restored = tsar_recovery::restore(
        &store,
        PEER,
        SERVICE,
        LIMITS,
        GUARD_MS,
        RecoveryClock {
            now_monotonic_ms: 1000,
            now_unix_ms: 1000,
        },
        std::slice::from_ref(&handle),
    )
    .unwrap();
    let transaction = &restored.transactions[0].transaction;
    assert_eq!(
        restored
            .bank
            .pending_message(transaction)
            .unwrap()
            .unwrap()
            .bytes(),
        pdu
    );
    let mut core = LocalMessaging::from_store(store).unwrap();
    let key = handle.submission_key();
    let result = tsar_submission::commit(
        &mut core,
        &mut restored.bank,
        transaction,
        SubmissionContext {
            destination: &destination(),
            submission_key: &key,
            created_at_unix_ms: 10,
            monotonic_ms: 999,
        },
    );
    assert!(matches!(
        result,
        Err(tsar_submission::Error::Ownership(
            IncomingError::ClockRegression
        ))
    ));
    let id = core.store().pending_for(MobileIdentity::Min34(2)).unwrap()[0].id;
    drop(core);
    drop(restored);
    let store = StateStore::open(&database.0).unwrap();
    let mut restored = tsar_recovery::restore(
        &store,
        PEER,
        SERVICE,
        LIMITS,
        GUARD_MS,
        RecoveryClock {
            now_monotonic_ms: 2000,
            now_unix_ms: 2000,
        },
        std::slice::from_ref(&handle),
    )
    .unwrap();
    let mut core = LocalMessaging::from_store(store).unwrap();
    let saved = tsar_submission::commit(
        &mut core,
        &mut restored.bank,
        &restored.transactions[0].transaction,
        SubmissionContext {
            destination: &destination(),
            submission_key: &key,
            created_at_unix_ms: 10,
            monotonic_ms: 2000,
        },
    )
    .unwrap();
    assert_eq!(saved.id, id);
    assert_eq!(
        core.store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
    let mut journal_store = StateStore::open(&database.0).unwrap();
    journal_store
        .append_incoming_event(
            &handle,
            revision,
            &JournalEvent::ApplicationCommitted(id),
            2000,
        )
        .unwrap();
    let restored = tsar_recovery::restore(
        &journal_store,
        PEER,
        SERVICE,
        LIMITS,
        GUARD_MS,
        RecoveryClock {
            now_monotonic_ms: 3000,
            now_unix_ms: 3000,
        },
        &[handle],
    )
    .unwrap();
    assert_eq!(
        restored
            .bank
            .application_outcome(&restored.transactions[0].transaction)
            .unwrap(),
        Some(ApplicationOutcome::Committed)
    );
    assert!(
        restored
            .bank
            .pending_message(&restored.transactions[0].transaction)
            .unwrap()
            .is_none()
    );
    assert!(!journal_store.relay_accepted(id).unwrap());
}
#[test]
fn recovery_rechecks_the_durable_application_identity() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    let (handle, pdu, revision) = complete_cmt(&mut store);
    let mut core = LocalMessaging::from_store(store).unwrap();
    let saved = core
        .submit_complete_sms(PEER, &destination(), &pdu, &handle.submission_key(), 10)
        .unwrap();
    let mut store = StateStore::open(&database.0).unwrap();
    store
        .append_incoming_event(
            &handle,
            revision,
            &JournalEvent::ApplicationCommitted(saved.id),
            1000,
        )
        .unwrap();
    let external = rusqlite::Connection::open(&database.0).unwrap();
    external
        .execute(
            "UPDATE sms_queue SET dedup_key='wrong-application' WHERE id=?1",
            [saved.id.value()],
        )
        .unwrap();
    assert!(matches!(
        store.incoming_snapshot(&handle),
        Err(StoreError::CorruptState)
    ));
    assert!(matches!(
        tsar_recovery::restore(
            &store,
            PEER,
            SERVICE,
            LIMITS,
            GUARD_MS,
            RecoveryClock {
                now_monotonic_ms: 2000,
                now_unix_ms: 2000
            },
            &[handle]
        ),
        Err(tsar_recovery::Error::Store(StoreError::CorruptState))
    ));
}
#[test]
fn semantic_replay_failure_does_not_skip_history_or_modify_the_journal() {
    let mut store = StateStore::open(":memory:").unwrap();
    let handle = create(&mut store, 0, 0);
    store
        .append_incoming_event(&handle, 0, &JournalEvent::ApplicationRejected, 1)
        .unwrap();
    assert!(matches!(
        tsar_recovery::restore(
            &store,
            PEER,
            SERVICE,
            LIMITS,
            GUARD_MS,
            RecoveryClock {
                now_monotonic_ms: 1000,
                now_unix_ms: 1000
            },
            std::slice::from_ref(&handle)
        ),
        Err(tsar_recovery::Error::Ownership(
            IncomingError::StaleApplication
        ))
    ));
    assert_eq!(store.incoming_journal(&handle).unwrap().revision, 1);
    assert_eq!(
        store.incoming_events(&handle).unwrap()[0].event,
        JournalEvent::ApplicationRejected
    );
}

fn transition<'a>(
    handle: &'a JournalHandle,
    revision: u32,
    unix_ms: i64,
    event: JournalEvent,
) -> Transition<'a> {
    Transition {
        peer: PEER,
        protocol: SERVICE,
        limits: LIMITS,
        reuse_guard_ms: GUARD_MS,
        clock: RecoveryClock {
            now_monotonic_ms: 1000 + unix_ms as u64,
            now_unix_ms: unix_ms,
        },
        target: handle,
        expected_revision: revision,
        event,
    }
}
#[test]
fn ingress_publishes_reassembly_and_response_only_with_the_persisted_segment() {
    let mut store = StateStore::open(":memory:").unwrap();
    let handle = create(&mut store, 0, 0);
    let first = tsar_ingress::apply(
        &mut store,
        transition(&handle, 0, 0, JournalEvent::Segment(begin(0))),
    )
    .unwrap();
    assert!(first.response.is_none());
    assert_eq!(first.incoming.transactions[0].journal.revision, 1);
    assert_eq!(store.incoming_events(&handle).unwrap().len(), 1);
    let last = tsar_ingress::apply(
        &mut store,
        transition(&handle, 1, 1, JournalEvent::Segment(end(0))),
    )
    .unwrap();
    assert!(last.response.is_some());
    assert_eq!(
        last.incoming
            .bank
            .pending_message(&last.incoming.transactions[0].transaction)
            .unwrap()
            .unwrap()
            .bytes(),
        b"abcd"
    );
    assert_eq!(store.incoming_events(&handle).unwrap().len(), 2);
    let settled = tsar_ingress::apply(
        &mut store,
        transition(&handle, 2, 2, JournalEvent::ApplicationRejected),
    )
    .unwrap();
    assert_eq!(
        settled
            .incoming
            .bank
            .application_outcome(&settled.incoming.transactions[0].transaction)
            .unwrap(),
        Some(ApplicationOutcome::Rejected)
    );
    assert_eq!(settled.incoming.transactions[0].journal.revision, 3);
}
#[test]
fn ingress_rejects_semantic_errors_before_writing_history() {
    let mut store = StateStore::open(":memory:").unwrap();
    let handle = create(&mut store, 0, 0);
    assert!(matches!(
        tsar_ingress::apply(
            &mut store,
            transition(&handle, 0, 0, JournalEvent::ApplicationRejected)
        ),
        Err(tsar_ingress::Error::NoPendingMessage)
    ));
    assert!(
        tsar_ingress::apply(
            &mut store,
            transition(&handle, 0, 0, JournalEvent::Segment(end(0)))
        )
        .is_err()
    );
    assert!(store.incoming_events(&handle).unwrap().is_empty());
    assert_eq!(store.incoming_journal(&handle).unwrap().revision, 0);
}
#[test]
fn ingress_failed_durable_write_cannot_publish_advanced_receiver_or_response() {
    let mut store = StateStore::open(":memory:").unwrap();
    let handle = store
        .begin_incoming_journal(
            IncomingBinding {
                peer: PEER,
                protocol: SERVICE,
                transaction_id: 0,
                version: ProtocolVersion::Pv1,
                context: Vec::new(),
            },
            JournalLimits {
                maximum_events: 1,
                maximum_context_octets: 1,
            },
            0,
        )
        .unwrap()
        .handle;
    tsar_ingress::apply(
        &mut store,
        transition(&handle, 0, 0, JournalEvent::Segment(begin(0))),
    )
    .unwrap();
    assert!(matches!(
        tsar_ingress::apply(
            &mut store,
            transition(&handle, 1, 1, JournalEvent::Segment(end(0)))
        ),
        Err(tsar_ingress::Error::Store(StoreError::IncomingJournal(
            bts136::core::store::incoming_journal::JournalError::Capacity
        )))
    ));
    let restored = tsar_recovery::restore(
        &store,
        PEER,
        SERVICE,
        LIMITS,
        GUARD_MS,
        RecoveryClock {
            now_monotonic_ms: 1001,
            now_unix_ms: 1,
        },
        std::slice::from_ref(&handle),
    )
    .unwrap();
    assert_eq!(
        restored
            .bank
            .receiver(&restored.transactions[0].transaction)
            .unwrap()
            .state(),
        ReceiverState::Receiving
    );
    assert!(
        restored
            .bank
            .pending_message(&restored.transactions[0].transaction)
            .unwrap()
            .is_none()
    );
    assert_eq!(store.incoming_events(&handle).unwrap().len(), 1);
}
#[test]
fn ingress_revision_and_target_failures_do_not_modify_other_streams() {
    let mut store = StateStore::open(":memory:").unwrap();
    let a = create(&mut store, 0, 0);
    let b = create(&mut store, 1, 0);
    tsar_ingress::apply(
        &mut store,
        transition(&a, 0, 0, JournalEvent::Segment(begin(0))),
    )
    .unwrap();
    assert!(matches!(
        tsar_ingress::apply(
            &mut store,
            transition(&a, 0, 1, JournalEvent::Segment(end(0)))
        ),
        Err(tsar_ingress::Error::Revision)
    ));
    let mut wrong_peer = transition(&a, 1, 1, JournalEvent::Segment(end(0)));
    wrong_peer.peer = MobileIdentity::Min34(3);
    assert!(matches!(
        tsar_ingress::apply(&mut store, wrong_peer),
        Err(tsar_ingress::Error::Target)
    ));
    assert_eq!(store.incoming_events(&a).unwrap().len(), 1);
    assert!(store.incoming_events(&b).unwrap().is_empty());
}

#[test]
fn ingress_counts_other_active_streams_against_the_shared_payload_budget() {
    let mut store = StateStore::open(":memory:").unwrap();
    let a = create(&mut store, 0, 0);
    let b = create(&mut store, 1, 0);
    let mut request = transition(&a, 0, 0, JournalEvent::Segment(begin(0)));
    request.limits.maximum_reserved_payload_octets = 2 * LIMITS.maximum_message_octets;
    assert!(matches!(
        tsar_ingress::apply(&mut store, request),
        Err(tsar_ingress::Error::Recovery(
            tsar_recovery::Error::Ownership(IncomingError::Capacity)
        ))
    ));
    assert!(store.incoming_events(&a).unwrap().is_empty());
    assert!(store.incoming_events(&b).unwrap().is_empty());
}

#[test]
fn ingress_timeout_response_and_cancellation_require_their_durable_observations() {
    let mut store = StateStore::open(":memory:").unwrap();
    let handle = create(&mut store, 0, 0);
    tsar_ingress::apply(
        &mut store,
        transition(&handle, 0, 0, JournalEvent::Segment(begin(0))),
    )
    .unwrap();
    let timeout = tsar_ingress::apply(
        &mut store,
        transition(&handle, 1, 300_001, JournalEvent::Advance),
    )
    .unwrap();
    assert_eq!(
        timeout.response.unwrap().status,
        Status::InterSegmentTimeout
    );
    assert_eq!(
        store.incoming_events(&handle).unwrap()[1].event,
        JournalEvent::Advance
    );
    let canceled = tsar_ingress::apply(
        &mut store,
        transition(&handle, 2, 300_002, JournalEvent::Cancel),
    )
    .unwrap();
    assert!(canceled.response.is_none());
    assert_eq!(
        store.incoming_events(&handle).unwrap()[2].event,
        JournalEvent::Cancel
    );
}

#[test]
fn durable_received_response_waits_for_application_settlement_and_blocks_retirement() {
    use bts136::core::store::incoming_journal::{JournalError, ResponseOutcome};
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    let handle = create(&mut store, 0, 0);
    tsar_ingress::apply(
        &mut store,
        transition(&handle, 0, 0, JournalEvent::Segment(begin(0))),
    )
    .unwrap();
    let last = tsar_ingress::apply(
        &mut store,
        transition(&handle, 1, 1, JournalEvent::Segment(end(0))),
    )
    .unwrap();
    assert!(last.response.is_some());
    assert!(store.pending_incoming_responses(1).unwrap().is_empty());
    tsar_ingress::apply(
        &mut store,
        transition(&handle, 2, 2, JournalEvent::ApplicationRejected),
    )
    .unwrap();
    let pending = store.pending_incoming_responses(1).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].response, last.response.unwrap());
    assert_eq!(pending[0].handle.ordinal(), 1);
    assert_eq!(pending[0].handle.journal(), &handle);
    assert!(matches!(
        store.retire_incoming_journal_fenced(&handle, 3, 3, GUARD_MS),
        Err(StoreError::IncomingJournal(JournalError::ResponsePending))
    ));
    store
        .settle_incoming_response(&pending[0].handle, ResponseOutcome::Accepted, 3)
        .unwrap();
    store
        .settle_incoming_response(&pending[0].handle, ResponseOutcome::Accepted, 3)
        .unwrap();
    store
        .settle_incoming_response(&pending[0].handle, ResponseOutcome::Accepted, 4)
        .unwrap();
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    let settled: i64 = connection.query_row("SELECT settled_at_unix_ms FROM tsar_incoming_responses WHERE stream_id=?1 AND event_ordinal=1", [handle.id()], |row| row.get(0)).unwrap();
    assert_eq!(settled, 3);
    assert!(matches!(
        store.settle_incoming_response(&pending[0].handle, ResponseOutcome::Rejected, 3),
        Err(StoreError::IncomingJournal(JournalError::RevisionConflict))
    ));
    assert!(store.pending_incoming_responses(1).unwrap().is_empty());
    store
        .retire_incoming_journal_fenced(&handle, 3, 3, GUARD_MS)
        .unwrap();
}

#[test]
fn durable_responses_remain_ordered_across_restart_and_uncertain_append_retries() {
    use bts136::core::store::incoming_journal::ResponseOutcome;
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    let handle = create(&mut store, 0, 0);
    tsar_ingress::apply(
        &mut store,
        transition(&handle, 0, 0, JournalEvent::Segment(begin(0))),
    )
    .unwrap();
    let timeout = tsar_ingress::apply(
        &mut store,
        transition(&handle, 1, 300_001, JournalEvent::Advance),
    )
    .unwrap();
    let response = timeout.response.unwrap();
    let retry = store
        .append_incoming_transition(&handle, 1, &JournalEvent::Advance, 300_001, Some(&response))
        .unwrap();
    assert!(retry.already_committed);
    assert!(
        store
            .append_incoming_event(&handle, 1, &JournalEvent::Advance, 300_001)
            .is_err()
    );
    tsar_ingress::apply(
        &mut store,
        transition(&handle, 2, 300_002, JournalEvent::Segment(end(0))),
    )
    .unwrap();
    tsar_ingress::apply(
        &mut store,
        transition(&handle, 3, 300_003, JournalEvent::ApplicationRejected),
    )
    .unwrap();
    drop(store);
    let mut store = StateStore::open(&database.0).unwrap();
    let pending = store.pending_incoming_responses(32).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].response.status, Status::InterSegmentTimeout);
    store
        .settle_incoming_response(&pending[0].handle, ResponseOutcome::Expired, 300_004)
        .unwrap();
    let next = store.pending_incoming_responses(32).unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].response.status, Status::Received);
    assert_eq!(next[0].handle.ordinal(), 2);
}

#[test]
fn response_write_failure_rolls_back_its_receiver_event_and_header() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    let handle = create(&mut store, 0, 0);
    tsar_ingress::apply(
        &mut store,
        transition(&handle, 0, 0, JournalEvent::Segment(begin(0))),
    )
    .unwrap();
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_response BEFORE INSERT ON tsar_incoming_responses BEGIN SELECT RAISE(ABORT, 'response write failure'); END").unwrap();
    assert!(matches!(
        tsar_ingress::apply(
            &mut store,
            transition(&handle, 1, 1, JournalEvent::Segment(end(0)))
        ),
        Err(tsar_ingress::Error::Store(StoreError::Sql(_)))
    ));
    assert_eq!(store.incoming_journal(&handle).unwrap().revision, 1);
    assert_eq!(store.incoming_events(&handle).unwrap().len(), 1);
    assert!(store.pending_incoming_responses(32).unwrap().is_empty());
    connection
        .execute_batch("DROP TRIGGER fail_response")
        .unwrap();
    tsar_ingress::apply(
        &mut store,
        transition(&handle, 1, 1, JournalEvent::Segment(end(0))),
    )
    .unwrap();
    assert_eq!(store.incoming_journal(&handle).unwrap().revision, 2);
}

#[test]
fn uncommitted_application_does_not_hide_other_streams_from_a_limited_response_scan() {
    let mut store = StateStore::open(":memory:").unwrap();
    let a = create(&mut store, 0, 0);
    let b = create(&mut store, 1, 0);
    tsar_ingress::apply(
        &mut store,
        transition(&a, 0, 0, JournalEvent::Segment(begin(0))),
    )
    .unwrap();
    tsar_ingress::apply(
        &mut store,
        transition(&a, 1, 1, JournalEvent::Segment(end(0))),
    )
    .unwrap();
    tsar_ingress::apply(
        &mut store,
        transition(&b, 0, 1, JournalEvent::Segment(begin(1))),
    )
    .unwrap();
    tsar_ingress::apply(
        &mut store,
        transition(&b, 1, 300_002, JournalEvent::Advance),
    )
    .unwrap();
    let pending = store.pending_incoming_responses(1).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].handle.journal(), &b);
}

#[test]
fn schema_six_upgrade_preserves_journals_and_application_records() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    let (handle, payload, _) = complete_cmt(&mut store);
    let snapshot = store.incoming_snapshot(&handle).unwrap();
    let mut core = LocalMessaging::from_store(store).unwrap();
    let sms = core
        .submit_complete_sms(PEER, &destination(), &payload, &handle.submission_key(), 1)
        .unwrap();
    drop(core);
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection
        .execute_batch("DROP TABLE sms_hub_parts; DROP TABLE sms_hub_outbox; DROP TABLE sms_hub_inbox; DROP TABLE esn_subscribers; DROP TABLE broadcast_revisions; DROP TABLE ordinary_sms_requests; DROP TABLE tsar_outgoing_events; DROP TABLE tsar_outgoing_segments; DROP TABLE tsar_outgoing; DROP TABLE tsar_incoming_responses; ALTER TABLE tsar_incoming DROP COLUMN retired_at_unix_ms; PRAGMA user_version=6")
        .unwrap();
    drop(connection);
    let store = StateStore::open(&database.0).unwrap();
    assert_eq!(store.incoming_events(&handle).unwrap(), snapshot.events);
    assert_eq!(
        store.pending_for(MobileIdentity::Min34(2)).unwrap()[0].id,
        sms.id
    );
    assert!(store.pending_incoming_responses(32).unwrap().is_empty());
}
