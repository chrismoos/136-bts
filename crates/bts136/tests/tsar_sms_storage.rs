use bts136::{
    core::{
        messaging::LocalMessaging,
        registry::ProvisionedSubscriber,
        store::{QueueState, StateStore},
    },
    l3::{
        address::{AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        relay::{Direction, HigherLayerProtocol, Teleservice},
        sms::{Metadata, SmsMessage, UserData},
        tsar::{AcknowledgmentWindow, DecodedSegment, MessagePlan, plan_message},
    },
    link::{
        rach::MobileIdentity,
        tsar::{
            ApplicationOutcome, IncomingError, IncomingLimits, IncomingTransaction,
            IncomingTransactions,
        },
    },
    runtime::sms::tsar_submission::{self, SubmissionContext},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
const PEER: MobileIdentity = MobileIdentity::Min34(1);
const RECIPIENT: MobileIdentity = MobileIdentity::Min34(2);
const SMS: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
const MAX_CMT_USER_OCTETS: usize = 254;
static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        loop {
            let id = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "136bts-tsar-storage-{}-{id}.sqlite3",
                std::process::id()
            ));
            match std::fs::File::create_new(&path) {
                Ok(_) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("creating test database failed: {error}"),
            }
        }
    }
    fn open(&self) -> LocalMessaging {
        let mut store = StateStore::open(&self.0).unwrap();
        store
            .reconcile(&[
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
        LocalMessaging::from_store(store).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn destination(number: &str) -> AddressInfo {
    AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd(number.into()),
    }
}
fn pdu(text: &[u8]) -> Vec<u8> {
    SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 42,
            privacy: 0,
            urgency: 1,
            delivery_acknowledgment: true,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(text.into()),
    }
    .encode()
    .unwrap()
}
fn completed(
    payload: &[u8],
    protocol: HigherLayerProtocol,
) -> (IncomingTransactions, IncomingTransaction) {
    let MessagePlan::Segmented(plan) = plan_message(
        payload,
        10,
        3,
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
    )
    .unwrap() else {
        panic!("expected segmentation");
    };
    let mut bank = IncomingTransactions::try_new(
        PEER,
        protocol,
        IncomingLimits {
            maximum_message_octets: 300,
            maximum_reserved_payload_octets: 600,
        },
        100,
        0,
    )
    .unwrap();
    let start = bank
        .begin(
            PEER,
            protocol,
            &DecodedSegment {
                segment: plan.segments()[0].clone(),
                unparsed_octets: vec![],
            },
            ProtocolVersion::Pv1,
            0,
        )
        .unwrap();
    for (index, segment) in plan.segments().iter().enumerate().skip(1) {
        bank.receive(
            &start.transaction,
            PEER,
            protocol,
            &DecodedSegment {
                segment: segment.clone(),
                unparsed_octets: vec![],
            },
            index as u64,
        )
        .unwrap();
    }
    assert_eq!(
        bank.pending_message(&start.transaction)
            .unwrap()
            .unwrap()
            .bytes(),
        payload
    );
    (bank, start.transaction)
}
fn context<'a>(destination: &'a AddressInfo, key: &'a str, now: u64) -> SubmissionContext<'a> {
    SubmissionContext {
        destination,
        submission_key: key,
        created_at_unix_ms: 1000,
        monotonic_ms: now,
    }
}
#[test]
fn full_cmt_message_commits_after_reassembly_and_survives_restart_without_false_delivery() {
    use bts136::core::delivery::{DeliveryPolicy, SmsDelivery};
    use bts136::{
        l3::registration::{AccessMessage, RegistrationType},
        link::rach::MessageSet,
    };
    let database = Database::new();
    let mut core = database.open();
    let payload = pdu(&vec![b'x'; MAX_CMT_USER_OCTETS]);
    assert!(payload.len() > bts136::l3::relay::MAX_DATA_PAYLOAD_OCTETS);
    let (mut incoming, transaction) = completed(&payload, SMS);
    assert_eq!(
        incoming.retire_fenced(&transaction, 100),
        Err(IncomingError::PendingApplication)
    );
    let stored = tsar_submission::commit(
        &mut core,
        &mut incoming,
        &transaction,
        context(&destination("1002"), "full-pdu", 100),
    )
    .unwrap();
    assert_eq!(
        incoming.application_outcome(&transaction).unwrap(),
        Some(ApplicationOutcome::Committed)
    );
    assert_eq!(core.store().state(stored.id).unwrap(), QueueState::Queued);
    let pending = core.store().pending_for(RECIPIENT).unwrap();
    let SmsMessage::Deliver { user_data, .. } =
        bts136::l3::sms::decode_prefix(&pending[0].sms_pdu, Direction::Forward)
            .unwrap()
            .message
    else {
        panic!("expected delivery");
    };
    assert_eq!(user_data, UserData::Ira(vec![b'x'; MAX_CMT_USER_OCTETS]));
    drop(core);
    let mut core = database.open();
    assert_eq!(core.store().pending_for(RECIPIENT).unwrap(), pending);
    core.register(
        &MessageSet {
            identity: RECIPIENT,
            messages: vec![
                AccessMessage::Registration {
                    registration_type: RegistrationType::PowerUp,
                    scm: 21,
                    protocol_version: ProtocolVersion::Pv1,
                }
                .encode()
                .unwrap(),
            ],
        },
        0,
    )
    .unwrap();
    let mut delivery = SmsDelivery::after_restart(
        core,
        DeliveryPolicy {
            response_timeout_ms: 50,
            reuse_guard_ms: 100,
        },
        0,
    )
    .unwrap();
    assert!(delivery.prepare(RECIPIENT, 100).unwrap().is_none());
    assert_eq!(
        delivery.core().store().state(stored.id).unwrap(),
        QueueState::Queued
    );
    assert!(matches!(
        tsar_submission::commit(
            delivery.core_mut(),
            &mut incoming,
            &transaction,
            context(&destination("1002"), "new-key", 100)
        ),
        Err(tsar_submission::Error::NoPendingMessage)
    ));
    assert_eq!(
        delivery
            .core()
            .store()
            .pending_for(RECIPIENT)
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn post_commit_settlement_failure_retries_the_same_durable_key_without_duplicate_sms() {
    let database = Database::new();
    let mut core = database.open();
    let (mut incoming, transaction) = completed(&pdu(b"retry after storage"), SMS);
    incoming.advance(&transaction, 100).unwrap();
    let result = tsar_submission::commit(
        &mut core,
        &mut incoming,
        &transaction,
        context(&destination("1002"), "atomic-gap", 99),
    );
    assert!(matches!(
        result,
        Err(tsar_submission::Error::Ownership(
            IncomingError::ClockRegression
        ))
    ));
    let pending = core.store().pending_for(RECIPIENT).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(incoming.pending_message(&transaction).unwrap().is_some());
    drop(core);
    let mut reopened = database.open();
    let stored = tsar_submission::commit(
        &mut reopened,
        &mut incoming,
        &transaction,
        context(&destination("1002"), "atomic-gap", 100),
    )
    .unwrap();
    assert_eq!(stored.id, pending[0].id);
    assert_eq!(reopened.store().pending_for(RECIPIENT).unwrap().len(), 1);
    assert!(incoming.pending_message(&transaction).unwrap().is_none());
}
#[test]
fn invalid_destination_or_storage_conflict_does_not_settle_pending_application_work() {
    let database = Database::new();
    let mut core = database.open();
    core.submit_complete_sms(
        PEER,
        &destination("1002"),
        &pdu(b"original"),
        "same-key",
        1000,
    )
    .unwrap();
    let (mut incoming, transaction) = completed(&pdu(b"different payload"), SMS);
    assert!(matches!(
        tsar_submission::commit(
            &mut core,
            &mut incoming,
            &transaction,
            context(&destination("9999"), "not-routed", 100)
        ),
        Err(tsar_submission::Error::Messaging(_))
    ));
    assert!(matches!(
        tsar_submission::commit(
            &mut core,
            &mut incoming,
            &transaction,
            context(&destination("1002"), "same-key", 100)
        ),
        Err(tsar_submission::Error::Messaging(_))
    ));
    assert!(incoming.pending_message(&transaction).unwrap().is_some());
    assert_eq!(incoming.application_outcome(&transaction).unwrap(), None);
    assert_eq!(core.store().pending_for(RECIPIENT).unwrap().len(), 1);
}
#[test]
fn unsupported_teleservice_and_unrecognized_sender_cannot_store_or_settle_sms() {
    let database = Database::new();
    let mut core = database.open();
    let (mut incoming, transaction) = completed(
        &pdu(b"not an OATS SMS"),
        HigherLayerProtocol::Segmented(Teleservice::Oats),
    );
    assert!(matches!(
        tsar_submission::commit(
            &mut core,
            &mut incoming,
            &transaction,
            context(&destination("1002"), "wrong-service", 100)
        ),
        Err(tsar_submission::Error::Protocol)
    ));
    assert!(incoming.pending_message(&transaction).unwrap().is_some());
    assert!(
        core.submit_complete_sms(
            MobileIdentity::Min34(99),
            &destination("1002"),
            &pdu(b"unknown"),
            "unknown",
            1000
        )
        .is_err()
    );
    assert!(core.store().pending_for(RECIPIENT).unwrap().is_empty());
}
#[test]
fn invalid_cmt_is_not_stored_and_optional_elements_are_ignored() {
    let database = Database::new();
    let mut core = database.open();
    assert!(
        core.submit_complete_sms(PEER, &destination("1002"), &[0xff], "invalid", 1000)
            .is_err()
    );
    assert!(core.store().pending_for(RECIPIENT).unwrap().is_empty());
    let mut with_suffix = pdu(b"options");
    with_suffix.push(0xff);
    core.submit_complete_sms(PEER, &destination("1002"), &with_suffix, "options", 1000)
        .unwrap();
    assert_eq!(core.store().pending_for(RECIPIENT).unwrap().len(), 1);
}

#[test]
fn bound_destination_rejects_number_reassignment_even_with_a_stale_registry() {
    let database = Database::new();
    let mut core = database.open();
    let bound = core.bind_sms_destination(&destination("1002")).unwrap();
    let mut changed = StateStore::open(&database.0).unwrap();
    save_external(
        &mut changed,
        ProvisionedSubscriber {
            identity: RECIPIENT,
            number: "1003".into(),
        },
    );
    save_external(
        &mut changed,
        ProvisionedSubscriber {
            identity: MobileIdentity::Min34(3),
            number: "1002".into(),
        },
    );
    assert!(matches!(
        core.submit_bound_sms(PEER, &bound, &pdu(b"frozen recipient"), "bound-new", 1000),
        Err(bts136::core::messaging::MessagingError::Store(
            bts136::core::store::StoreError::RecipientMismatch
        ))
    ));
    assert!(core.store().pending_for(RECIPIENT).unwrap().is_empty());
    assert!(
        core.store()
            .pending_for(MobileIdentity::Min34(3))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn exact_bound_retry_survives_reassignment_and_disabled_provisioning() {
    let database = Database::new();
    let mut core = database.open();
    let bound = core.bind_sms_destination(&destination("1002")).unwrap();
    let payload = pdu(b"committed message");
    let original = core
        .submit_bound_sms(PEER, &bound, &payload, "bound-retry", 1000)
        .unwrap();
    let mut changed = StateStore::open(&database.0).unwrap();
    save_external(
        &mut changed,
        ProvisionedSubscriber {
            identity: RECIPIENT,
            number: "1003".into(),
        },
    );
    save_external(
        &mut changed,
        ProvisionedSubscriber {
            identity: MobileIdentity::Min34(3),
            number: "1002".into(),
        },
    );
    disable_external(&mut changed, PEER);
    disable_external(&mut changed, RECIPIENT);
    drop(core);
    let mut core = LocalMessaging::from_store(StateStore::open(&database.0).unwrap()).unwrap();
    let restored = bts136::core::messaging::SmsDestination::try_from_binding(
        bound.recipient().unwrap(),
        bound.number().into(),
    )
    .unwrap();
    let retry = core
        .submit_bound_sms(PEER, &restored, &payload, "bound-retry", 2000)
        .unwrap();
    assert_eq!(retry.id, original.id);
    assert!(retry.delivery.is_none());
    assert!(matches!(
        core.submit_bound_sms(
            PEER,
            &restored,
            &pdu(b"different message"),
            "bound-retry",
            2000
        ),
        Err(bts136::core::messaging::MessagingError::Store(
            bts136::core::store::StoreError::IdempotencyConflict
        ))
    ));
    assert!(
        core.store()
            .pending_for(MobileIdentity::Min34(3))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn bound_tsar_commit_failure_preserves_pending_application_ownership() {
    let database = Database::new();
    let mut core = database.open();
    let bound = core.bind_sms_destination(&destination("1002")).unwrap();
    let (mut incoming, transaction) = completed(&pdu(b"frozen transaction payload"), SMS);
    let mut changed = StateStore::open(&database.0).unwrap();
    save_external(
        &mut changed,
        ProvisionedSubscriber {
            identity: RECIPIENT,
            number: "1003".into(),
        },
    );
    let result = tsar_submission::commit_bound(
        &mut core,
        &mut incoming,
        &transaction,
        tsar_submission::BoundSubmissionContext {
            destination: &bound,
            submission_key: "bound-transaction",
            created_at_unix_ms: 1000,
            monotonic_ms: 100,
        },
    );
    assert!(matches!(
        result,
        Err(tsar_submission::Error::Messaging(
            bts136::core::messaging::MessagingError::Store(
                bts136::core::store::StoreError::RecipientMismatch
            )
        ))
    ));
    assert!(incoming.pending_message(&transaction).unwrap().is_some());
    assert_eq!(incoming.application_outcome(&transaction).unwrap(), None);
}

#[test]
fn restored_destination_cannot_redirect_an_existing_key_or_insert_for_disabled_sender() {
    use bts136::core::{
        messaging::{MessagingError, SmsDestination},
        store::StoreError,
    };
    let database = Database::new();
    let mut core = database.open();
    let bound = core.bind_sms_destination(&destination("1002")).unwrap();
    let payload = pdu(b"immutable application");
    core.submit_bound_sms(PEER, &bound, &payload, "bound-original", 1000)
        .unwrap();
    let mut changed = StateStore::open(&database.0).unwrap();
    let other = MobileIdentity::Min34(3);
    save_external(
        &mut changed,
        ProvisionedSubscriber {
            identity: other,
            number: "1003".into(),
        },
    );
    let wrong = SmsDestination::try_from_binding(other, "1003".into()).unwrap();
    assert!(matches!(
        core.submit_bound_sms(PEER, &wrong, &payload, "bound-original", 1000),
        Err(MessagingError::Store(StoreError::IdempotencyConflict))
    ));
    disable_external(&mut changed, PEER);
    assert!(matches!(
        core.submit_bound_sms(PEER, &bound, &payload, "bound-uncommitted", 1000),
        Err(MessagingError::Store(StoreError::DisabledSubscriber))
    ));
    assert!(core.store().pending_for(other).unwrap().is_empty());
    assert_eq!(core.store().pending_for(RECIPIENT).unwrap().len(), 1);
}

fn save_external(store: &mut StateStore, subscriber: ProvisionedSubscriber) {
    let mut provisioning = store.enabled_subscribers().unwrap();
    provisioning.retain(|current| current.identity != subscriber.identity);
    provisioning.push(subscriber);
    store.reconcile(&provisioning).unwrap();
}
fn disable_external(store: &mut StateStore, identity: MobileIdentity) {
    let mut provisioning = store.enabled_subscribers().unwrap();
    provisioning.retain(|current| current.identity != identity);
    store.reconcile(&provisioning).unwrap();
}

#[test]
fn frozen_submission_key_checks_the_original_sender_and_recipient_numbers() {
    use bts136::core::messaging::{BoundSmsSubmission, MessagingError, SmsDestination};
    use bts136::core::store::StoreError;
    let database = Database::new();
    let mut core = database.open();
    let destination = core.bind_sms_destination(&destination("1002")).unwrap();
    let payload = pdu(b"original parties");
    let stored = core
        .submit_frozen_sms(
            BoundSmsSubmission {
                sender: PEER,
                sender_number: "1001",
                destination: &destination,
            },
            &payload,
            "frozen-parties",
            1000,
        )
        .unwrap();
    assert!(matches!(
        core.submit_frozen_sms(
            BoundSmsSubmission {
                sender: PEER,
                sender_number: "1009",
                destination: &destination
            },
            &payload,
            "frozen-parties",
            2000
        ),
        Err(MessagingError::Store(StoreError::IdempotencyConflict))
    ));
    let wrong = SmsDestination::try_from_binding(RECIPIENT, "1009".into()).unwrap();
    assert!(matches!(
        core.submit_frozen_sms(
            BoundSmsSubmission {
                sender: PEER,
                sender_number: "1001",
                destination: &wrong
            },
            &payload,
            "frozen-parties",
            2000
        ),
        Err(MessagingError::Store(StoreError::IdempotencyConflict))
    ));
    assert_eq!(
        core.store().pending_for(RECIPIENT).unwrap()[0].id,
        stored.id
    );
}
