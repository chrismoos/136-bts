use super::*;
use crate::{
    config::Config, l3::broadcast::MandatoryBroadcast, scheduler::broadcast::BroadcastContent,
};
use std::path::PathBuf;

fn contents(sid: u16) -> Vec<u8> {
    let config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let mut messages = crate::runtime::serving::messages(&config);
    let MandatoryBroadcast::SystemIdentity(identity) = &mut messages[4] else {
        panic!()
    };
    identity.sid = sid;
    BroadcastContent::try_new(
        crate::l3::designation::AssignedSlots::Full14,
        messages,
        vec![],
    )
    .unwrap()
    .storage_bytes()
    .unwrap()
}
fn cell() -> CellKey {
    CellKey::try_new(Band::Cellular800, 991).unwrap()
}
fn flags(changed: bool) -> BroadcastSignaling {
    BroadcastSignaling {
        changes: BroadcastChanges {
            fbcch: changed,
            ebcch: false,
        },
        notification: changed,
    }
}
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "136bts-broadcast-{}-{stamp}.sqlite3",
            std::process::id()
        )))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn restart_retains_unconfirmed_intent_and_previous_confirmed_content() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    assert!(store.broadcast_recovery(cell()).unwrap().pending.is_none());
    let first = store
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
        .unwrap();
    drop(store);
    let mut store = StateStore::open(&database.0).unwrap();
    let recovered = store.broadcast_recovery(cell()).unwrap();
    assert!(recovered.confirmed.is_none());
    let pending = recovered.pending.unwrap();
    assert!(pending.same_revision(&first));
    assert_eq!(pending.id, first.id);
    assert_eq!(pending.contents(), contents(42));
    let retry = store
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 20)
        .unwrap();
    assert_eq!(retry.id, first.id);
    assert_eq!(retry.prepared_at_unix_ms(), 10);
    let first = store.confirm_broadcast_revision(&pending, 30).unwrap();
    assert!(first.same_revision(&pending));
    let second = store
        .prepare_broadcast_revision(cell(), &contents(43), flags(true), 40)
        .unwrap();
    drop(store);
    let mut store = StateStore::open(&database.0).unwrap();
    let state = store.broadcast_recovery(cell()).unwrap();
    assert_eq!(state.confirmed.unwrap().id, first.id);
    assert_eq!(state.pending.unwrap().id, second.id);
    assert_eq!(
        store
            .confirm_broadcast_revision(&first, 50)
            .unwrap()
            .confirmed_at_unix_ms(),
        Some(30)
    );
    assert_eq!(
        store
            .broadcast_recovery(cell())
            .unwrap()
            .pending
            .unwrap()
            .id,
        second.id
    );
    store.confirm_broadcast_revision(&second, 60).unwrap();
    assert_eq!(
        store
            .confirm_broadcast_revision(&second, 70)
            .unwrap()
            .confirmed_at_unix_ms(),
        Some(60)
    );
    drop(store);
    let store = StateStore::open(&database.0).unwrap();
    let state = store.broadcast_recovery(cell()).unwrap();
    assert!(state.pending.is_none());
    assert_eq!(state.confirmed.unwrap().contents(), contents(43));
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM broadcast_revisions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn conflicts_clock_regression_and_foreign_owner_cannot_confirm_or_replace_a_revision() {
    let mut store = StateStore::open(":memory:").unwrap();
    let mut foreign = StateStore::open(":memory:").unwrap();
    let pending = store
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
        .unwrap();
    foreign
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
        .unwrap();
    assert!(matches!(
        foreign.confirm_broadcast_revision(&pending, 20),
        Err(Error::UnknownOwner)
    ));
    assert!(matches!(
        store.prepare_broadcast_revision(cell(), &contents(43), flags(true), 20),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        store.confirm_broadcast_revision(&pending, 9),
        Err(Error::Clock)
    ));
    store.confirm_broadcast_revision(&pending, 20).unwrap();
    assert_eq!(
        store
            .prepare_broadcast_revision(cell(), &contents(42), flags(false), 30)
            .unwrap()
            .id,
        pending.id
    );
    assert!(matches!(
        store.prepare_broadcast_revision(cell(), &contents(43), flags(true), 19),
        Err(Error::Clock)
    ));
    assert!(matches!(
        store.prepare_broadcast_revision(cell(), &[], flags(true), 30),
        Err(Error::Contents)
    ));
    assert!(matches!(
        store.prepare_broadcast_revision(cell(), &vec![0; MAX_CONTENT_OCTETS + 1], flags(true), 30),
        Err(Error::Contents)
    ));
    let state = store.broadcast_recovery(cell()).unwrap();
    assert!(state.pending.is_none());
    assert_eq!(state.confirmed.unwrap().id, pending.id);
}

#[test]
fn failed_sql_writes_never_publish_a_preparation_or_confirmation() {
    let mut store = StateStore::open(":memory:").unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_broadcast BEFORE INSERT ON broadcast_revisions BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
    assert!(matches!(
        store.prepare_broadcast_revision(cell(), &contents(42), flags(false), 10),
        Err(Error::Sql(_))
    ));
    assert!(store.broadcast_recovery(cell()).unwrap().pending.is_none());
    store
        .connection
        .execute_batch("DROP TRIGGER fail_broadcast")
        .unwrap();
    let pending = store
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
        .unwrap();
    store.connection.execute_batch("CREATE TRIGGER ignore_broadcast BEFORE UPDATE ON broadcast_revisions BEGIN SELECT RAISE(IGNORE); END").unwrap();
    assert!(matches!(
        store.confirm_broadcast_revision(&pending, 20),
        Err(Error::Corrupt)
    ));
    let state = store.broadcast_recovery(cell()).unwrap();
    assert!(state.confirmed.is_none());
    assert_eq!(state.pending.unwrap().id, pending.id);
    store
        .connection
        .execute_batch("DROP TRIGGER ignore_broadcast")
        .unwrap();
    store.confirm_broadcast_revision(&pending, 20).unwrap();
}

#[test]
fn schema_ten_upgrade_preserves_existing_request_rows() {
    let database = Database::new();
    let store = StateStore::open(&database.0).unwrap();
    store.connection.execute_batch("INSERT INTO ordinary_sms_requests(sender_type,sender_value,request,optional_octets,observed_at_unix_ms,expires_at_unix_ms) VALUES(2,1,X'0100',X'',10,20); DROP TABLE sms_hub_parts; DROP TABLE sms_hub_outbox; DROP TABLE sms_hub_inbox; DROP TABLE esn_subscribers; DROP TABLE broadcast_revisions; PRAGMA user_version=10").unwrap();
    let before: Vec<u8> = store
        .connection
        .query_row("SELECT owner FROM ordinary_sms_requests", [], |row| {
            row.get(0)
        })
        .unwrap();
    drop(store);
    let store = StateStore::open(&database.0).unwrap();
    assert_eq!(
        store
            .connection
            .query_row("SELECT owner FROM ordinary_sms_requests", [], |row| row
                .get::<_, Vec<u8>>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT request FROM ordinary_sms_requests", [], |row| row
                .get::<_, Vec<
                u8,
            >>(
                0
            ))
            .unwrap(),
        vec![1, 0]
    );
    assert_eq!(
        store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        crate::core::store::SCHEMA_VERSION
    );
    assert!(store.broadcast_recovery(cell()).unwrap().pending.is_none());
}

#[test]
fn concurrent_preparations_share_one_pending_owner_and_preserve_confirmation() {
    let database = Database::new();
    drop(StateStore::open(&database.0).unwrap());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let path = database.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = StateStore::open(path).unwrap();
                barrier.wait();
                store
                    .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
                    .unwrap()
            })
        })
        .collect();
    let revisions: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(revisions[0].id, revisions[1].id);
    assert_eq!(revisions[0].owner, revisions[1].owner);
    let mut store = StateStore::open(&database.0).unwrap();
    store.confirm_broadcast_revision(&revisions[0], 20).unwrap();
    assert_eq!(
        store
            .confirm_broadcast_revision(&revisions[1], 30)
            .unwrap()
            .confirmed_at_unix_ms(),
        Some(20)
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM broadcast_revisions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn band_keys_and_changed_signaling_are_part_of_revision_identity() {
    let mut store = StateStore::open(":memory:").unwrap();
    let other = CellKey::try_new(Band::Pcs1900, 991).unwrap();
    let first = store
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
        .unwrap();
    store.confirm_broadcast_revision(&first, 20).unwrap();
    let second = store
        .prepare_broadcast_revision(cell(), &contents(42), flags(true), 30)
        .unwrap();
    assert_ne!(first.id, second.id);
    assert!(!first.same_revision(&second));
    let foreign = store
        .prepare_broadcast_revision(other, &contents(43), flags(false), 40)
        .unwrap();
    let forged_cell = Revision {
        cell: other,
        ..second.clone()
    };
    assert!(matches!(
        store.confirm_broadcast_revision(&forged_cell, 50),
        Err(Error::UnknownOwner)
    ));
    assert_eq!(
        store
            .broadcast_recovery(cell())
            .unwrap()
            .pending
            .unwrap()
            .id,
        second.id
    );
    assert_eq!(
        store.broadcast_recovery(other).unwrap().pending.unwrap().id,
        foreign.id
    );
    assert!(CellKey::try_new(Band::Cellular800, 990).is_err());
    assert!(CellKey::try_new(Band::Pcs1900, 1999).is_err());
}

#[test]
fn predecessor_is_owned_and_scoped_to_the_same_cell() {
    let mut store = StateStore::open(":memory:").unwrap();
    let mut foreign = StateStore::open(":memory:").unwrap();
    let first = store
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
        .unwrap();
    assert!(store.broadcast_predecessor(&first).unwrap().is_none());
    store.confirm_broadcast_revision(&first, 20).unwrap();
    let other_cell = CellKey::try_new(Band::Cellular800, 992).unwrap();
    let other = store
        .prepare_broadcast_revision(other_cell, &contents(45), flags(false), 30)
        .unwrap();
    store.confirm_broadcast_revision(&other, 40).unwrap();
    let next = store
        .prepare_broadcast_revision(cell(), &contents(43), flags(true), 50)
        .unwrap();
    assert!(
        store
            .broadcast_predecessor(&next)
            .unwrap()
            .unwrap()
            .same_revision(&first)
    );
    let unrelated = foreign
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
        .unwrap();
    assert!(matches!(
        store.broadcast_predecessor(&unrelated),
        Err(Error::UnknownOwner)
    ));
}

#[test]
fn latest_timestamp_covers_broadcast_revisions() {
    let database = Database::new();
    let mut store = StateStore::open(&database.0).unwrap();
    assert_eq!(store.latest_unix_ms().unwrap(), None);
    let pending = store
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
        .unwrap();
    assert_eq!(store.latest_unix_ms().unwrap(), Some(10));
    store.confirm_broadcast_revision(&pending, 30).unwrap();
    assert_eq!(store.latest_unix_ms().unwrap(), Some(30));
}

#[test]
fn replicated_revisions_reach_the_file_and_survive_reopen() {
    let database = Database::new();
    let mut store = StateStore::open_replicated(&database.0).unwrap();
    let first = store
        .prepare_broadcast_revision(cell(), &contents(42), flags(false), 10)
        .unwrap();
    store.confirm_broadcast_revision(&first, 30).unwrap();
    store
        .prepare_broadcast_revision(cell(), &contents(43), flags(true), 40)
        .unwrap();
    assert!(store.drain(std::time::Duration::from_secs(5)).unwrap());
    let expected = store.rows();
    assert_eq!(
        crate::core::store::replica::table_rows(&rusqlite::Connection::open(&database.0).unwrap()),
        expected
    );
    drop(store);
    let reopened = StateStore::open_replicated(&database.0).unwrap();
    let recovered = reopened.broadcast_recovery(cell()).unwrap();
    assert!(recovered.confirmed.unwrap().same_revision(&first));
    assert!(recovered.pending.is_some());
}
