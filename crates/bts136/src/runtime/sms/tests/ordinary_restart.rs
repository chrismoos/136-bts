use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
const UNIX_MS: i64 = 1000;
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(PathBuf::from(format!(
            "/var/tmp/136bts-ordinary-restart-{}-{}.sqlite3",
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
fn reopen(database: &Database, session: &str, unix_ms: i64) -> SmsRuntime {
    let mut store = StateStore::open(&database.0).unwrap();
    store
        .seed_once(&[
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
    let template = runtime();
    let mut reopened = SmsRuntime::try_new(
        LocalMessaging::from_store(store).unwrap(),
        template.allocation,
        session.into(),
    )
    .unwrap();
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
    reopened
        .set_access_parameters(parameters, super::super::super::timers::SUPERFRAME_MS)
        .unwrap();
    reopened.start_incoming_sms(0, unix_ms).unwrap();
    for id in [1, 2] {
        reopened
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
    reopened
}
#[test]
fn accepted_ordinary_sms_replay_after_restart_does_not_insert_a_second_message() {
    let database = Database::new();
    let mut first = reopen(&database, "before", UNIX_MS);
    first.receive(&submission(), 0, UNIX_MS).unwrap();
    let before = first
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(before.len(), 1);
    let id = before[0].id;
    drop(first);
    let mut recovered = reopen(&database, "after", UNIX_MS + 1);
    recovered.receive(&submission(), 0, UNIX_MS + 1).unwrap();
    let after = recovered
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, id);
}

#[test]
fn ordinary_response_storage_failure_recovers_inserted_sms_after_renumbering() {
    let database = Database::new();
    let mut first = reopen(&database, "before", UNIX_MS);
    let sql = rusqlite::Connection::open(&database.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_response BEFORE UPDATE OF response ON ordinary_sms_requests BEGIN SELECT RAISE(FAIL,'injected response write failure'); END;").unwrap();
    assert!(first.receive(&submission(), 0, UNIX_MS).is_err());
    let stored = first
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(stored.len(), 1);
    first
        .core_mut()
        .save_subscriber(ProvisionedSubscriber {
            identity: MobileIdentity::Min34(2),
            number: "2002".into(),
        })
        .unwrap();
    sql.execute_batch("DROP TRIGGER fail_response;").unwrap();
    drop(first);
    let mut recovered = reopen(&database, "after", UNIX_MS + 1);
    recovered.receive(&submission(), 0, UNIX_MS + 1).unwrap();
    let after = recovered
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, stored[0].id);
    assert_eq!(after[0].created_at_unix_ms, UNIX_MS);
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM ordinary_sms_requests WHERE response IS NOT NULL",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn ordinary_insertion_failure_recovers_the_original_write_ahead_owner() {
    let database = Database::new();
    let mut first = reopen(&database, "before", UNIX_MS);
    let sql = rusqlite::Connection::open(&database.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_sms BEFORE INSERT ON sms_queue BEGIN SELECT RAISE(FAIL,'injected SMS write failure'); END;").unwrap();
    assert!(first.receive(&submission(), 0, UNIX_MS).is_err());
    assert!(
        first
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .is_empty()
    );
    let owner: i64 = sql
        .query_row("SELECT id FROM ordinary_sms_requests", [], |row| row.get(0))
        .unwrap();
    sql.execute_batch("DROP TRIGGER fail_sms;").unwrap();
    drop(first);
    let mut recovered = reopen(&database, "after", UNIX_MS + 1);
    recovered.receive(&submission(), 0, UNIX_MS + 1).unwrap();
    let stored = recovered
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].created_at_unix_ms, UNIX_MS);
    assert_eq!(
        sql.query_row("SELECT id FROM ordinary_sms_requests", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        owner
    );
}

#[test]
fn ordinary_replay_after_restart_expires_at_the_original_boundary() {
    let database = Database::new();
    let mut first = reopen(&database, "before", UNIX_MS);
    let guard = first.submission_retry_guard_ms;
    first.receive(&submission(), 0, UNIX_MS).unwrap();
    drop(first);
    let mut recovered = reopen(&database, "after", UNIX_MS + guard as i64 - 1);
    recovered.receive(&submission(), 0, -1).unwrap();
    assert_eq!(
        recovered
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
    recovered.advance(1).unwrap();
    recovered.receive(&submission(), 1, -1).unwrap();
    assert_eq!(
        recovered
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        2
    );
    let sql = rusqlite::Connection::open(&database.0).unwrap();
    let expires: i64 = sql
        .query_row(
            "SELECT expires_at_unix_ms FROM ordinary_sms_requests ORDER BY id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(expires, UNIX_MS + guard as i64);
}

#[test]
fn durable_ordinary_request_rejects_a_regressed_restart_clock() {
    let database = Database::new();
    let mut first = reopen(&database, "before", UNIX_MS);
    first.receive(&submission(), 0, UNIX_MS).unwrap();
    drop(first);
    let store = StateStore::open(&database.0).unwrap();
    let mut restarted = SmsRuntime::try_new(
        LocalMessaging::from_store(store).unwrap(),
        runtime().allocation,
        "after".into(),
    )
    .unwrap();
    assert!(restarted.start_incoming_sms(0, UNIX_MS - 1).is_err());
    assert_eq!(
        restarted
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn full_traffic_submission_replay_survives_restart_without_dcch_conversion() {
    let database = Database::new();
    let mut first = reopen(&database, "before", UNIX_MS);
    let caller = super::peer(1, 7, true);
    first.sync_traffic(vec![caller], 0).unwrap();
    let request = super::full_traffic_submission();
    first.receive_traffic(caller, &request, 1, UNIX_MS).unwrap();
    let stored = first
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(stored.len(), 1);
    drop(first);
    let mut recovered = reopen(&database, "after", UNIX_MS + 2);
    let new_call = super::peer(1, 8, true);
    recovered.sync_traffic(vec![new_call], 0).unwrap();
    recovered
        .receive_traffic(new_call, &request, 1, UNIX_MS + 2)
        .unwrap();
    let after = recovered
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, stored[0].id);
    let sql = rusqlite::Connection::open(&database.0).unwrap();
    let optional: Vec<u8> = sql
        .query_row(
            "SELECT optional_octets FROM ordinary_sms_requests",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(optional, request.optional_octets);
}

#[test]
fn request_owner_write_failure_never_inserts_sms_or_queues_acceptance() {
    let database = Database::new();
    let mut first = reopen(&database, "before", UNIX_MS);
    let sql = rusqlite::Connection::open(&database.0).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_owner BEFORE INSERT ON ordinary_sms_requests BEGIN SELECT RAISE(FAIL,'injected request owner failure'); END;").unwrap();
    assert!(first.receive(&submission(), 0, UNIX_MS).is_err());
    assert!(
        first
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .is_empty()
    );
    assert!(first.pending.is_empty());
    assert_eq!(
        sql.query_row("SELECT count(*) FROM ordinary_sms_requests", [], |row| row
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
    sql.execute_batch("DROP TRIGGER fail_owner;").unwrap();
    drop(first);
    let mut recovered = reopen(&database, "after", UNIX_MS + 1);
    recovered.receive(&submission(), 0, UNIX_MS + 1).unwrap();
    assert_eq!(
        recovered
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn persisted_rejection_stays_fixed_after_provisioning_changes_and_restart() {
    let database = Database::new();
    let mut first = reopen(&database, "before", UNIX_MS);
    first
        .core_mut()
        .delete_subscriber(MobileIdentity::Min34(2))
        .unwrap();
    first.receive(&submission(), 0, UNIX_MS).unwrap();
    let sql = rusqlite::Connection::open(&database.0).unwrap();
    let response: Vec<u8> = sql
        .query_row("SELECT response FROM ordinary_sms_requests", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(matches!(
        relay::decode_prefix(&response, Direction::Forward)
            .unwrap()
            .message,
        RelayMessage::Reject { .. }
    ));
    first
        .core_mut()
        .save_subscriber(ProvisionedSubscriber {
            identity: MobileIdentity::Min34(2),
            number: "1002".into(),
        })
        .unwrap();
    drop(first);
    let mut recovered = reopen(&database, "after", UNIX_MS + 1);
    recovered.receive(&submission(), 0, UNIX_MS + 1).unwrap();
    assert!(
        recovered
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        sql.query_row("SELECT response FROM ordinary_sms_requests", [], |row| row
            .get::<_, Vec<
            u8,
        >>(
            0
        ))
        .unwrap(),
        response
    );
}

#[test]
fn delivered_ordinary_sms_replay_after_restart_does_not_requeue_delivery() {
    let database = Database::new();
    let mut first = reopen(&database, "before", UNIX_MS);
    first.receive(&submission(), 0, UNIX_MS).unwrap();
    let id = first
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap()[0]
        .id;
    first.core().store().mark_delivered(id).unwrap();
    drop(first);
    let mut recovered = reopen(&database, "after", UNIX_MS + 1);
    recovered.receive(&submission(), 0, UNIX_MS + 1).unwrap();
    assert!(
        recovered
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        recovered.core().store().state(id).unwrap(),
        crate::core::store::QueueState::Delivered
    );
    let sql = rusqlite::Connection::open(&database.0).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM sms_queue", [], |row| row
            .get::<_, u32>(0))
            .unwrap(),
        1
    );
}

#[test]
fn external_ordinary_acceptance_replays_after_restart_without_a_second_hub_job() {
    let database = Database::new();
    let mut first = reopen(&database, "external-before", UNIX_MS);
    first.core_mut().enable_sms_hub(true);
    let mut request = submission();
    let mut addressed =
        crate::l3::relay::addressing::decode(&request.messages[0], Direction::Reverse)
            .unwrap()
            .relay;
    addressed.addresses.destination.as_mut().unwrap().address = AddressValue::Tbcd("2000".into());
    request.messages[0] = addressed.encode(Direction::Reverse).unwrap();
    first.receive(&request, 0, UNIX_MS).unwrap();
    assert_eq!(first.core().store().hub_queue_depth().unwrap(), 1);
    let id = first.core().store().operation_messages(10).unwrap()[0].id;
    drop(first);
    let mut recovered = reopen(&database, "external-after", UNIX_MS + 1);
    recovered.receive(&request, 0, UNIX_MS + 1).unwrap();
    assert_eq!(recovered.core().store().hub_queue_depth().unwrap(), 1);
    let messages = recovered.core().store().operation_messages(10).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, id);
    assert!(
        recovered
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .is_empty()
    );
}
