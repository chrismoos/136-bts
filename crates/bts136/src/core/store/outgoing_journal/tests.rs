use super::*;
use crate::{
    core::{
        registry::ProvisionedSubscriber,
        store::{NewSms, QueueState},
    },
    l3::sms::{DisplayTime, Metadata, SmsMessage, UserData, Validity},
    link::tsar::Completion,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
const PEER: MobileIdentity = MobileIdentity::Min34(2);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(PathBuf::from(format!(
            "/var/tmp/136bts-outgoing-{}-{}.sqlite3",
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
fn seed(store: &mut StateStore) {
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
}
fn queue(store: &mut StateStore, key: &str, receipt: bool) -> SmsId {
    let message = SmsMessage::Deliver {
        metadata: Metadata {
            message_reference: 1,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: receipt,
            manual_acknowledgment: false,
            new_message: true,
        },
        validity: Validity::Indefinite,
        display_time: DisplayTime::Default,
        user_data: UserData::Ira(vec![b'x'; 254]),
    };
    store
        .enqueue_teleservice(NewSms {
            dedup_key: key,
            sender: MobileIdentity::Min34(1),
            recipient: PEER,
            message: &message,
            created_at_unix_ms: 0,
        })
        .unwrap()
}
fn request(id: SmsId) -> OutgoingRequest {
    OutgoingRequest {
        sms_id: id,
        peer: PEER,
        version: ProtocolVersion::Pv1,
        payload_budget_octets: 100,
        acknowledgment_window: AcknowledgmentWindow::Last,
        context: b"originating 1001".to_vec(),
        maximum_context_octets: 128,
        maximum_events: 64,
    }
}
fn begin(store: &mut StateStore, key: &str, receipt: bool) -> JournalSnapshot {
    let id = queue(store, key, receipt);
    store.begin_outgoing_journal(request(id), 1000).unwrap()
}
fn append(
    store: &mut StateStore,
    snapshot: &JournalSnapshot,
    event: JournalEvent,
    time: i64,
) -> JournalSnapshot {
    store
        .append_outgoing_event(&snapshot.handle, snapshot.revision(), event, time)
        .unwrap()
}
fn submit_all(
    store: &mut StateStore,
    mut snapshot: JournalSnapshot,
    accept_last: bool,
) -> JournalSnapshot {
    for sequence in 0..snapshot.plan.segments().len() {
        let ordinal = snapshot.revision();
        let now = snapshot.observed_at_unix_ms + 1;
        snapshot = append(
            store,
            &snapshot,
            JournalEvent::Submitted {
                sequence: sequence as u8,
                queued_at_unix_ms: now,
            },
            now,
        );
        if accept_last || sequence + 1 < snapshot.plan.segments().len() {
            snapshot = append(
                store,
                &snapshot,
                JournalEvent::Accepted {
                    submission_ordinal: ordinal,
                },
                now + 1,
            );
        }
    }
    snapshot
}
fn received(snapshot: &JournalSnapshot) -> JournalEvent {
    JournalEvent::Response {
        payload: tsar::Response {
            transaction_id: snapshot.plan.segments()[0].transaction_id,
            status: tsar::Status::Received,
            acknowledgments: tsar::AcknowledgmentMap::try_new(vec![
                true;
                snapshot.plan.segments().len()
            ])
            .unwrap(),
            optional_services_support: None,
        }
        .encode(snapshot.version)
        .unwrap(),
    }
}
#[test]
fn preparation_and_lower_accepts_do_not_report_application_delivery() {
    for receipt in [false, true] {
        let mut store = StateStore::open(":memory:").unwrap();
        seed(&mut store);
        let initial = begin(&mut store, "long", receipt);
        assert_eq!(initial.replay().unwrap().state(), SenderState::Prepared);
        assert!(!store.relay_accepted(initial.sms_id).unwrap());
        let waiting = submit_all(&mut store, initial, true);
        assert_eq!(
            waiting.replay().unwrap().state(),
            SenderState::AwaitingResponse
        );
        assert!(!store.relay_accepted(waiting.sms_id).unwrap());
        let done = append(&mut store, &waiting, received(&waiting), 1020);
        assert_eq!(
            done.replay().unwrap().state(),
            SenderState::Complete(Completion::PeerReceived)
        );
        assert!(store.relay_accepted(done.sms_id).unwrap());
        assert_eq!(
            store.state(done.sms_id).unwrap(),
            if receipt {
                QueueState::Queued
            } else {
                QueueState::Delivered
            }
        );
    }
}
#[test]
fn restart_preserves_frozen_fragments_original_deadline_and_receipt_before_last_accept() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    seed(&mut store);
    let initial = begin(&mut store, "restart", false);
    let submitted = append(
        &mut store,
        &initial,
        JournalEvent::Submitted {
            sequence: 0,
            queued_at_unix_ms: 1001,
        },
        1002,
    );
    assert_eq!(
        submitted.replay().unwrap().deadline_monotonic_ms(),
        Some(331001)
    );
    drop(store);
    let mut store = StateStore::open(&database.0).unwrap();
    let recovered = store.outgoing_journals(0, 1).unwrap().remove(0);
    assert_eq!(recovered.handle, initial.handle);
    assert_eq!(recovered.plan, initial.plan);
    let pending = recovered.submitted_fragment().unwrap().unwrap();
    assert_eq!(pending.submission_ordinal, 0);
    assert_eq!(pending.queued_at_unix_ms, 1001);
    assert_eq!(pending.segment, initial.plan.segments()[0]);
    assert_eq!(
        recovered.replay().unwrap().deadline_monotonic_ms(),
        Some(331001)
    );
    let mut next = append(
        &mut store,
        &recovered,
        JournalEvent::Accepted {
            submission_ordinal: 0,
        },
        1003,
    );
    for sequence in 1..next.plan.segments().len() {
        let ordinal = next.revision();
        let now = next.observed_at_unix_ms + 1;
        next = append(
            &mut store,
            &next,
            JournalEvent::Submitted {
                sequence: sequence as u8,
                queued_at_unix_ms: now,
            },
            now,
        );
        if sequence + 1 < next.plan.segments().len() {
            next = append(
                &mut store,
                &next,
                JournalEvent::Accepted {
                    submission_ordinal: ordinal,
                },
                now + 1,
            );
        }
    }
    assert_eq!(next.replay().unwrap().state(), SenderState::AwaitingAccept);
    let done = append(&mut store, &next, received(&next), 1020);
    assert!(store.relay_accepted(done.sms_id).unwrap());
    drop(store);
    let mut store = StateStore::open(&database.0).unwrap();
    assert_eq!(
        store
            .outgoing_journal(&done.handle)
            .unwrap()
            .replay()
            .unwrap()
            .state(),
        SenderState::Complete(Completion::PeerReceived)
    );
}
#[test]
fn uncertain_retry_is_exact_and_stale_submission_generation_cannot_accept_retry() {
    let mut store = StateStore::open(":memory:").unwrap();
    seed(&mut store);
    let initial = begin(&mut store, "retry", false);
    let event = JournalEvent::Submitted {
        sequence: 0,
        queued_at_unix_ms: 1001,
    };
    let submitted = append(&mut store, &initial, event.clone(), 1001);
    assert_eq!(
        store
            .append_outgoing_event(&initial.handle, 0, event, 1001)
            .unwrap()
            .revision(),
        1
    );
    assert!(matches!(
        store.append_outgoing_event(&initial.handle, 0, JournalEvent::Advance, 1001),
        Err(StoreError::OutgoingJournal(JournalError::Conflict))
    ));
    let expired = append(&mut store, &submitted, JournalEvent::Advance, 331001);
    let retry = append(
        &mut store,
        &expired,
        JournalEvent::Submitted {
            sequence: 0,
            queued_at_unix_ms: 331002,
        },
        331002,
    );
    assert!(matches!(
        store.append_outgoing_event(
            &retry.handle,
            retry.revision(),
            JournalEvent::Accepted {
                submission_ordinal: 0
            },
            331003
        ),
        Err(StoreError::OutgoingJournal(JournalError::Event))
    ));
    assert_eq!(
        store.outgoing_journal(&retry.handle).unwrap().revision(),
        retry.revision()
    );
    let accepted = append(
        &mut store,
        &retry,
        JournalEvent::Accepted {
            submission_ordinal: 2,
        },
        331003,
    );
    assert_eq!(
        accepted
            .replay()
            .unwrap()
            .submission()
            .unwrap()
            .segment()
            .sequence_number,
        1
    );
}
#[test]
fn final_receipt_and_sms_acceptance_rollback_together() {
    let mut store = StateStore::open(":memory:").unwrap();
    seed(&mut store);
    let initial = begin(&mut store, "rollback", false);
    let waiting = submit_all(&mut store, initial, true);
    store.connection.execute_batch("CREATE TRIGGER reject_accept BEFORE UPDATE OF relay_accepted ON sms_queue BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
    assert!(
        store
            .append_outgoing_event(
                &waiting.handle,
                waiting.revision(),
                received(&waiting),
                1020
            )
            .is_err()
    );
    assert_eq!(
        store.outgoing_journal(&waiting.handle).unwrap().revision(),
        waiting.revision()
    );
    assert!(!store.relay_accepted(waiting.sms_id).unwrap());
    store
        .connection
        .execute_batch("DROP TRIGGER reject_accept")
        .unwrap();
    let done = append(&mut store, &waiting, received(&waiting), 1020);
    assert!(store.relay_accepted(done.sms_id).unwrap());
}
#[test]
fn occupied_wire_ids_stay_held_after_cancellation_and_restart() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    seed(&mut store);
    for index in 0..8 {
        let initial = begin(&mut store, &format!("sms-{index}"), false);
        assert_eq!(initial.plan.segments()[0].transaction_id, index);
        append(&mut store, &initial, JournalEvent::Cancel, 1001);
    }
    drop(store);
    let mut store = StateStore::open(&database.0).unwrap();
    assert_eq!(store.outgoing_journals(0, 3).unwrap().len(), 3);
    let id = queue(&mut store, "ninth", false);
    assert!(matches!(
        store.begin_outgoing_journal(request(id), 1002),
        Err(StoreError::OutgoingJournal(JournalError::Capacity))
    ));
    assert!(!store.relay_accepted(id).unwrap());
}
#[test]
fn invalid_events_limits_bindings_and_foreign_database_handles_are_rejected() {
    let mut store = StateStore::open(":memory:").unwrap();
    seed(&mut store);
    let id = queue(&mut store, "validation", false);
    let mut invalid = request(id);
    invalid.maximum_events = 0;
    assert!(store.begin_outgoing_journal(invalid, 1000).is_err());
    let mut invalid = request(id);
    invalid.peer = MobileIdentity::Min34(1);
    assert!(store.begin_outgoing_journal(invalid, 1000).is_err());
    let initial = store.begin_outgoing_journal(request(id), 1000).unwrap();
    assert!(
        store
            .append_outgoing_event(
                &initial.handle,
                0,
                JournalEvent::Accepted {
                    submission_ordinal: 0
                },
                1001
            )
            .is_err()
    );
    assert!(
        store
            .append_outgoing_event(&initial.handle, 0, received(&initial), 1001)
            .is_err()
    );
    assert!(
        store
            .append_outgoing_event(&initial.handle, 0, JournalEvent::Advance, 999)
            .is_err()
    );
    assert_eq!(
        store.outgoing_journal(&initial.handle).unwrap().revision(),
        0
    );
    let mut other = StateStore::open(":memory:").unwrap();
    seed(&mut other);
    begin(&mut other, "other", false);
    assert!(matches!(
        other.outgoing_journal(&initial.handle),
        Err(StoreError::OutgoingJournal(JournalError::StaleHandle))
    ));
    assert_eq!(
        store
            .begin_outgoing_journal(request(id), 1010)
            .unwrap()
            .handle,
        initial.handle
    );
    let mut changed = request(id);
    changed.context.push(1);
    assert!(store.begin_outgoing_journal(changed, 1010).is_err());
}
#[test]
fn corrupted_plan_or_queue_binding_never_restores_a_sender() {
    let mut store = StateStore::open(":memory:").unwrap();
    seed(&mut store);
    let initial = begin(&mut store, "corrupt", false);
    store.connection.execute("UPDATE tsar_outgoing_segments SET payload=x'000001ff' WHERE stream_id=?1 AND sequence=0",[initial.handle.id]).unwrap();
    assert!(store.outgoing_journal(&initial.handle).is_err());
    let second = begin(&mut store, "queue-corrupt", false);
    store
        .connection
        .execute(
            "UPDATE sms_queue SET sms_pdu=x'ff' WHERE id=?1",
            [second.sms_id.0],
        )
        .unwrap();
    assert!(matches!(
        store.outgoing_journal(&second.handle),
        Err(StoreError::OutgoingJournal(JournalError::Binding))
    ));
}
#[test]
fn schema_seven_upgrade_preserves_queue_and_incoming_tables() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    seed(&mut store);
    let id = queue(&mut store, "upgrade", false);
    store.connection.execute_batch("DROP TABLE sms_hub_parts; DROP TABLE sms_hub_outbox; DROP TABLE sms_hub_inbox; DROP TABLE esn_subscribers; DROP TABLE broadcast_revisions; DROP TABLE ordinary_sms_requests; DROP TABLE tsar_outgoing_events; DROP TABLE tsar_outgoing_segments; DROP TABLE tsar_outgoing; ALTER TABLE tsar_incoming DROP COLUMN retired_at_unix_ms; PRAGMA user_version=7").unwrap();
    drop(store);
    let mut store = StateStore::open(&database.0).unwrap();
    assert_eq!(store.state(id).unwrap(), QueueState::Queued);
    assert!(!store.relay_accepted(id).unwrap());
    assert!(store.pending_incoming_responses(1).unwrap().is_empty());
    assert_eq!(
        store
            .begin_outgoing_journal(request(id), 1000)
            .unwrap()
            .sms_id,
        id
    );
}

#[test]
fn explicit_rejection_and_event_capacity_keep_the_owned_transaction() {
    let mut store = StateStore::open(":memory:").unwrap();
    seed(&mut store);
    let initial = begin(&mut store, "reject", false);
    let submitted = append(
        &mut store,
        &initial,
        JournalEvent::Submitted {
            sequence: 0,
            queued_at_unix_ms: 1001,
        },
        1001,
    );
    let rejected = append(
        &mut store,
        &submitted,
        JournalEvent::Rejected {
            submission_ordinal: 0,
            cause: 3,
        },
        1002,
    );
    assert_eq!(
        rejected.replay().unwrap().state(),
        SenderState::Failed(crate::link::tsar::SenderFailure::LowerLayer(
            LowerLayerFailure::Rejected { cause: 3 }
        ))
    );
    assert!(rejected.submitted_fragment().unwrap().is_none());
    assert!(!store.relay_accepted(rejected.sms_id).unwrap());
    assert_eq!(store.state(rejected.sms_id).unwrap(), QueueState::Failed);
    assert!(
        store
            .append_outgoing_event(
                &rejected.handle,
                rejected.revision(),
                JournalEvent::Submitted {
                    sequence: 0,
                    queued_at_unix_ms: 1003
                },
                1003
            )
            .is_err()
    );
    let id = queue(&mut store, "capacity", false);
    let mut limited = request(id);
    limited.maximum_events = 1;
    let limited = store.begin_outgoing_journal(limited, 1000).unwrap();
    let submitted = append(
        &mut store,
        &limited,
        JournalEvent::Submitted {
            sequence: 0,
            queued_at_unix_ms: 1001,
        },
        1001,
    );
    assert!(matches!(
        store.append_outgoing_event(
            &submitted.handle,
            1,
            JournalEvent::Accepted {
                submission_ordinal: 0
            },
            1002
        ),
        Err(StoreError::OutgoingJournal(JournalError::Capacity))
    ));
    let retained = store.outgoing_journal(&submitted.handle).unwrap();
    assert_eq!(retained.revision(), 1);
    assert_eq!(retained.observed_at_unix_ms, 1001);
    assert_eq!(
        retained.replay().unwrap().state(),
        SenderState::AwaitingAccept
    );
}

const STORAGE_GUARD_MS: u64 = 100;
#[test]
fn retirement_requires_closed_state_exact_revision_and_a_nonzero_guard() {
    let mut store = StateStore::open(":memory:").unwrap();
    seed(&mut store);
    let initial = begin(&mut store, "retirement-validation", false);
    assert!(matches!(
        store.retire_outgoing_journal_fenced(&initial.handle, 0, 1001, STORAGE_GUARD_MS),
        Err(StoreError::OutgoingJournal(JournalError::StillActive))
    ));
    let canceled = append(&mut store, &initial, JournalEvent::Cancel, 1001);
    assert!(matches!(
        store.retire_outgoing_journal_fenced(&canceled.handle, 0, 1002, STORAGE_GUARD_MS),
        Err(StoreError::OutgoingJournal(JournalError::Revision))
    ));
    assert!(matches!(
        store.retire_outgoing_journal_fenced(&canceled.handle, 1, 1000, STORAGE_GUARD_MS),
        Err(StoreError::OutgoingJournal(JournalError::Clock))
    ));
    assert!(matches!(
        store.retire_outgoing_journal_fenced(&canceled.handle, 1, 1002, 0),
        Err(StoreError::OutgoingJournal(JournalError::GuardDuration))
    ));
    assert!(
        store
            .retire_outgoing_journal_fenced(&canceled.handle, 1, i64::MAX, STORAGE_GUARD_MS)
            .is_err()
    );
    assert!(
        store
            .outgoing_journal(&canceled.handle)
            .unwrap()
            .retirement
            .is_none()
    );
    let receipt = store
        .retire_outgoing_journal_fenced(&canceled.handle, 1, 1002, STORAGE_GUARD_MS)
        .unwrap();
    assert!(!receipt.already_committed);
    let retry = store
        .retire_outgoing_journal_fenced(&canceled.handle, 1, 1002, STORAGE_GUARD_MS)
        .unwrap();
    assert!(retry.already_committed);
    assert_eq!(receipt.retirement, retry.retirement);
    assert!(matches!(
        store.retire_outgoing_journal_fenced(&canceled.handle, 1, 1003, STORAGE_GUARD_MS - 1),
        Err(StoreError::OutgoingJournal(JournalError::Conflict))
    ));
    assert!(matches!(
        store.append_outgoing_event(&canceled.handle, 1, JournalEvent::Advance, 1003),
        Err(StoreError::OutgoingJournal(JournalError::Closed))
    ));
    assert!(
        store
            .append_outgoing_event(&canceled.handle, 0, JournalEvent::Cancel, 1001)
            .unwrap()
            .retirement
            .is_some()
    );
    assert_eq!(store.outgoing_journals(0, 10).unwrap().len(), 1);
    assert!(store.active_outgoing_journals(0, 10).unwrap().is_empty());
}
#[test]
fn three_full_wire_id_cycles_preserve_history_and_guards_across_restart() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    seed(&mut store);
    let mut histories = Vec::new();
    let mut start = 1000;
    for cycle in 0..3 {
        let mut batch = Vec::new();
        for index in 0..8 {
            let id = queue(&mut store, &format!("cycle-{cycle}-{index}"), false);
            let snapshot = store.begin_outgoing_journal(request(id), start).unwrap();
            assert_eq!(snapshot.plan.segments()[0].transaction_id, index);
            let waiting = submit_all(&mut store, snapshot, true);
            let done = append(&mut store, &waiting, received(&waiting), start + 20);
            assert!(store.relay_accepted(id).unwrap());
            batch.push(done);
        }
        let ninth = queue(&mut store, &format!("guarded-{cycle}"), false);
        for done in &batch {
            store
                .retire_outgoing_journal_fenced(
                    &done.handle,
                    done.revision(),
                    start + 21,
                    STORAGE_GUARD_MS,
                )
                .unwrap();
        }
        assert!(matches!(
            store.begin_outgoing_journal(request(ninth), start + 120),
            Err(StoreError::OutgoingJournal(JournalError::Capacity))
        ));
        histories.extend(batch);
        drop(store);
        store = StateStore::open(&database.0).unwrap();
        assert!(store.active_outgoing_journals(0, 32).unwrap().is_empty());
        start += 121;
    }
    let id = queue(&mut store, "new-owner", false);
    let active = store.begin_outgoing_journal(request(id), start).unwrap();
    assert_eq!(active.plan.segments()[0].transaction_id, 0);
    for old in &histories {
        let recovered = store.outgoing_journal(&old.handle).unwrap();
        assert_eq!(recovered.plan, old.plan);
        assert_eq!(recovered.events, old.events);
        assert!(recovered.retirement.is_some());
        assert_ne!(old.handle, active.handle);
        assert!(
            store
                .append_outgoing_event(&old.handle, old.revision(), JournalEvent::Cancel, start)
                .is_err()
        );
    }
    assert_eq!(store.active_outgoing_journals(0, 32).unwrap().len(), 1);
    assert_eq!(store.outgoing_journals(0, 32).unwrap().len(), 25);
}
#[test]
fn failed_retirement_write_rolls_back_the_lease_and_preserves_its_owner() {
    let mut store = StateStore::open(":memory:").unwrap();
    seed(&mut store);
    let initial = begin(&mut store, "retire-rollback", false);
    let closed = append(&mut store, &initial, JournalEvent::Cancel, 1001);
    store.connection.execute_batch("CREATE TRIGGER reject_retirement BEFORE UPDATE OF retired_at_unix_ms ON tsar_outgoing BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
    assert!(
        store
            .retire_outgoing_journal_fenced(&closed.handle, 1, 1002, STORAGE_GUARD_MS)
            .is_err()
    );
    assert!(
        store
            .outgoing_journal(&closed.handle)
            .unwrap()
            .retirement
            .is_none()
    );
    assert_eq!(store.active_outgoing_journals(0, 2).unwrap().len(), 1);
    store
        .connection
        .execute_batch("DROP TRIGGER reject_retirement")
        .unwrap();
    assert!(
        !store
            .retire_outgoing_journal_fenced(&closed.handle, 1, 1002, STORAGE_GUARD_MS)
            .unwrap()
            .already_committed
    );
}
#[test]
fn schema_eight_upgrade_preserves_bound_history_and_autoincrement_high_water() {
    use crate::core::store::{
        INCOMING_RESPONSE_SCHEMA, INCOMING_TSAR_SCHEMA, INITIAL_SCHEMA, MANAGEMENT_SCHEMA,
        MANUAL_RECEIPT_SCHEMA, OUTGOING_TSAR_SCHEMA, RECEIPT_SCHEMA, RELAY_ACCEPTANCE_SCHEMA,
    };
    let source = Database::new();
    let mut store = StateStore::open(&source.0).unwrap();
    seed(&mut store);
    let initial = begin(&mut store, "before-upgrade", false);
    let submitted = append(
        &mut store,
        &initial,
        JournalEvent::Submitted {
            sequence: 0,
            queued_at_unix_ms: 1001,
        },
        1001,
    );
    let incoming_binding = crate::core::store::incoming_journal::IncomingBinding {
        peer: PEER,
        protocol: service(),
        transaction_id: 7,
        version: ProtocolVersion::Pv1,
        context: b"legacy incoming".to_vec(),
    };
    let incoming_limits = crate::core::store::incoming_journal::JournalLimits {
        maximum_events: 8,
        maximum_context_octets: 128,
    };
    let incoming = store
        .begin_incoming_journal(incoming_binding.clone(), incoming_limits, 0)
        .unwrap();
    store
        .append_incoming_event(
            &incoming.handle,
            0,
            &crate::core::store::incoming_journal::JournalEvent::Cancel,
            1,
        )
        .unwrap();
    store
        .retire_incoming_journal_fenced(&incoming.handle, 1, 1, 100)
        .unwrap();
    drop(store);
    let target = Database::new();
    let connection = rusqlite::Connection::open(&target.0).unwrap();
    for schema in [
        INITIAL_SCHEMA,
        RECEIPT_SCHEMA,
        MANUAL_RECEIPT_SCHEMA,
        RELAY_ACCEPTANCE_SCHEMA,
        MANAGEMENT_SCHEMA,
        INCOMING_TSAR_SCHEMA,
        INCOMING_RESPONSE_SCHEMA,
        OUTGOING_TSAR_SCHEMA,
    ] {
        connection.execute_batch(schema).unwrap();
    }
    connection
        .execute(
            "ATTACH DATABASE ?1 AS original",
            [source.0.to_str().unwrap()],
        )
        .unwrap();
    for table in [
        "subscribers",
        "provisioning_state",
        "sms_queue",
        "sms_receipts",
        "tsar_incoming",
        "tsar_incoming_events",
        "tsar_incoming_responses",
        "tsar_outgoing",
        "tsar_outgoing_segments",
        "tsar_outgoing_events",
    ] {
        let columns: Vec<String> = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let columns = columns.join(",");
        if table == "provisioning_state" {
            connection
                .execute("DELETE FROM provisioning_state", [])
                .unwrap();
        }
        connection
            .execute_batch(&format!(
                "INSERT INTO {table}({columns}) SELECT {columns} FROM original.{table}"
            ))
            .unwrap();
    }
    connection.execute_batch("UPDATE sqlite_sequence SET seq=999 WHERE name='tsar_outgoing'; PRAGMA user_version=8; DETACH DATABASE original").unwrap();
    drop(connection);
    let mut upgraded = StateStore::open(&target.0).unwrap();
    let recovered = upgraded.outgoing_journal(&submitted.handle).unwrap();
    assert_eq!(recovered.plan, submitted.plan);
    assert_eq!(recovered.events, submitted.events);
    assert_eq!(recovered.context, submitted.context);
    assert_eq!(
        recovered.replay().unwrap().deadline_monotonic_ms(),
        Some(331001)
    );
    assert!(recovered.retirement.is_none());
    assert_eq!(
        upgraded
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        crate::core::store::SCHEMA_VERSION
    );
    assert!(
        upgraded
            .connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        upgraded
            .incoming_journal(&incoming.handle)
            .unwrap()
            .reuse_after_unix_ms,
        Some(101)
    );
    assert!(matches!(
        upgraded.retire_incoming_journal_fenced(&incoming.handle, 1, 1, 100),
        Err(StoreError::IncomingJournal(
            crate::core::store::incoming_journal::JournalError::Closed
        ))
    ));
    assert!(matches!(
        upgraded.begin_incoming_journal(incoming_binding.clone(), incoming_limits, 100),
        Err(StoreError::IncomingJournal(
            crate::core::store::incoming_journal::JournalError::Guarded
        ))
    ));
    assert_ne!(
        upgraded
            .begin_incoming_journal(incoming_binding, incoming_limits, 101)
            .unwrap()
            .handle,
        incoming.handle
    );
    let id = queue(&mut upgraded, "after-upgrade", false);
    let next = upgraded.begin_outgoing_journal(request(id), 1002).unwrap();
    assert_eq!(next.handle.id(), 1000);
}

#[test]
fn concurrent_allocators_cannot_share_a_released_wire_id() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    seed(&mut store);
    for index in 0..8 {
        let initial = begin(&mut store, &format!("old-{index}"), false);
        let canceled = append(&mut store, &initial, JournalEvent::Cancel, 1001);
        store
            .retire_outgoing_journal_fenced(&canceled.handle, 1, 1002, STORAGE_GUARD_MS)
            .unwrap();
    }
    let ids: Vec<_> = (0..9)
        .map(|index| queue(&mut store, &format!("racing-{index}"), false))
        .collect();
    drop(store);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(ids.len()));
    let threads: Vec<_> = ids
        .into_iter()
        .map(|id| {
            let path = database.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = StateStore::open(path).unwrap();
                barrier.wait();
                match store.begin_outgoing_journal(request(id), 1102) {
                    Ok(snapshot) => Some(snapshot.plan.segments()[0].transaction_id),
                    Err(StoreError::OutgoingJournal(JournalError::Capacity)) => None,
                    Err(error) => panic!("unexpected allocator failure: {error}"),
                }
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|id| id.is_none()).count(), 1);
    let mut allocated: Vec<_> = results.into_iter().flatten().collect();
    allocated.sort_unstable();
    assert_eq!(allocated, (0..8).collect::<Vec<_>>());
}
#[test]
fn invalid_foreign_keys_roll_back_the_entire_schema_eight_rebuild() {
    use crate::core::store::{
        INCOMING_RESPONSE_SCHEMA, INCOMING_TSAR_SCHEMA, INITIAL_SCHEMA, MANAGEMENT_SCHEMA,
        MANUAL_RECEIPT_SCHEMA, OUTGOING_TSAR_SCHEMA, RECEIPT_SCHEMA, RELAY_ACCEPTANCE_SCHEMA,
    };
    let database = Database::new();
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    for schema in [
        INITIAL_SCHEMA,
        RECEIPT_SCHEMA,
        MANUAL_RECEIPT_SCHEMA,
        RELAY_ACCEPTANCE_SCHEMA,
        MANAGEMENT_SCHEMA,
        INCOMING_TSAR_SCHEMA,
        INCOMING_RESPONSE_SCHEMA,
        OUTGOING_TSAR_SCHEMA,
    ] {
        connection.execute_batch(schema).unwrap();
    }
    connection.execute("INSERT INTO tsar_outgoing_events(stream_id,ordinal,observed_at_unix_ms,event) VALUES(999,0,0,?1)",[b"Advance\n".as_slice()]).unwrap();
    connection.execute_batch("PRAGMA user_version=8").unwrap();
    drop(connection);
    assert!(matches!(
        StateStore::open(&database.0),
        Err(StoreError::CorruptState)
    ));
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        8
    );
    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(tsar_outgoing)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!columns.iter().any(|column| column == "retired_at_unix_ms"));
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM tsar_outgoing_events", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
}
