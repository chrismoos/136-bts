use super::*;
use crate::l3::sms::{DisplayTime, Metadata, UserData, Validity};
const UNIX_MS: i64 = 1000;
const GUARD_MS: u64 = 10;
const PEER: MobileIdentity = MobileIdentity::Min34(1);
fn metadata() -> Metadata {
    Metadata {
        message_reference: 12,
        privacy: 0,
        urgency: 0,
        delivery_acknowledgment: false,
        manual_acknowledgment: false,
        new_message: true,
    }
}
fn request(id: u8) -> Vec<u8> {
    let mut bytes = vec![id];
    bytes.extend(
        SmsMessage::Submit {
            metadata: metadata(),
            user_data: UserData::Ira(b"hello".to_vec()),
        }
        .encode()
        .unwrap(),
    );
    bytes
}
fn rejection(id: u8, cause: u8) -> Vec<u8> {
    RelayMessage::Reject {
        transaction_id: id,
        cause,
    }
    .encode(Direction::Forward)
    .unwrap()
}
#[test]
fn request_keys_preserve_the_original_guard_and_distinguish_new_generations() {
    let mut store = StateStore::open(":memory:").unwrap();
    let first = store
        .claim_ordinary_request(PEER, &request(4), &[1], UNIX_MS, GUARD_MS)
        .unwrap();
    let duplicate = store
        .claim_ordinary_request(
            PEER,
            &request(4),
            &[1],
            UNIX_MS + GUARD_MS as i64 - 1,
            GUARD_MS * 2,
        )
        .unwrap();
    assert_eq!(first.submission_key(), duplicate.submission_key());
    assert_eq!(duplicate.expires_at_unix_ms, UNIX_MS + GUARD_MS as i64);
    let next = store
        .claim_ordinary_request(PEER, &request(4), &[1], UNIX_MS + GUARD_MS as i64, GUARD_MS)
        .unwrap();
    assert_ne!(first.submission_key(), next.submission_key());
    store
        .settle_ordinary_request(&first, &rejection(4, 95), UNIX_MS + GUARD_MS as i64)
        .unwrap();
    let current = store
        .claim_ordinary_request(PEER, &request(4), &[1], UNIX_MS + GUARD_MS as i64, GUARD_MS)
        .unwrap();
    assert_eq!(current.submission_key(), next.submission_key());
    assert!(current.response.is_none());
}
#[test]
fn request_identity_includes_peer_payload_and_optional_fields() {
    let mut store = StateStore::open(":memory:").unwrap();
    let first = store
        .claim_ordinary_request(PEER, &request(4), &[], UNIX_MS, GUARD_MS)
        .unwrap();
    let option = store
        .claim_ordinary_request(PEER, &request(4), &[1], UNIX_MS, GUARD_MS)
        .unwrap();
    let other = store
        .claim_ordinary_request(
            MobileIdentity::Min34(2),
            &request(4),
            &[],
            UNIX_MS,
            GUARD_MS,
        )
        .unwrap();
    let mut changed = request(4);
    *changed.last_mut().unwrap() = b'!';
    let payload = store
        .claim_ordinary_request(PEER, &changed, &[], UNIX_MS, GUARD_MS)
        .unwrap();
    let keys = [first, option, other, payload].map(|owner| owner.submission_key());
    let unique: std::collections::HashSet<_> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len());
}
#[test]
fn private_request_owners_cannot_cross_database_boundaries() {
    let mut first = StateStore::open(":memory:").unwrap();
    let mut second = StateStore::open(":memory:").unwrap();
    let a = first
        .claim_ordinary_request(PEER, &request(4), &[], UNIX_MS, GUARD_MS)
        .unwrap();
    let b = second
        .claim_ordinary_request(PEER, &request(4), &[], UNIX_MS, GUARD_MS)
        .unwrap();
    assert_eq!(a.id, b.id);
    assert_ne!(a.submission_key(), b.submission_key());
    assert!(second.ordinary_request_sms(&a).is_err());
    assert!(
        second
            .settle_ordinary_request(&a, &rejection(4, 95), UNIX_MS)
            .is_err()
    );
    let unchanged = second
        .claim_ordinary_request(PEER, &request(4), &[], UNIX_MS, GUARD_MS)
        .unwrap();
    assert!(unchanged.response.is_none());
}
#[test]
fn recorded_response_is_immutable_and_exact_retries_do_not_slide_its_timestamp() {
    let mut store = StateStore::open(":memory:").unwrap();
    let owner = store
        .claim_ordinary_request(PEER, &request(4), &[], UNIX_MS, GUARD_MS)
        .unwrap();
    let accepted = RelayMessage::Accept { transaction_id: 4 }
        .encode(Direction::Forward)
        .unwrap();
    assert!(
        store
            .settle_ordinary_request(&owner, &accepted, UNIX_MS)
            .is_err()
    );
    assert!(
        store
            .settle_ordinary_request(&owner, &rejection(5, 95), UNIX_MS)
            .is_err()
    );
    store
        .settle_ordinary_request(&owner, &rejection(4, 95), UNIX_MS + 1)
        .unwrap();
    store
        .settle_ordinary_request(&owner, &rejection(4, 95), UNIX_MS + 2)
        .unwrap();
    assert!(
        store
            .settle_ordinary_request(&owner, &rejection(4, 3), UNIX_MS + 2)
            .is_err()
    );
    let settled: i64 = store
        .connection
        .query_row(
            "SELECT settled_at_unix_ms FROM ordinary_sms_requests",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(settled, UNIX_MS + 1);
}
#[test]
fn active_request_capacity_reopens_only_after_fixed_expiry() {
    let mut store = StateStore::open(":memory:").unwrap();
    for id in 0..MAX_ACTIVE_REQUESTS {
        store
            .claim_ordinary_request(PEER, &request(id as u8), &[], UNIX_MS, GUARD_MS)
            .unwrap();
    }
    assert!(matches!(
        store.claim_ordinary_request(
            MobileIdentity::Min34(2),
            &request(0),
            &[],
            UNIX_MS,
            GUARD_MS
        ),
        Err(StoreError::SubmissionCapacity)
    ));
    store
        .claim_ordinary_request(
            MobileIdentity::Min34(2),
            &request(0),
            &[],
            UNIX_MS + GUARD_MS as i64,
            GUARD_MS,
        )
        .unwrap();
    let count: u32 = store
        .connection
        .query_row("SELECT count(*) FROM ordinary_sms_requests", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, MAX_ACTIVE_REQUESTS + 1);
}
#[test]
fn regression_and_unrepresentable_guards_do_not_create_another_owner() {
    let mut store = StateStore::open(":memory:").unwrap();
    let owner = store
        .claim_ordinary_request(PEER, &request(4), &[], UNIX_MS, GUARD_MS)
        .unwrap();
    store
        .settle_ordinary_request(&owner, &rejection(4, 95), UNIX_MS + 1)
        .unwrap();
    assert!(
        store
            .claim_ordinary_request(PEER, &request(5), &[], UNIX_MS, GUARD_MS)
            .is_err()
    );
    assert!(
        store
            .claim_ordinary_request(PEER, &request(5), &[], UNIX_MS + 1, u64::MAX)
            .is_err()
    );
    assert!(
        store
            .claim_ordinary_request(PEER, &request(5), &[], UNIX_MS + 1, 0)
            .is_err()
    );
    let count: u32 = store
        .connection
        .query_row("SELECT count(*) FROM ordinary_sms_requests", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}
#[test]
fn schema_nine_upgrade_preserves_sms_history_and_the_next_identity() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::path::PathBuf::from(format!(
        "/var/tmp/136bts-ordinary-migration-{}-{stamp}.sqlite3",
        std::process::id()
    ));
    let mut store = StateStore::open(&path).unwrap();
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
    let message = SmsMessage::Deliver {
        metadata: metadata(),
        validity: Validity::Indefinite,
        display_time: DisplayTime::Default,
        user_data: UserData::Ira(b"old".to_vec()),
    };
    let old = store
        .enqueue(NewSms {
            dedup_key: "before",
            sender: PEER,
            recipient: MobileIdentity::Min34(2),
            message: &message,
            created_at_unix_ms: UNIX_MS,
        })
        .unwrap();
    store.mark_delivered(old).unwrap();
    store
        .connection
        .execute_batch("DROP TABLE sms_hub_parts; DROP TABLE sms_hub_outbox; DROP TABLE sms_hub_inbox; DROP TABLE esn_subscribers; DROP TABLE broadcast_revisions; DROP TABLE ordinary_sms_requests")
        .unwrap();
    store
        .connection
        .pragma_update(None, "user_version", OUTGOING_RETIREMENT_SCHEMA_VERSION)
        .unwrap();
    drop(store);
    let mut upgraded = StateStore::open(&path).unwrap();
    assert_eq!(upgraded.state(old).unwrap(), QueueState::Delivered);
    let next = upgraded
        .enqueue(NewSms {
            dedup_key: "after",
            sender: PEER,
            recipient: MobileIdentity::Min34(2),
            message: &message,
            created_at_unix_ms: UNIX_MS + 1,
        })
        .unwrap();
    assert!(next.value() > old.value());
    assert_eq!(
        upgraded
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    assert!(
        !upgraded
            .connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    drop(upgraded);
    std::fs::remove_file(path).unwrap();
}
