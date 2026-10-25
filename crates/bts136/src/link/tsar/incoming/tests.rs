use super::*;
use crate::l3::{
    relay::Teleservice,
    tsar::{AcknowledgmentWindow, OptionalServices, Segment, Status},
};
const PEER: MobileIdentity = MobileIdentity::Min34(1);
const SMS: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
const LIMITS: IncomingLimits = IncomingLimits {
    maximum_message_octets: 10,
    maximum_reserved_payload_octets: 160,
};
fn bank() -> IncomingTransactions {
    IncomingTransactions::try_new(PEER, SMS, LIMITS, 100, 0).unwrap()
}
fn begin_packet(id: u8) -> DecodedSegment {
    DecodedSegment {
        segment: Segment {
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
        },
        unparsed_octets: vec![],
    }
}
fn end_packet(id: u8) -> DecodedSegment {
    DecodedSegment {
        segment: Segment {
            transaction_id: id,
            sequence_number: 1,
            segment_data: b"cd".to_vec(),
            kind: SegmentKind::End {
                message_length: None,
                crc: None,
            },
        },
        unparsed_octets: vec![],
    }
}
fn begin(bank: &mut IncomingTransactions, id: u8, now: u64) -> IncomingTransaction {
    bank.begin(PEER, SMS, &begin_packet(id), ProtocolVersion::Pv1, now)
        .unwrap()
        .transaction
}
fn complete(
    bank: &mut IncomingTransactions,
    transaction: &IncomingTransaction,
    now: u64,
) -> ApplicationMessage {
    let reception = bank
        .receive(
            transaction,
            PEER,
            SMS,
            &end_packet(transaction.transaction_id()),
            now,
        )
        .unwrap();
    assert_eq!(reception.response.unwrap().status, Status::Received);
    reception.message.unwrap()
}
#[test]
fn all_eight_wire_ids_have_distinct_owned_receivers_and_cannot_be_overwritten() {
    let mut bank = bank();
    let transactions: Vec<_> = (0..TRANSACTION_COUNT as u8)
        .map(|id| begin(&mut bank, id, 0))
        .collect();
    assert_eq!(
        bank.reserved_payload_octets(),
        LIMITS.maximum_reserved_payload_octets
    );
    for transaction in &transactions {
        assert_eq!(
            bank.receiver(transaction).unwrap().state(),
            ReceiverState::Receiving
        );
        assert!(matches!(
            bank.begin(
                PEER,
                SMS,
                &begin_packet(transaction.transaction_id()),
                ProtocolVersion::Pv1,
                0
            ),
            Err(IncomingError::Occupied)
        ));
    }
    drop(transactions);
    assert!(matches!(
        bank.begin(PEER, SMS, &begin_packet(0), ProtocolVersion::Pv1, 0),
        Err(IncomingError::Occupied)
    ));
}
#[test]
fn completion_stays_pending_until_exact_application_message_is_settled() {
    let mut bank = bank();
    let transaction = begin(&mut bank, 3, 0);
    let message = complete(&mut bank, &transaction, 1);
    assert_eq!(message.bytes(), b"abcd");
    assert_eq!(
        bank.pending_message(&transaction).unwrap().unwrap().bytes(),
        b"abcd"
    );
    assert_eq!(
        bank.retire_fenced(&transaction, 1),
        Err(IncomingError::PendingApplication)
    );
    let duplicate = bank
        .receive(&transaction, PEER, SMS, &end_packet(3), 2)
        .unwrap();
    assert!(duplicate.message.is_none());
    assert_eq!(duplicate.response.unwrap().status, Status::Received);
    assert!(matches!(
        bank.begin(PEER, SMS, &begin_packet(3), ProtocolVersion::Pv1, 2),
        Err(IncomingError::Occupied)
    ));
    let retry = bank.pending_message(&transaction).unwrap().unwrap().clone();
    bank.settle_application(&transaction, &retry, ApplicationOutcome::Committed, 3)
        .unwrap();
    assert!(bank.pending_message(&transaction).unwrap().is_none());
    assert_eq!(
        bank.application_outcome(&transaction).unwrap(),
        Some(ApplicationOutcome::Committed)
    );
    assert_eq!(
        bank.settle_application(&transaction, &message, ApplicationOutcome::Committed, 3),
        Err(IncomingError::StaleApplication)
    );
    bank.retire_fenced(&transaction, 3).unwrap();
    assert_eq!(bank.reserved_payload_octets(), 0);
    assert!(matches!(
        bank.begin(PEER, SMS, &begin_packet(3), ProtocolVersion::Pv1, 102),
        Err(IncomingError::Guarded)
    ));
    let replacement = begin(&mut bank, 3, 103);
    assert_eq!(
        bank.advance(&transaction, 103),
        Err(IncomingError::StaleTransaction)
    );
    assert_eq!(
        bank.settle_application(&replacement, &message, ApplicationOutcome::Committed, 103),
        Err(IncomingError::StaleApplication)
    );
    let replacement_message = complete(&mut bank, &replacement, 104);
    assert_eq!(replacement_message.bytes(), message.bytes());
    assert_eq!(
        bank.settle_application(&replacement, &message, ApplicationOutcome::Committed, 104),
        Err(IncomingError::StaleApplication)
    );
    assert_eq!(
        bank.pending_message(&replacement).unwrap().unwrap().bytes(),
        b"abcd"
    );
    bank.settle_application(
        &replacement,
        &replacement_message,
        ApplicationOutcome::Committed,
        104,
    )
    .unwrap();
}
#[test]
fn payload_capacity_is_reserved_before_admission_and_released_only_on_retirement() {
    let mut bank = IncomingTransactions::try_new(
        PEER,
        SMS,
        IncomingLimits {
            maximum_reserved_payload_octets: 20,
            ..LIMITS
        },
        100,
        0,
    )
    .unwrap();
    let first = begin(&mut bank, 0, 0);
    assert!(matches!(
        bank.begin(PEER, SMS, &begin_packet(1), ProtocolVersion::Pv1, 0),
        Err(IncomingError::Capacity)
    ));
    assert_eq!(
        bank.retire_fenced(&first, 0),
        Err(IncomingError::StillActive)
    );
    bank.cancel(&first, 1).unwrap();
    assert_eq!(bank.reserved_payload_octets(), 20);
    bank.retire_fenced(&first, 1).unwrap();
    let second = begin(&mut bank, 1, 1);
    let message = complete(&mut bank, &second, 2);
    bank.settle_application(&second, &message, ApplicationOutcome::Rejected, 2)
        .unwrap();
    assert_eq!(
        bank.application_outcome(&second).unwrap(),
        Some(ApplicationOutcome::Rejected)
    );
    bank.retire_fenced(&second, 2).unwrap();
    assert_eq!(bank.reserved_payload_octets(), 0);
}
#[test]
fn restart_guards_ids_and_stale_local_generations_do_not_cross_banks() {
    let mut old = bank();
    let previous = begin(&mut old, 0, 0);
    let mut restarted = IncomingTransactions::after_restart(PEER, SMS, LIMITS, 100, 500).unwrap();
    assert!(matches!(
        restarted.begin(PEER, SMS, &begin_packet(0), ProtocolVersion::Pv1, 599),
        Err(IncomingError::Guarded)
    ));
    let current = begin(&mut restarted, 0, 600);
    assert_eq!(
        restarted.cancel(&previous, 600),
        Err(IncomingError::StaleTransaction)
    );
    assert_eq!(restarted.receiver(&current).unwrap().stored_octets(), 2);
}
#[test]
fn malformed_begin_wrong_scope_and_bad_clock_do_not_allocate_or_replace_owners() {
    let mut bank = bank();
    assert!(matches!(
        bank.begin(
            MobileIdentity::Min34(2),
            SMS,
            &begin_packet(0),
            ProtocolVersion::Pv1,
            0
        ),
        Err(IncomingError::Identity)
    ));
    assert!(matches!(
        bank.begin(
            PEER,
            HigherLayerProtocol::Sms,
            &begin_packet(0),
            ProtocolVersion::Pv1,
            0
        ),
        Err(IncomingError::Service)
    ));
    assert!(matches!(
        bank.begin(PEER, SMS, &end_packet(0), ProtocolVersion::Pv1, 0),
        Err(IncomingError::Receiver(ReceiveError::BeginRequired))
    ));
    let mut bad = begin_packet(0);
    bad.unparsed_octets.push(0xff);
    assert!(matches!(
        bank.begin(PEER, SMS, &bad, ProtocolVersion::Pv1, 0),
        Err(IncomingError::Receiver(ReceiveError::UnparsedSegment))
    ));
    assert!(matches!(
        bank.begin(PEER, SMS, &begin_packet(8), ProtocolVersion::Pv1, 0),
        Err(IncomingError::TransactionId)
    ));
    assert_eq!(bank.reserved_payload_octets(), 0);
    let transaction = begin(&mut bank, 0, 10);
    assert_eq!(
        bank.advance(&transaction, 9),
        Err(IncomingError::ClockRegression)
    );
    assert!(matches!(
        bank.receive(&transaction, PEER, SMS, &end_packet(1), 10),
        Err(IncomingError::Receiver(ReceiveError::TransactionId))
    ));
    assert_eq!(bank.receiver(&transaction).unwrap().stored_octets(), 2);
}
#[test]
fn timeout_response_and_prohibited_expiry_keep_the_correct_owned_stream() {
    let mut bank = bank();
    let enabled = begin(&mut bank, 0, 0);
    let mut prohibited = begin_packet(1);
    if let SegmentKind::Begin {
        ref mut services, ..
    } = prohibited.segment.kind
    {
        *services = None;
    }
    let prohibited = bank
        .begin(PEER, SMS, &prohibited, ProtocolVersion::Pv1, 0)
        .unwrap()
        .transaction;
    assert_eq!(
        bank.advance(&enabled, 300_000).unwrap().unwrap().status,
        Status::InterSegmentTimeout
    );
    assert_eq!(bank.advance(&prohibited, 300_000).unwrap(), None);
    assert_eq!(
        bank.receiver(&prohibited).unwrap().state(),
        ReceiverState::Expired
    );
    assert!(matches!(
        bank.receive(&prohibited, PEER, SMS, &end_packet(1), 300_001),
        Err(IncomingError::Receiver(ReceiveError::Closed))
    ));
    let message = complete(&mut bank, &enabled, 300_001);
    assert_eq!(message.bytes(), b"abcd");
}
#[test]
fn constructor_and_time_overflow_leave_ownership_intact() {
    assert!(matches!(
        IncomingTransactions::try_new(
            PEER,
            SMS,
            IncomingLimits {
                maximum_message_octets: usize::MAX,
                maximum_reserved_payload_octets: usize::MAX
            },
            1,
            0
        ),
        Err(IncomingError::Limits)
    ));
    assert!(matches!(
        IncomingTransactions::try_new(
            PEER,
            SMS,
            IncomingLimits {
                maximum_reserved_payload_octets: 19,
                ..LIMITS
            },
            1,
            0
        ),
        Err(IncomingError::Limits)
    ));
    assert!(matches!(
        IncomingTransactions::try_new(PEER, SMS, LIMITS, 0, 0),
        Err(IncomingError::GuardDuration)
    ));
    assert!(matches!(
        IncomingTransactions::after_restart(PEER, SMS, LIMITS, 1, u64::MAX),
        Err(IncomingError::TimeOverflow)
    ));
    let mut bank = bank();
    assert!(matches!(
        bank.begin(PEER, SMS, &begin_packet(0), ProtocolVersion::Pv1, u64::MAX),
        Err(IncomingError::Receiver(ReceiveError::TimeOverflow))
    ));
    assert_eq!(bank.reserved_payload_octets(), 0);
    let transaction = begin(&mut bank, 0, 0);
    bank.cancel(&transaction, 0).unwrap();
    assert_eq!(
        bank.retire_fenced(&transaction, u64::MAX),
        Err(IncomingError::TimeOverflow)
    );
    assert_eq!(
        bank.receiver(&transaction).unwrap().state(),
        ReceiverState::Canceled
    );
    bank.retire_fenced(&transaction, 0).unwrap();
}

#[test]
fn foreign_receiver_and_message_owners_cannot_settle_equal_payloads() {
    let mut first = bank();
    let mut second = bank();
    let a = begin(&mut first, 0, 0);
    let b = begin(&mut second, 0, 0);
    let ma = complete(&mut first, &a, 1);
    let mb = complete(&mut second, &b, 1);
    assert_eq!(ma.bytes(), mb.bytes());
    assert_eq!(first.advance(&b, 1), Err(IncomingError::StaleTransaction));
    assert_eq!(
        first.settle_application(&a, &mb, ApplicationOutcome::Committed, 1),
        Err(IncomingError::StaleApplication)
    );
    assert!(first.pending_message(&a).unwrap().is_some());
    first
        .settle_application(&a, &ma, ApplicationOutcome::Committed, 1)
        .unwrap();
    assert!(second.pending_message(&b).unwrap().is_some());
}
#[test]
fn late_conflicting_fragment_does_not_discard_a_pending_application_message() {
    let mut bank = bank();
    let transaction = begin(&mut bank, 0, 0);
    let message = complete(&mut bank, &transaction, 1);
    let mut conflict = end_packet(0);
    conflict.segment.segment_data.push(0);
    let result = bank.receive(&transaction, PEER, SMS, &conflict, 2).unwrap();
    assert_eq!(
        result.response.unwrap().status,
        Status::DuplicateSizeMismatch
    );
    assert_eq!(
        bank.receiver(&transaction).unwrap().state(),
        ReceiverState::Failed(Status::DuplicateSizeMismatch)
    );
    assert_eq!(
        bank.pending_message(&transaction).unwrap().unwrap().bytes(),
        b"abcd"
    );
    assert_eq!(
        bank.retire_fenced(&transaction, 2),
        Err(IncomingError::PendingApplication)
    );
    bank.settle_application(&transaction, &message, ApplicationOutcome::Rejected, 2)
        .unwrap();
    bank.retire_fenced(&transaction, 2).unwrap();
}
