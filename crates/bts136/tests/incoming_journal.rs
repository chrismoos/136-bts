use bts136::{
    core::{
        messaging::LocalMessaging,
        registry::ProvisionedSubscriber,
        store::{
            StateStore, StoreError,
            incoming_journal::{IncomingBinding, JournalError, JournalEvent, JournalLimits},
        },
    },
    l3::{
        address::{AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        relay::{HigherLayerProtocol, Teleservice},
        sms::{Metadata, SmsMessage, UserData},
        tsar::{self, AcknowledgmentWindow, OptionalServices, Segment, SegmentKind},
    },
    link::{
        rach::MobileIdentity,
        tsar::{ApplicationOutcome, IncomingLimits, IncomingTransactions},
    },
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
const PEER: MobileIdentity = MobileIdentity::Min34(1);
const PROTOCOL: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
const LIMITS: JournalLimits = JournalLimits {
    maximum_events: 16,
    maximum_context_octets: 128,
};
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(PathBuf::from(format!(
            "/var/tmp/136bts-incoming-journal-{}-{}.sqlite",
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
fn binding() -> IncomingBinding {
    IncomingBinding {
        peer: PEER,
        protocol: PROTOCOL,
        transaction_id: 3,
        version: ProtocolVersion::Pv1,
        context: b"frozen address and transport context".to_vec(),
    }
}
fn begin() -> Segment {
    Segment {
        transaction_id: 3,
        sequence_number: 0,
        segment_data: b"a".to_vec(),
        kind: SegmentKind::Begin {
            message_length: Some(3),
            services: Some(OptionalServices {
                acknowledgment_window: AcknowledgmentWindow::Every8,
                ..OptionalServices::default()
            }),
        },
    }
}
fn error(result: Result<impl Sized, StoreError>, expected: JournalError) {
    assert!(matches!(result, Err(StoreError::IncomingJournal(actual)) if actual==expected));
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
#[test]
fn journal_survives_restart_with_frozen_binding_and_stable_application_key() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    let original = store.begin_incoming_journal(binding(), LIMITS, 10).unwrap();
    let key = original.handle.submission_key();
    store
        .append_incoming_event(&original.handle, 0, &JournalEvent::Segment(begin()), 11)
        .unwrap();
    drop(store);
    let mut store = StateStore::open(&database.0).unwrap();
    let loaded = store.active_incoming_journals(0, 8).unwrap().remove(0);
    assert_eq!(loaded.handle, original.handle);
    assert_eq!(loaded.handle.submission_key(), key);
    assert_eq!(loaded.binding, binding());
    assert_eq!(loaded.revision, 1);
    let duplicate = store
        .begin_incoming_journal(binding(), LIMITS, 1000)
        .unwrap();
    assert_eq!(duplicate.created_at_unix_ms, 10);
    assert_eq!(duplicate.observed_at_unix_ms, 11);
    assert_eq!(duplicate.revision, 1);
    let mut changed = binding();
    changed.context.push(0);
    error(
        store.begin_incoming_journal(changed, LIMITS, 1000),
        JournalError::ContextConflict,
    );
    assert_eq!(
        store.incoming_events(&original.handle).unwrap()[0].event,
        JournalEvent::Segment(begin())
    );
}
#[test]
fn lost_append_result_is_idempotent_without_hiding_revision_conflicts() {
    let mut store = StateStore::open(":memory:").unwrap();
    let journal = store.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    let first = store
        .append_incoming_event(&journal.handle, 0, &JournalEvent::Segment(begin()), 1)
        .unwrap();
    assert!(!first.already_committed);
    store
        .append_incoming_event(&journal.handle, 1, &JournalEvent::Advance, 2)
        .unwrap();
    let retry = store
        .append_incoming_event(&journal.handle, 0, &JournalEvent::Segment(begin()), 1)
        .unwrap();
    assert!(retry.already_committed);
    assert_eq!(retry.revision, 2);
    error(
        store.append_incoming_event(&journal.handle, 0, &JournalEvent::Cancel, 1),
        JournalError::RevisionConflict,
    );
    error(
        store.append_incoming_event(&journal.handle, 3, &JournalEvent::Cancel, 3),
        JournalError::RevisionConflict,
    );
    assert_eq!(store.incoming_events(&journal.handle).unwrap().len(), 2);
}
#[test]
fn retirement_is_explicit_guarded_and_does_not_alias_a_reused_wire_id() {
    let mut store = StateStore::open(":memory:").unwrap();
    let original = store.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    store
        .append_incoming_event(&original.handle, 0, &JournalEvent::Cancel, 1)
        .unwrap();
    error(
        store.retire_incoming_journal_fenced(&original.handle, 0, 2, 100),
        JournalError::RevisionConflict,
    );
    error(
        store.retire_incoming_journal_fenced(&original.handle, 1, 2, 0),
        JournalError::GuardDuration,
    );
    assert!(
        store
            .retire_incoming_journal_fenced(&original.handle, 1, i64::MAX, 1)
            .is_err()
    );
    assert!(
        store
            .incoming_journal(&original.handle)
            .unwrap()
            .reuse_after_unix_ms
            .is_none()
    );
    store
        .retire_incoming_journal_fenced(&original.handle, 1, 2, 100)
        .unwrap();
    store
        .retire_incoming_journal_fenced(&original.handle, 1, 2, 100)
        .unwrap();
    error(
        store.retire_incoming_journal_fenced(&original.handle, 1, 3, 99),
        JournalError::Closed,
    );
    error(
        store.retire_incoming_journal_fenced(&original.handle, 1, 3, 100),
        JournalError::Closed,
    );
    error(
        store.append_incoming_event(&original.handle, 1, &JournalEvent::Advance, 3),
        JournalError::Closed,
    );
    error(
        store.begin_incoming_journal(binding(), LIMITS, 101),
        JournalError::Guarded,
    );
    let replacement = store
        .begin_incoming_journal(binding(), LIMITS, 102)
        .unwrap();
    assert_ne!(original.handle, replacement.handle);
    assert_ne!(
        original.handle.submission_key(),
        replacement.handle.submission_key()
    );
    assert_eq!(
        store.active_incoming_journals(0, 1).unwrap()[0].handle,
        replacement.handle
    );
}
#[test]
fn capacity_clock_and_packet_validation_fail_without_appending_partial_events() {
    let mut store = StateStore::open(":memory:").unwrap();
    let journal = store
        .begin_incoming_journal(
            binding(),
            JournalLimits {
                maximum_events: 2,
                ..LIMITS
            },
            10,
        )
        .unwrap();
    error(
        store.append_incoming_event(&journal.handle, 0, &JournalEvent::Advance, 9),
        JournalError::ClockRegression,
    );
    let mut wrong = begin();
    wrong.transaction_id = 2;
    error(
        store.append_incoming_event(&journal.handle, 0, &JournalEvent::Segment(wrong), 11),
        JournalError::Segment,
    );
    let mut oversized = begin();
    oversized.segment_data.resize(254, 0);
    error(
        store.append_incoming_event(&journal.handle, 0, &JournalEvent::Segment(oversized), 11),
        JournalError::Segment,
    );
    store
        .append_incoming_event(&journal.handle, 0, &JournalEvent::Segment(begin()), 11)
        .unwrap();
    store
        .append_incoming_event(&journal.handle, 1, &JournalEvent::Advance, 12)
        .unwrap();
    error(
        store.append_incoming_event(&journal.handle, 2, &JournalEvent::Cancel, 13),
        JournalError::Capacity,
    );
    assert_eq!(store.incoming_events(&journal.handle).unwrap().len(), 2);
    assert_eq!(
        store
            .incoming_journal(&journal.handle)
            .unwrap()
            .observed_at_unix_ms,
        12
    );
}
#[test]
fn handles_cannot_cross_databases_even_when_row_ids_match() {
    let mut first = StateStore::open(":memory:").unwrap();
    let mut second = StateStore::open(":memory:").unwrap();
    let a = first.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    let b = second.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    assert_eq!(a.handle.id(), b.handle.id());
    assert_ne!(a.handle, b.handle);
    error(
        second.append_incoming_event(&a.handle, 0, &JournalEvent::Cancel, 1),
        JournalError::StaleHandle,
    );
    assert_eq!(second.incoming_journal(&b.handle).unwrap().revision, 0);
}
#[test]
fn write_failure_rolls_back_both_event_and_revision() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    let journal = store.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    let external = rusqlite::Connection::open(&database.0).unwrap();
    external.execute_batch("CREATE TRIGGER fail_journal_update BEFORE UPDATE ON tsar_incoming BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").unwrap();
    assert!(
        store
            .append_incoming_event(&journal.handle, 0, &JournalEvent::Segment(begin()), 1)
            .is_err()
    );
    assert!(store.incoming_events(&journal.handle).unwrap().is_empty());
    assert_eq!(store.incoming_journal(&journal.handle).unwrap().revision, 0);
    external
        .execute_batch("DROP TRIGGER fail_journal_update;")
        .unwrap();
    store
        .append_incoming_event(&journal.handle, 0, &JournalEvent::Segment(begin()), 1)
        .unwrap();
    assert_eq!(store.incoming_events(&journal.handle).unwrap().len(), 1);
}
#[test]
fn replay_preserves_timeout_advancement_and_exact_pending_application_settlement() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    let journal = store.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    let end = Segment {
        transaction_id: 3,
        sequence_number: 1,
        segment_data: b"bc".to_vec(),
        kind: SegmentKind::End {
            message_length: None,
            crc: None,
        },
    };
    for (ordinal, (time, event)) in [
        (0, JournalEvent::Segment(begin())),
        (300_001, JournalEvent::Advance),
        (300_002, JournalEvent::Segment(end)),
        (300_003, JournalEvent::ApplicationRejected),
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_incoming_event(&journal.handle, ordinal as u32, &event, time)
            .unwrap();
    }
    drop(store);
    let store = StateStore::open(&database.0).unwrap();
    let mut bank = IncomingTransactions::try_new(
        PEER,
        PROTOCOL,
        IncomingLimits {
            maximum_message_octets: 259,
            maximum_reserved_payload_octets: 518,
        },
        100,
        0,
    )
    .unwrap();
    let mut transaction = None;
    for event in store.incoming_events(&journal.handle).unwrap() {
        let now = event.observed_at_unix_ms as u64;
        match event.event {
            JournalEvent::Segment(segment) => {
                let decoded = tsar::decode_segment(
                    &segment.encode(ProtocolVersion::Pv1).unwrap(),
                    ProtocolVersion::Pv1,
                )
                .unwrap();
                if let Some(owner) = &transaction {
                    bank.receive(owner, PEER, PROTOCOL, &decoded, now).unwrap();
                } else {
                    transaction = Some(
                        bank.begin(PEER, PROTOCOL, &decoded, ProtocolVersion::Pv1, now)
                            .unwrap()
                            .transaction,
                    );
                }
            }
            JournalEvent::Advance => {
                assert!(
                    bank.advance(transaction.as_ref().unwrap(), now)
                        .unwrap()
                        .is_some()
                );
            }
            JournalEvent::ApplicationRejected => {
                let owner = transaction.as_ref().unwrap();
                let pending = bank.pending_message(owner).unwrap().unwrap().clone();
                assert_eq!(pending.bytes(), b"abc");
                bank.settle_application(owner, &pending, ApplicationOutcome::Rejected, now)
                    .unwrap();
            }
            _ => panic!(),
        }
    }
    assert_eq!(
        bank.application_outcome(transaction.as_ref().unwrap())
            .unwrap(),
        Some(ApplicationOutcome::Rejected)
    );
}
#[test]
fn committed_application_requires_the_journal_key_and_cannot_change_outcome() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    provision(&mut store);
    let journal = store.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    let mut core = LocalMessaging::from_store(store).unwrap();
    let mut journal_store = StateStore::open(&database.0).unwrap();
    let destination = AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd("1002".into()),
    };
    let pdu = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 1,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(b"journal application".to_vec()),
    }
    .encode()
    .unwrap();
    let wrong = core
        .submit_complete_sms(PEER, &destination, &pdu, "unrelated", 0)
        .unwrap();
    error(
        journal_store.append_incoming_event(
            &journal.handle,
            0,
            &JournalEvent::ApplicationCommitted(wrong.id),
            1,
        ),
        JournalError::ApplicationConflict,
    );
    let stored = core
        .submit_complete_sms(
            PEER,
            &destination,
            &pdu,
            &journal.handle.submission_key(),
            0,
        )
        .unwrap();
    journal_store
        .append_incoming_event(
            &journal.handle,
            0,
            &JournalEvent::ApplicationCommitted(stored.id),
            1,
        )
        .unwrap();
    error(
        journal_store.append_incoming_event(
            &journal.handle,
            1,
            &JournalEvent::ApplicationRejected,
            2,
        ),
        JournalError::ApplicationConflict,
    );
    assert_eq!(
        core.store().incoming_events(&journal.handle).unwrap()[0].event,
        JournalEvent::ApplicationCommitted(stored.id)
    );
}
#[test]
fn version_five_upgrade_preserves_existing_sms_and_is_idempotent() {
    let database = Database::new();
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    for schema in [
        include_str!("../migrations/0001_initial.sql"),
        include_str!("../migrations/0002_delivery_receipts.sql"),
        include_str!("../migrations/0003_manual_receipts.sql"),
        include_str!("../migrations/0004_relay_acceptance.sql"),
        include_str!("../migrations/0005_subscriber_management.sql"),
    ] {
        connection.execute_batch(schema).unwrap();
    }
    connection.execute_batch("PRAGMA user_version=5; INSERT INTO subscribers(identity_type,identity_value,number,enabled) VALUES(2,1,'1001',1),(2,2,'1002',1); INSERT INTO sms_queue(dedup_key,sender_type,sender_value,recipient_type,recipient_value,sender_number,sms_pdu,created_at_unix_ms,state,recipient_number) VALUES('submit:existing',2,1,2,2,'1001',X'00010108020841',0,0,'1002');").unwrap();
    drop(connection);
    let mut store = StateStore::open(&database.0).unwrap();
    assert_eq!(store.enabled_subscribers().unwrap().len(), 2);
    assert_eq!(
        store.pending_for(MobileIdentity::Min34(2)).unwrap().len(),
        1
    );
    let journal = store.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    drop(store);
    let store = StateStore::open(&database.0).unwrap();
    assert_eq!(
        store.incoming_journal(&journal.handle).unwrap().binding,
        binding()
    );
    assert_eq!(
        store.pending_for(MobileIdentity::Min34(2)).unwrap().len(),
        1
    );
}

#[test]
fn independent_writers_cannot_fork_the_same_journal_revision() {
    let database = Database::new();
    let mut first = StateStore::open(&database.0).unwrap();
    let journal = first.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    let mut second = StateStore::open(&database.0).unwrap();
    first
        .append_incoming_event(&journal.handle, 0, &JournalEvent::Segment(begin()), 1)
        .unwrap();
    error(
        second.append_incoming_event(&journal.handle, 0, &JournalEvent::Advance, 2),
        JournalError::RevisionConflict,
    );
    second
        .append_incoming_event(&journal.handle, 1, &JournalEvent::Advance, 2)
        .unwrap();
    let events = first.incoming_events(&journal.handle).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, JournalEvent::Segment(begin()));
    assert_eq!(events[1].event, JournalEvent::Advance);
}
#[test]
fn invalid_bindings_and_limits_do_not_publish_a_stream() {
    let mut store = StateStore::open(":memory:").unwrap();
    let mut invalid_id = binding();
    invalid_id.transaction_id = 8;
    let mut invalid_service = binding();
    invalid_service.protocol = HigherLayerProtocol::Sms;
    let mut invalid_version = binding();
    invalid_version.version = ProtocolVersion::Is136RevA;
    let mut invalid_peer = binding();
    invalid_peer.peer = MobileIdentity::Min34(1 << 34);
    for binding in [invalid_id, invalid_service, invalid_version, invalid_peer] {
        error(
            store.begin_incoming_journal(binding, LIMITS, 0),
            JournalError::Binding,
        );
    }
    for limits in [
        JournalLimits {
            maximum_events: 0,
            ..LIMITS
        },
        JournalLimits {
            maximum_context_octets: 1,
            ..LIMITS
        },
    ] {
        error(
            store.begin_incoming_journal(binding(), limits, 0),
            JournalError::Limits,
        );
    }
    assert!(store.active_incoming_journals(0, 8).unwrap().is_empty());
}
#[test]
fn recovery_pagination_preserves_full_service_and_transaction_namespaces() {
    let mut store = StateStore::open(":memory:").unwrap();
    let first = store.begin_incoming_journal(binding(), LIMITS, 0).unwrap();
    let mut next_id = binding();
    next_id.transaction_id = 4;
    let second = store.begin_incoming_journal(next_id, LIMITS, 0).unwrap();
    let mut carrier = binding();
    carrier.protocol = HigherLayerProtocol::CarrierSpecific(0xc1);
    let third = store
        .begin_incoming_journal(carrier.clone(), LIMITS, 0)
        .unwrap();
    assert_eq!(
        store.active_incoming_journals(0, 1).unwrap()[0].handle,
        first.handle
    );
    assert_eq!(
        store
            .active_incoming_journals(first.handle.id(), 1)
            .unwrap()[0]
            .handle,
        second.handle
    );
    let page = store
        .active_incoming_journals(second.handle.id(), 1)
        .unwrap();
    assert_eq!(page[0].handle, third.handle);
    assert_eq!(page[0].binding, carrier);
    store
        .retire_incoming_journal_fenced(&first.handle, 0, 0, 100)
        .unwrap();
    assert_eq!(
        store.active_incoming_journals(0, 1).unwrap()[0].handle,
        second.handle
    );
}
