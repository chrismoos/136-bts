use bts136::{
    core::{
        delivery::{DeliveryError, DeliveryPolicy, SmsDelivery},
        messaging::{LocalMessaging, MessagingError},
        registry::ProvisionedSubscriber,
        relay_attempt::{AttemptError, AttemptState, RelayAttempt},
        relay_exchange::{ExchangeError, RelayExchange, RelayOutcome},
        relay_ids::{IdentifierError, RelayIdentifiers},
        store::{QueueState, SmsId, StateStore, StoreError},
    },
    l3::{
        address::{AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        registration::{AccessMessage, RegistrationType},
        relay::{
            Direction, HigherLayerProtocol, RelayMessage,
            addressing::{AddressedRelay, RelayAddresses},
        },
        sms::{self, Metadata, SmsMessage, UserData},
    },
    link::rach::{MessageSet, MobileIdentity},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self::with_stamp(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
    }
    fn with_stamp(stamp: u128) -> Self {
        loop {
            let sequence = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "136bts-routing-{}-{stamp}-{sequence}.sqlite3",
                std::process::id()
            ));
            match std::fs::File::create_new(&path) {
                Ok(_) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("creating messaging test database failed: {error}"),
            }
        }
    }
}
#[test]
fn parallel_databases_with_identical_clock_timestamps_are_independent() {
    let first = std::thread::spawn(|| Database::with_stamp(0));
    let second = std::thread::spawn(|| Database::with_stamp(0));
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert_ne!(first.0, second.0);
    let first_store = open(&first);
    let second_store = open(&second);
    drop(first_store);
    drop(first);
    assert!(second.0.is_file());
    drop(second_store);
    assert!(open(&second).registry().by_number("1002").is_some());
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn open(database: &Database) -> LocalMessaging {
    let mut store = StateStore::open(&database.0).unwrap();
    store
        .reconcile(&[
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
    LocalMessaging::from_store(store).unwrap()
}
fn submit(number: &str, text: &[u8]) -> Vec<u8> {
    submit_with_manual_request(number, text, false)
}
fn submit_with_manual_request(number: &str, text: &[u8], manual_acknowledgment: bool) -> Vec<u8> {
    let sms = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 42,
            privacy: 0,
            urgency: 1,
            delivery_acknowledgment: true,
            manual_acknowledgment,
            new_message: true,
        },
        user_data: UserData::Ira(text.to_vec()),
    };
    AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: 19,
            protocol: HigherLayerProtocol::Sms,
            payload: sms.encode().unwrap(),
        },
        addresses: RelayAddresses {
            destination: Some(AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd(number.into()),
            }),
            ..RelayAddresses::default()
        },
    }
    .encode(Direction::Reverse)
    .unwrap()
}
fn registration() -> MessageSet {
    MessageSet {
        identity: MobileIdentity::Min34(2),
        messages: vec![
            AccessMessage::Registration {
                registration_type: RegistrationType::PowerUp,
                scm: 21,
                protocol_version: ProtocolVersion::Is136RevA,
            }
            .encode()
            .unwrap(),
        ],
    }
}
fn recover_offer_from_iq(offer: &bts136::core::messaging::DeliveryOffer) -> AddressedRelay {
    recover_relay_from_iq(offer.recipient, &offer.relay(7))
}
fn recover_relay_from_iq(recipient: MobileIdentity, relay: &AddressedRelay) -> AddressedRelay {
    use bts136::{
        link::spach::{Channel, HeaderFlags, SingleMessage, SpachReceiver},
        phy::{
            acquisition::decode_forward_control,
            control::{self, LogicalChannel, SharedFeedback},
            forward::ForwardModulator,
            sync::SyncWord,
            traffic::{self, DATA_BITS, SACCH_BITS},
        },
    };
    let message = SingleMessage {
        channel: Channel::Smsch,
        flags: HeaderFlags::default(),
        identity: recipient,
        message: relay.encode(Direction::Forward).unwrap(),
    };
    let frames = message.encode_fragmented().unwrap();
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for (phase, information) in frames.iter().enumerate() {
        samples.extend(
            transmitter.encode_slot(
                &control::encode_forward(
                    SyncWord::One,
                    phase as u8,
                    &SharedFeedback {
                        leading: [false; 12],
                        trailing: [false; 10],
                    },
                    information,
                    LogicalChannel::Spach,
                    42,
                )
                .unwrap(),
            ),
        );
        for word in [SyncWord::Two, SyncWord::Three] {
            samples.extend(
                transmitter.encode_slot(
                    &traffic::encode_forward(
                        word,
                        &[false; SACCH_BITS],
                        &[false; DATA_BITS],
                        42,
                        0,
                    )
                    .unwrap(),
                ),
            );
        }
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_control(
        &samples,
        transmitter.sample_rate_hz(),
        LogicalChannel::Spach,
        42,
    )
    .unwrap();
    assert_eq!(bursts.len(), frames.len());
    let mut receiver = SpachReceiver::default();
    let mut recovered = Vec::new();
    for burst in bursts {
        if let Some(message) = receiver.push(&burst.fields.block.information).unwrap() {
            recovered.push(message);
        }
    }
    assert_eq!(recovered, [message]);
    bts136::l3::relay::addressing::decode(&recovered[0].message, Direction::Forward)
        .unwrap()
        .relay
}

#[test]
fn offline_submission_survives_restart_and_registration_offers_exact_message() {
    let database = Database::new();
    let bytes = submit("1002", b"queued while offline");
    let id = {
        let mut core = open(&database);
        let submission = core
            .submit_mobile_sms(MobileIdentity::Min34(1), &bytes, "submission-1", 1000)
            .unwrap();
        assert_eq!(
            submission.response,
            RelayMessage::Accept { transaction_id: 19 }
        );
        assert!(submission.delivery.is_none());
        assert_eq!(
            core.store().state(submission.id).unwrap(),
            QueueState::Queued
        );
        submission.id
    };
    let mut core = open(&database);
    let dispatch = core.register(&registration(), 100).unwrap();
    assert_eq!(dispatch.pending.len(), 1);
    assert_eq!(dispatch.pending[0].queued.id, id);
    let relay = recover_offer_from_iq(&dispatch.pending[0]);
    assert_eq!(
        relay.addresses.originating.unwrap().address,
        AddressValue::Tbcd("1001".into())
    );
    let RelayMessage::Data {
        transaction_id,
        payload,
        ..
    } = relay.message
    else {
        panic!("expected R-DATA");
    };
    assert_eq!(transaction_id, 7);
    let SmsMessage::Deliver {
        user_data,
        metadata,
        ..
    } = sms::decode_prefix(&payload, Direction::Forward)
        .unwrap()
        .message
    else {
        panic!("expected DELIVER");
    };
    assert_eq!(user_data, UserData::Ira(b"queued while offline".to_vec()));
    assert_eq!(metadata.message_reference, 42);
    let repeated = core
        .submit_mobile_sms(MobileIdentity::Min34(1), &bytes, "submission-1", 2000)
        .unwrap();
    assert_eq!(repeated.id, id);
    assert!(repeated.delivery.is_some());
    assert_eq!(
        core.store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn unknown_sender_destination_and_conflicting_retries_are_not_accepted() {
    let database = Database::new();
    let mut core = open(&database);
    assert!(matches!(
        core.submit_mobile_sms(MobileIdentity::Min34(3), &submit("1002", b"x"), "key", 0),
        Err(MessagingError::UnknownSender)
    ));
    assert!(matches!(
        core.submit_mobile_sms(MobileIdentity::Min34(1), &submit("9999", b"x"), "key", 0),
        Err(MessagingError::UnknownDestination)
    ));
    assert!(
        core.store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .is_empty()
    );
    core.submit_mobile_sms(
        MobileIdentity::Min34(1),
        &submit("1002", b"first"),
        "key",
        0,
    )
    .unwrap();
    assert!(matches!(
        core.submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"other"),
            "key",
            0
        ),
        Err(MessagingError::Store(_))
    ));
    assert_eq!(
        core.store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
}

fn delivery_ack(number: &str, reference: u16) -> Vec<u8> {
    let acknowledgment = SmsMessage::DeliveryAck {
        message_reference: reference,
        user_data: UserData::Ira(Vec::new()),
    };
    addressed_ack(number, &acknowledgment)
}
fn addressed_ack(number: &str, acknowledgment: &SmsMessage) -> Vec<u8> {
    addressed_ack_payload(number, acknowledgment.encode().unwrap())
}
fn addressed_ack_payload(number: &str, payload: Vec<u8>) -> Vec<u8> {
    AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: 23,
            protocol: HigherLayerProtocol::Sms,
            payload,
        },
        addresses: RelayAddresses {
            destination: Some(AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd(number.into()),
            }),
            ..RelayAddresses::default()
        },
    }
    .encode(Direction::Reverse)
    .unwrap()
}
#[test]
fn delivery_ack_completes_original_and_persists_one_receipt_for_offline_originator() {
    let database = Database::new();
    let mut core = open(&database);
    let original = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"receipt requested"),
            "receipt-test",
            0,
        )
        .unwrap()
        .id;
    let bytes = delivery_ack("1001", 42);
    let dispatch = core
        .accept_delivery_ack(MobileIdentity::Min34(2), &bytes, 1000)
        .unwrap();
    assert_eq!(dispatch.original_id, original);
    assert_eq!(
        dispatch.response,
        RelayMessage::Accept { transaction_id: 23 }
    );
    assert!(dispatch.delivery.is_none());
    assert_eq!(core.store().state(original).unwrap(), QueueState::Delivered);
    let repeated = core
        .accept_delivery_ack(MobileIdentity::Min34(2), &bytes, 2000)
        .unwrap();
    assert_eq!(repeated.receipt_id, dispatch.receipt_id);
    drop(core);
    let mut core = open(&database);
    let set = MessageSet {
        identity: MobileIdentity::Min34(1),
        messages: registration().messages,
    };
    let pending = core.register(&set, 100).unwrap().pending;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].queued.id, dispatch.receipt_id);
    let relay = recover_offer_from_iq(&pending[0]);
    assert_eq!(
        relay.addresses.originating.unwrap().address,
        AddressValue::Tbcd("1002".into())
    );
    let RelayMessage::Data { payload, .. } = relay.message else {
        panic!("expected R-DATA");
    };
    assert_eq!(
        sms::decode_prefix(&payload, Direction::Forward)
            .unwrap()
            .message,
        SmsMessage::DeliveryAck {
            message_reference: 42,
            user_data: UserData::Ira(Vec::new())
        }
    );
}
#[test]
fn mismatched_and_ambiguous_delivery_acknowledgments_do_not_complete_messages() {
    let database = Database::new();
    let mut core = open(&database);
    let original = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"first"),
            "first",
            0,
        )
        .unwrap()
        .id;
    assert!(matches!(
        core.accept_delivery_ack(MobileIdentity::Min34(1), &delivery_ack("1001", 42), 1),
        Err(MessagingError::UnknownReceipt)
    ));
    assert!(matches!(
        core.accept_delivery_ack(MobileIdentity::Min34(2), &delivery_ack("1001", 43), 1),
        Err(MessagingError::UnknownReceipt)
    ));
    assert_eq!(core.store().state(original).unwrap(), QueueState::Queued);
    core.submit_mobile_sms(
        MobileIdentity::Min34(1),
        &submit("1002", b"second"),
        "second",
        0,
    )
    .unwrap();
    assert!(matches!(
        core.accept_delivery_ack(MobileIdentity::Min34(2), &delivery_ack("1001", 42), 1),
        Err(MessagingError::AmbiguousReceipt)
    ));
    assert_eq!(core.store().state(original).unwrap(), QueueState::Queued);
}

#[test]
fn manual_and_delivery_receipts_remain_independent_after_restart_and_forward_encoding() {
    let database = Database::new();
    let mut core = open(&database);
    let original = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit_with_manual_request("1002", b"please confirm", true),
            "both-receipts",
            0,
        )
        .unwrap()
        .id;
    let manual = SmsMessage::ManualAck {
        message_reference: 42,
        user_data: UserData::Ira(b"read".to_vec()),
    };
    let bytes = addressed_ack("1001", &manual);
    assert!(matches!(
        core.accept_delivery_ack(MobileIdentity::Min34(2), &bytes, 1),
        Err(MessagingError::MessageType)
    ));
    let first = core
        .accept_manual_ack(MobileIdentity::Min34(2), &bytes, 1)
        .unwrap();
    assert_eq!(first.original_id, original);
    assert_eq!(first.response, RelayMessage::Accept { transaction_id: 23 });
    assert!(first.delivery.is_none());
    let delivered = core
        .accept_delivery_ack(MobileIdentity::Min34(2), &delivery_ack("1001", 42), 2)
        .unwrap();
    assert_ne!(first.receipt_id, delivered.receipt_id);
    drop(core);
    let mut core = open(&database);
    assert_eq!(
        core.accept_manual_ack(MobileIdentity::Min34(2), &bytes, 3)
            .unwrap()
            .receipt_id,
        first.receipt_id
    );
    let changed = SmsMessage::ManualAck {
        message_reference: 42,
        user_data: UserData::Ira(b"different".to_vec()),
    };
    assert!(matches!(
        core.accept_manual_ack(
            MobileIdentity::Min34(2),
            &addressed_ack("1001", &changed),
            3
        ),
        Err(MessagingError::Store(StoreError::IdempotencyConflict))
    ));
    let set = MessageSet {
        identity: MobileIdentity::Min34(1),
        messages: registration().messages,
    };
    let offers = core.register(&set, 100).unwrap().pending;
    assert_eq!(offers.len(), 2);
    let offer = offers
        .iter()
        .find(|offer| offer.queued.id == first.receipt_id)
        .unwrap();
    let RelayMessage::Data { payload, .. } = recover_offer_from_iq(offer).message else {
        panic!("expected R-DATA");
    };
    assert_eq!(
        sms::decode_prefix(&payload, Direction::Forward)
            .unwrap()
            .message,
        manual
    );
    assert_eq!(core.store().state(original).unwrap(), QueueState::Delivered);
}
#[test]
fn unrequested_manual_receipt_does_not_complete_or_queue_a_response() {
    let database = Database::new();
    let mut core = open(&database);
    let original = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"delivery only"),
            "no-manual",
            0,
        )
        .unwrap()
        .id;
    let manual = SmsMessage::ManualAck {
        message_reference: 42,
        user_data: UserData::Ira(Vec::new()),
    };
    assert!(matches!(
        core.accept_manual_ack(MobileIdentity::Min34(2), &addressed_ack("1001", &manual), 1),
        Err(MessagingError::UnknownReceipt)
    ));
    assert_eq!(core.store().state(original).unwrap(), QueueState::Queued);
    assert!(
        core.store()
            .pending_for(MobileIdentity::Min34(1))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn transport_accepted_message_is_not_reoffered_after_restart_but_receipt_still_routes() {
    let database = Database::new();
    let mut core = open(&database);
    let original = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"accepted before restart"),
            "transport-accepted",
            0,
        )
        .unwrap()
        .id;
    drop(core);
    let mut store = StateStore::open(&database.0).unwrap();
    assert_eq!(
        store
            .record_relay_accept(original, MobileIdentity::Min34(2))
            .unwrap(),
        QueueState::Queued
    );
    drop(store);
    let mut core = open(&database);
    assert!(
        core.register(&registration(), 100)
            .unwrap()
            .pending
            .is_empty()
    );
    let dispatch = core
        .accept_delivery_ack(MobileIdentity::Min34(2), &delivery_ack("1001", 42), 1)
        .unwrap();
    assert_eq!(dispatch.original_id, original);
    assert_eq!(core.store().state(original).unwrap(), QueueState::Delivered);
    let set = MessageSet {
        identity: MobileIdentity::Min34(1),
        messages: registration().messages,
    };
    let pending = core.register(&set, 101).unwrap().pending;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].queued.id, dispatch.receipt_id);
    let RelayMessage::Data { payload, .. } = recover_offer_from_iq(&pending[0]).message else {
        panic!("expected R-DATA");
    };
    assert_eq!(
        sms::decode_prefix(&payload, Direction::Forward)
            .unwrap()
            .message,
        SmsMessage::DeliveryAck {
            message_reference: 42,
            user_data: UserData::Ira(Vec::new())
        }
    );
}

#[test]
fn relay_exchange_correlates_peer_and_identifier_before_persisting_acceptance() {
    let database = Database::new();
    let mut core = open(&database);
    core.register(&registration(), 0).unwrap();
    let submitted = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"correlated"),
            "exchange",
            0,
        )
        .unwrap();
    let mut exchange = RelayExchange::new(submitted.delivery.unwrap(), 37);
    assert!(matches!(
        exchange.relay().message,
        RelayMessage::Data {
            transaction_id: 37,
            ..
        }
    ));
    drop(core);
    let mut store = StateStore::open(&database.0).unwrap();
    let acceptance = RelayMessage::Accept { transaction_id: 37 }
        .encode(Direction::Reverse)
        .unwrap();
    assert!(matches!(
        exchange.receive(&mut store, MobileIdentity::Min34(1), &acceptance),
        Err(ExchangeError::Recipient)
    ));
    let wrong = RelayMessage::Accept { transaction_id: 38 }
        .encode(Direction::Reverse)
        .unwrap();
    assert!(matches!(
        exchange.receive(&mut store, MobileIdentity::Min34(2), &wrong),
        Err(ExchangeError::Transaction)
    ));
    let mut optional = acceptance.clone();
    optional.push(0xff);
    assert!(matches!(
        exchange.receive(&mut store, MobileIdentity::Min34(2), &optional),
        Err(ExchangeError::UnsupportedOptions)
    ));
    assert!(!store.relay_accepted(submitted.id).unwrap());
    assert!(exchange.outcome().is_none());
    let expected = RelayOutcome::Accepted {
        id: submitted.id,
        state: QueueState::Queued,
    };
    assert_eq!(
        exchange
            .receive(&mut store, MobileIdentity::Min34(2), &acceptance)
            .unwrap(),
        expected
    );
    assert_eq!(
        exchange
            .receive(&mut store, MobileIdentity::Min34(2), &acceptance)
            .unwrap(),
        expected
    );
    assert!(store.relay_accepted(submitted.id).unwrap());
    let rejection = RelayMessage::Reject {
        transaction_id: 37,
        cause: 22,
    }
    .encode(Direction::Reverse)
    .unwrap();
    assert!(matches!(
        exchange.receive(&mut store, MobileIdentity::Min34(2), &rejection),
        Err(ExchangeError::ConflictingResponse)
    ));
}
#[test]
fn relay_rejection_preserves_queue_and_identical_repeats_are_idempotent() {
    let database = Database::new();
    let mut core = open(&database);
    core.register(&registration(), 0).unwrap();
    let submitted = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"retry later"),
            "rejection",
            0,
        )
        .unwrap();
    let mut exchange = RelayExchange::new(submitted.delivery.unwrap(), 37);
    drop(core);
    let mut store = StateStore::open(&database.0).unwrap();
    let rejection = RelayMessage::Reject {
        transaction_id: 37,
        cause: 22,
    }
    .encode(Direction::Reverse)
    .unwrap();
    let expected = RelayOutcome::Rejected {
        id: submitted.id,
        cause: 22,
    };
    assert_eq!(
        exchange
            .receive(&mut store, MobileIdentity::Min34(2), &rejection)
            .unwrap(),
        expected
    );
    assert_eq!(
        exchange
            .receive(&mut store, MobileIdentity::Min34(2), &rejection)
            .unwrap(),
        expected
    );
    let changed = RelayMessage::Reject {
        transaction_id: 37,
        cause: 111,
    }
    .encode(Direction::Reverse)
    .unwrap();
    assert!(matches!(
        exchange.receive(&mut store, MobileIdentity::Min34(2), &changed),
        Err(ExchangeError::ConflictingResponse)
    ));
    assert!(!store.relay_accepted(submitted.id).unwrap());
    assert_eq!(
        store.pending_for(MobileIdentity::Min34(2)).unwrap().len(),
        1
    );
    assert_eq!(store.state(submitted.id).unwrap(), QueueState::Queued);
}

#[test]
fn reserved_identifier_binds_correct_peer_and_remains_guarded_after_exchange_completion() {
    let database = Database::new();
    let mut core = open(&database);
    core.register(&registration(), 0).unwrap();
    let submitted = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"reserved"),
            "reserved",
            0,
        )
        .unwrap();
    let offer = submitted.delivery.unwrap();
    let mut wrong = RelayIdentifiers::try_new(MobileIdentity::Min34(1), 100).unwrap();
    let wrong_id = wrong.reserve(0).unwrap();
    assert!(matches!(
        RelayExchange::from_reserved(offer.clone(), &wrong_id),
        Err(ExchangeError::Recipient)
    ));
    let mut bank = RelayIdentifiers::try_new(MobileIdentity::Min34(2), 100).unwrap();
    let identifier = bank.reserve(0).unwrap();
    let mut exchange = RelayExchange::from_reserved(offer, &identifier).unwrap();
    let RelayMessage::Data { transaction_id, .. } = exchange.relay().message else {
        panic!("expected R-DATA");
    };
    assert_eq!(transaction_id, identifier.value());
    drop(core);
    let mut store = StateStore::open(&database.0).unwrap();
    let bytes = RelayMessage::Accept { transaction_id }
        .encode(Direction::Reverse)
        .unwrap();
    assert!(matches!(
        exchange
            .receive(&mut store, MobileIdentity::Min34(2), &bytes)
            .unwrap(),
        RelayOutcome::Accepted { .. }
    ));
    drop(exchange);
    bank.retire(&identifier, 1).unwrap();
    assert_ne!(bank.reserve(1).unwrap().value(), identifier.value());
}

struct AttemptFixture {
    _database: Database,
    store: StateStore,
    bank: RelayIdentifiers,
    attempt: RelayAttempt,
    id: SmsId,
}
impl AttemptFixture {
    fn new() -> Self {
        let database = Database::new();
        let mut core = open(&database);
        core.register(&registration(), 0).unwrap();
        let submission = core
            .submit_mobile_sms(
                MobileIdentity::Min34(1),
                &submit("1002", b"managed"),
                "managed",
                0,
            )
            .unwrap();
        let mut bank = RelayIdentifiers::try_new(MobileIdentity::Min34(2), 100).unwrap();
        let attempt = RelayAttempt::try_new(submission.delivery.unwrap(), &mut bank, 10).unwrap();
        drop(core);
        let store = StateStore::open(&database.0).unwrap();
        Self {
            _database: database,
            store,
            bank,
            attempt,
            id: submission.id,
        }
    }
    fn response(&self) -> Vec<u8> {
        let RelayMessage::Data { transaction_id, .. } = self.attempt.relay().message else {
            panic!("expected R-DATA");
        };
        RelayMessage::Accept { transaction_id }
            .encode(Direction::Reverse)
            .unwrap()
    }
    fn fill_remaining_ids(&mut self, monotonic_ms: u64) {
        for _ in 0..usize::from(u8::MAX) {
            self.bank.reserve(monotonic_ms).unwrap();
        }
    }
}
#[test]
fn managed_acceptance_requires_transmission_and_retires_id_automatically() {
    let mut fixture = AttemptFixture::new();
    let bytes = fixture.response();
    assert!(matches!(
        fixture.attempt.receive(
            &mut fixture.store,
            &mut fixture.bank,
            MobileIdentity::Min34(2),
            &bytes,
            10
        ),
        Err(AttemptError::NotTransmitted)
    ));
    assert!(!fixture.store.relay_accepted(fixture.id).unwrap());
    fixture.fill_remaining_ids(10);
    fixture.attempt.transmitted(10, 50).unwrap();
    let expected = RelayOutcome::Accepted {
        id: fixture.id,
        state: QueueState::Queued,
    };
    assert_eq!(
        fixture
            .attempt
            .receive(
                &mut fixture.store,
                &mut fixture.bank,
                MobileIdentity::Min34(2),
                &bytes,
                11
            )
            .unwrap(),
        expected
    );
    assert_eq!(fixture.attempt.state(), AttemptState::Completed(expected));
    assert!(fixture.store.relay_accepted(fixture.id).unwrap());
    assert!(matches!(
        fixture.bank.reserve(110),
        Err(IdentifierError::Exhausted)
    ));
    assert!(matches!(
        fixture.attempt.receive(
            &mut fixture.store,
            &mut fixture.bank,
            MobileIdentity::Min34(2),
            &bytes,
            111
        ),
        Err(AttemptError::Closed)
    ));
    assert_eq!(fixture.bank.reserve(111).unwrap().value(), 0);
}
#[test]
fn managed_deadline_expires_at_boundary_without_accepting_late_response() {
    let mut fixture = AttemptFixture::new();
    fixture.attempt.transmitted(10, 50).unwrap();
    assert_eq!(
        fixture.attempt.advance(&mut fixture.bank, 59).unwrap(),
        AttemptState::AwaitingResponse {
            deadline_monotonic_ms: 60
        }
    );
    let bytes = fixture.response();
    assert!(matches!(
        fixture.attempt.receive(
            &mut fixture.store,
            &mut fixture.bank,
            MobileIdentity::Min34(2),
            &bytes,
            60
        ),
        Err(AttemptError::Closed)
    ));
    assert_eq!(fixture.attempt.state(), AttemptState::Expired);
    assert!(!fixture.store.relay_accepted(fixture.id).unwrap());
    assert_eq!(
        fixture
            .store
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
    fixture.fill_remaining_ids(60);
    assert!(matches!(
        fixture.bank.reserve(159),
        Err(IdentifierError::Exhausted)
    ));
    assert_eq!(fixture.bank.reserve(160).unwrap().value(), 0);
}
#[test]
fn managed_rejection_and_cancellation_preserve_queue_and_guard_identifiers() {
    let mut fixture = AttemptFixture::new();
    fixture.attempt.transmitted(10, 50).unwrap();
    let rejection = RelayMessage::Reject {
        transaction_id: 0,
        cause: 22,
    }
    .encode(Direction::Reverse)
    .unwrap();
    assert_eq!(
        fixture
            .attempt
            .receive(
                &mut fixture.store,
                &mut fixture.bank,
                MobileIdentity::Min34(2),
                &rejection,
                11
            )
            .unwrap(),
        RelayOutcome::Rejected {
            id: fixture.id,
            cause: 22
        }
    );
    assert!(!fixture.store.relay_accepted(fixture.id).unwrap());
    assert_eq!(
        fixture
            .store
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
    fixture.fill_remaining_ids(11);
    assert!(matches!(
        fixture.bank.reserve(110),
        Err(IdentifierError::Exhausted)
    ));
    assert_eq!(fixture.bank.reserve(111).unwrap().value(), 0);
    let mut cancelled = AttemptFixture::new();
    let mut foreign = RelayIdentifiers::try_new(MobileIdentity::Min34(2), 100).unwrap();
    foreign.reserve(10).unwrap();
    assert!(matches!(
        cancelled.attempt.cancel(&mut foreign, 10),
        Err(AttemptError::Identifier(IdentifierError::StaleReservation))
    ));
    assert_eq!(cancelled.attempt.state(), AttemptState::Prepared);
    cancelled.attempt.cancel(&mut cancelled.bank, 10).unwrap();
    assert_eq!(cancelled.attempt.state(), AttemptState::Cancelled);
    assert!(!cancelled.store.relay_accepted(cancelled.id).unwrap());
    assert!(matches!(
        cancelled.attempt.cancel(&mut cancelled.bank, 10),
        Err(AttemptError::Closed)
    ));
}
#[test]
fn managed_clock_timeout_and_store_errors_leave_attempt_open_for_recovery() {
    let mut fixture = AttemptFixture::new();
    assert!(matches!(
        fixture.attempt.transmitted(9, 50),
        Err(AttemptError::ClockRegression)
    ));
    assert!(matches!(
        fixture.attempt.transmitted(10, 0),
        Err(AttemptError::TimeoutDuration)
    ));
    assert!(matches!(
        fixture.attempt.transmitted(u64::MAX, 50),
        Err(AttemptError::TimeOverflow)
    ));
    assert_eq!(fixture.attempt.state(), AttemptState::Prepared);
    fixture.attempt.transmitted(10, 50).unwrap();
    assert!(matches!(
        fixture.attempt.transmitted(10, 50),
        Err(AttemptError::AlreadyTransmitted)
    ));
    fixture.store.mark_failed(fixture.id).unwrap();
    let bytes = fixture.response();
    assert!(matches!(
        fixture.attempt.receive(
            &mut fixture.store,
            &mut fixture.bank,
            MobileIdentity::Min34(2),
            &bytes,
            11
        ),
        Err(AttemptError::Exchange(ExchangeError::Store(
            StoreError::InvalidTransition
        )))
    ));
    assert_eq!(
        fixture.attempt.state(),
        AttemptState::AwaitingResponse {
            deadline_monotonic_ms: 60
        }
    );
    fixture.attempt.cancel(&mut fixture.bank, 11).unwrap();
    assert_eq!(fixture.attempt.state(), AttemptState::Cancelled);
}

fn delivery_policy() -> DeliveryPolicy {
    DeliveryPolicy {
        response_timeout_ms: 50,
        reuse_guard_ms: 100,
    }
}
fn accept_prepared(prepared: &bts136::core::delivery::PreparedDelivery) -> Vec<u8> {
    let RelayMessage::Data { transaction_id, .. } = prepared.relay().message else {
        panic!("expected R-DATA");
    };
    RelayMessage::Accept { transaction_id }
        .encode(Direction::Reverse)
        .unwrap()
}
#[test]
fn dispatcher_drives_registered_queue_through_iq_and_persists_acceptance() {
    let database = Database::new();
    let mut delivery = SmsDelivery::after_restart(open(&database), delivery_policy(), 0).unwrap();
    let submitted = delivery
        .core_mut()
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"owned delivery"),
            "owned",
            0,
        )
        .unwrap()
        .id;
    assert!(
        delivery
            .prepare(MobileIdentity::Min34(2), 0)
            .unwrap()
            .is_none()
    );
    delivery.core_mut().register(&registration(), 0).unwrap();
    assert!(matches!(
        delivery.prepare(MobileIdentity::Min34(2), 99),
        Err(DeliveryError::Attempt(AttemptError::Identifier(
            IdentifierError::Exhausted
        )))
    ));
    let prepared = delivery
        .prepare(MobileIdentity::Min34(2), 100)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.id(), submitted);
    assert_eq!(
        recover_relay_from_iq(prepared.recipient(), prepared.relay()),
        *prepared.relay()
    );
    assert!(matches!(
        delivery.prepare(MobileIdentity::Min34(2), 100),
        Err(DeliveryError::Busy)
    ));
    assert!(matches!(
        delivery.receive(MobileIdentity::Min34(2), &accept_prepared(&prepared), 100),
        Err(DeliveryError::Attempt(AttemptError::NotTransmitted))
    ));
    delivery.transmitted(&prepared, 100).unwrap();
    assert_eq!(
        delivery
            .receive(MobileIdentity::Min34(2), &accept_prepared(&prepared), 101)
            .unwrap(),
        RelayOutcome::Accepted {
            id: submitted,
            state: QueueState::Queued
        }
    );
    assert!(delivery.core().store().relay_accepted(submitted).unwrap());
    assert!(
        delivery
            .prepare(MobileIdentity::Min34(2), 101)
            .unwrap()
            .is_none()
    );
    drop(delivery);
    let mut restarted =
        SmsDelivery::after_restart(open(&database), delivery_policy(), 200).unwrap();
    restarted.core_mut().register(&registration(), 0).unwrap();
    assert!(
        restarted
            .prepare(MobileIdentity::Min34(2), 300)
            .unwrap()
            .is_none()
    );
}
#[test]
fn dispatcher_timeout_allows_retry_and_rejects_stale_transmission_callback() {
    let database = Database::new();
    let mut core = open(&database);
    core.register(&registration(), 0).unwrap();
    let id = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"retry"),
            "retry-owned",
            0,
        )
        .unwrap()
        .id;
    let mut delivery = SmsDelivery::after_restart(core, delivery_policy(), 0).unwrap();
    let first = delivery
        .prepare(MobileIdentity::Min34(2), 100)
        .unwrap()
        .unwrap();
    delivery.transmitted(&first, 100).unwrap();
    assert!(
        delivery
            .advance(MobileIdentity::Min34(2), 149)
            .unwrap()
            .is_none()
    );
    let ended = delivery
        .advance(MobileIdentity::Min34(2), 150)
        .unwrap()
        .unwrap();
    assert_eq!(ended.id, id);
    assert_eq!(ended.state, AttemptState::Expired);
    let second = delivery
        .prepare(MobileIdentity::Min34(2), 150)
        .unwrap()
        .unwrap();
    assert_eq!(second.id(), id);
    assert_ne!(first.relay().message, second.relay().message);
    assert!(matches!(
        delivery.transmitted(&first, 150),
        Err(DeliveryError::StaleTransmission)
    ));
    delivery.transmitted(&second, 150).unwrap();
    assert!(matches!(
        delivery.receive(MobileIdentity::Min34(2), &accept_prepared(&first), 151),
        Err(DeliveryError::Attempt(AttemptError::Exchange(
            ExchangeError::Transaction
        )))
    ));
    assert!(!delivery.core().store().relay_accepted(id).unwrap());
    delivery
        .receive(MobileIdentity::Min34(2), &accept_prepared(&second), 151)
        .unwrap();
}
#[test]
fn dispatcher_cancels_offline_attempts_and_restart_rejects_prior_callback() {
    let database = Database::new();
    let mut core = open(&database);
    core.register(&registration(), 0).unwrap();
    let id = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"interrupted"),
            "interrupted",
            0,
        )
        .unwrap()
        .id;
    let mut delivery = SmsDelivery::after_restart(core, delivery_policy(), 0).unwrap();
    let old = delivery
        .prepare(MobileIdentity::Min34(2), 100)
        .unwrap()
        .unwrap();
    drop(delivery);
    let mut delivery = SmsDelivery::after_restart(open(&database), delivery_policy(), 200).unwrap();
    assert!(
        delivery
            .prepare(MobileIdentity::Min34(2), 200)
            .unwrap()
            .is_none()
    );
    delivery.core_mut().register(&registration(), 0).unwrap();
    assert!(matches!(
        delivery.prepare(MobileIdentity::Min34(2), 299),
        Err(DeliveryError::Attempt(AttemptError::Identifier(
            IdentifierError::Exhausted
        )))
    ));
    let current = delivery
        .prepare(MobileIdentity::Min34(2), 300)
        .unwrap()
        .unwrap();
    assert_eq!(current.id(), id);
    assert!(matches!(
        delivery.transmitted(&old, 300),
        Err(DeliveryError::StaleTransmission)
    ));
    let mut down = registration();
    down.messages = vec![
        AccessMessage::Registration {
            registration_type: RegistrationType::PowerDown,
            scm: 0,
            protocol_version: ProtocolVersion::Is136RevA,
        }
        .encode()
        .unwrap(),
    ];
    delivery.core_mut().register(&down, 1).unwrap();
    assert_eq!(
        delivery
            .advance(MobileIdentity::Min34(2), 300)
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Cancelled
    );
    assert!(matches!(
        delivery.transmitted(&current, 300),
        Err(DeliveryError::StaleTransmission)
    ));
    assert_eq!(
        delivery
            .core()
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()[0]
            .id,
        id
    );
    assert!(matches!(
        delivery.prepare(MobileIdentity::Min34(2), 299),
        Err(DeliveryError::ClockRegression)
    ));
}

#[test]
fn dispatcher_offline_completion_and_rejection_preserve_message_for_later_retry() {
    let database = Database::new();
    let mut core = open(&database);
    core.register(&registration(), 0).unwrap();
    let id = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"not lost"),
            "not-lost",
            0,
        )
        .unwrap()
        .id;
    let mut delivery = SmsDelivery::after_restart(core, delivery_policy(), 0).unwrap();
    let prepared = delivery
        .prepare(MobileIdentity::Min34(2), 100)
        .unwrap()
        .unwrap();
    let mut down = registration();
    down.messages = vec![
        AccessMessage::Registration {
            registration_type: RegistrationType::PowerDown,
            scm: 0,
            protocol_version: ProtocolVersion::Is136RevA,
        }
        .encode()
        .unwrap(),
    ];
    delivery.core_mut().register(&down, 1).unwrap();
    assert!(matches!(
        delivery.transmitted(&prepared, 100),
        Err(DeliveryError::RecipientOffline)
    ));
    assert!(
        delivery
            .prepare(MobileIdentity::Min34(2), 100)
            .unwrap()
            .is_none()
    );
    delivery.core_mut().register(&registration(), 2).unwrap();
    let retry = delivery
        .prepare(MobileIdentity::Min34(2), 101)
        .unwrap()
        .unwrap();
    delivery.transmitted(&retry, 101).unwrap();
    let RelayMessage::Data { transaction_id, .. } = retry.relay().message else {
        panic!("expected R-DATA");
    };
    let rejection = RelayMessage::Reject {
        transaction_id,
        cause: 22,
    }
    .encode(Direction::Reverse)
    .unwrap();
    assert_eq!(
        delivery
            .receive(MobileIdentity::Min34(2), &rejection, 102)
            .unwrap(),
        RelayOutcome::Rejected { id, cause: 22 }
    );
    assert!(!delivery.core().store().relay_accepted(id).unwrap());
    let next = delivery
        .prepare(MobileIdentity::Min34(2), 102)
        .unwrap()
        .unwrap();
    assert_eq!(next.id(), id);
    assert_ne!(retry.relay().message, next.relay().message);
    assert_eq!(
        delivery
            .cancel(MobileIdentity::Min34(2), 102)
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Cancelled
    );
}
#[test]
fn dispatcher_rejects_invalid_policy_and_unknown_recipients() {
    let database = Database::new();
    for policy in [
        DeliveryPolicy {
            response_timeout_ms: 0,
            reuse_guard_ms: 100,
        },
        DeliveryPolicy {
            response_timeout_ms: 50,
            reuse_guard_ms: 0,
        },
    ] {
        assert!(matches!(
            SmsDelivery::after_restart(open(&database), policy, 0),
            Err(DeliveryError::Policy)
        ));
    }
    let mut delivery = SmsDelivery::after_restart(open(&database), delivery_policy(), 0).unwrap();
    assert!(matches!(
        delivery.prepare(MobileIdentity::Min34(3), 0),
        Err(DeliveryError::UnknownRecipient)
    ));
    assert!(matches!(
        delivery.advance(MobileIdentity::Min34(3), 0),
        Err(DeliveryError::UnknownRecipient)
    ));
}

#[test]
fn historical_final_burst_preserves_observation_order_and_response_deadline() {
    let mut fixture = AttemptFixture::new();
    fixture.attempt.advance(&mut fixture.bank, 300).unwrap();
    fixture.attempt.transmitted_at(20, 300, 1000).unwrap();
    assert_eq!(
        fixture.attempt.state(),
        AttemptState::AwaitingResponse {
            deadline_monotonic_ms: 1020
        }
    );
    assert_eq!(
        fixture.attempt.advance(&mut fixture.bank, 1019).unwrap(),
        AttemptState::AwaitingResponse {
            deadline_monotonic_ms: 1020
        }
    );
    assert_eq!(
        fixture.attempt.advance(&mut fixture.bank, 1020).unwrap(),
        AttemptState::Expired
    );
    assert!(!fixture.store.relay_accepted(fixture.id).unwrap());
}

#[test]
fn invalid_completion_receipts_leave_the_prepared_attempt_unchanged() {
    let mut fixture = AttemptFixture::new();
    fixture.attempt.advance(&mut fixture.bank, 30).unwrap();
    for (completed, observed) in [(9, 30), (31, 30)] {
        assert!(matches!(
            fixture.attempt.transmitted_at(completed, observed, 100),
            Err(AttemptError::ReceiptTime)
        ));
        assert_eq!(fixture.attempt.state(), AttemptState::Prepared);
    }
    assert!(matches!(
        fixture.attempt.transmitted_at(20, 29, 100),
        Err(AttemptError::ClockRegression)
    ));
    assert!(matches!(
        fixture.attempt.transmitted_at(u64::MAX - 1, u64::MAX, 2),
        Err(AttemptError::TimeOverflow)
    ));
    assert_eq!(fixture.attempt.state(), AttemptState::Prepared);
    fixture.attempt.transmitted_at(20, 30, 100).unwrap();
    assert_eq!(
        fixture.attempt.state(),
        AttemptState::AwaitingResponse {
            deadline_monotonic_ms: 120
        }
    );
}

#[test]
fn delivery_expires_an_overdue_completion_before_accepting_a_late_response() {
    let database = Database::new();
    let mut delivery = SmsDelivery::after_restart(open(&database), delivery_policy(), 0).unwrap();
    delivery.core_mut().register(&registration(), 0).unwrap();
    let id = delivery
        .core_mut()
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit("1002", b"delayed callback"),
            "completion-time",
            0,
        )
        .unwrap()
        .id;
    let recipient = MobileIdentity::Min34(2);
    let prepared = delivery.prepare(recipient, 100).unwrap().unwrap();
    delivery.advance(recipient, 130).unwrap();
    delivery.transmitted_at(&prepared, 101, 200).unwrap();
    assert!(matches!(
        delivery.receive(recipient, &accept_prepared(&prepared), 200),
        Err(DeliveryError::Attempt(AttemptError::Closed))
    ));
    assert!(!delivery.core().store().relay_accepted(id).unwrap());
    assert_eq!(
        delivery.advance(recipient, 200).unwrap().unwrap().state,
        AttemptState::Expired
    );
    assert_eq!(
        delivery
            .core()
            .store()
            .pending_for(recipient)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn manual_ack_with_a_response_code_is_accepted() {
    let database = Database::new();
    let mut core = open(&database);
    let original = core
        .submit_mobile_sms(
            MobileIdentity::Min34(1),
            &submit_with_manual_request("1002", b"please confirm", true),
            "response-code",
            0,
        )
        .unwrap()
        .id;
    let mut payload = SmsMessage::ManualAck {
        message_reference: 42,
        user_data: UserData::Ira(b"ok".to_vec()),
    }
    .encode()
    .unwrap();
    payload.extend([0x10, 0x70]);
    let accepted = core
        .accept_manual_ack(
            MobileIdentity::Min34(2),
            &addressed_ack_payload("1001", payload),
            1,
        )
        .unwrap();
    assert_eq!(accepted.original_id, original);
}
