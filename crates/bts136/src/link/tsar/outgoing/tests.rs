use super::*;
use crate::l3::{
    relay::Teleservice,
    tsar::{AcknowledgmentMap, Response, Status},
};
const PEER: MobileIdentity = MobileIdentity::Min34(1);
const SMS: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
fn bank() -> OutgoingTransactions {
    OutgoingTransactions::try_new(PEER, SMS, 100, 0).unwrap()
}
fn start(bank: &mut OutgoingTransactions, now: u64) -> Transaction {
    let Start::Segmented(transaction) = bank
        .start(
            b"abcdefghijklmnopqrst",
            10,
            AcknowledgmentWindow::Last,
            ProtocolVersion::Pv1,
            now,
        )
        .unwrap()
    else {
        panic!("expected segmentation");
    };
    transaction
}
#[test]
fn eight_owned_senders_and_guarded_reuse_reject_old_callbacks() {
    let mut bank = bank();
    let transactions: Vec<_> = (0..TRANSACTION_COUNT)
        .map(|_| start(&mut bank, 0))
        .collect();
    assert_eq!(
        transactions
            .iter()
            .map(Transaction::transaction_id)
            .collect::<Vec<_>>(),
        (0..TRANSACTION_COUNT as u8).collect::<Vec<_>>()
    );
    assert!(matches!(
        bank.start(
            b"abcdefghijklmnopqrst",
            10,
            AcknowledgmentWindow::Last,
            ProtocolVersion::Pv1,
            0
        ),
        Err(OutgoingError::Exhausted)
    ));
    let original = &transactions[0];
    let old_submission = bank.sender(original).unwrap().submission().unwrap().clone();
    assert_eq!(
        bank.retire_fenced(original, 0),
        Err(OutgoingError::StillActive)
    );
    bank.dispatch(original, SenderEvent::Cancel, 0).unwrap();
    bank.retire_fenced(original, 1).unwrap();
    assert!(matches!(
        bank.start(
            b"abcdefghijklmnopqrst",
            10,
            AcknowledgmentWindow::Last,
            ProtocolVersion::Pv1,
            100
        ),
        Err(OutgoingError::Exhausted)
    ));
    let replacement = start(&mut bank, 101);
    assert_eq!(replacement.transaction_id(), original.transaction_id());
    assert!(matches!(
        bank.sender(original),
        Err(OutgoingError::StaleTransaction)
    ));
    assert_eq!(
        bank.dispatch(original, SenderEvent::Advance, 101),
        Err(OutgoingError::StaleTransaction)
    );
    assert_eq!(
        bank.retire_fenced(original, 101),
        Err(OutgoingError::StaleTransaction)
    );
    assert_eq!(
        bank.dispatch(
            &replacement,
            SenderEvent::Submitted {
                submission: &old_submission,
                queued_monotonic_ms: 101
            },
            101
        ),
        Err(OutgoingError::Sender(SenderError::StaleSubmission))
    );
    assert_eq!(
        bank.sender(&replacement).unwrap().state(),
        SenderState::Prepared
    );
}
#[test]
fn restart_holds_all_ids_and_fitting_messages_bypass_exhaustion() {
    let mut bank = OutgoingTransactions::after_restart(PEER, SMS, 100, 500).unwrap();
    assert!(matches!(
        bank.start(
            b"abcdefghijklmnopqrst",
            10,
            AcknowledgmentWindow::Last,
            ProtocolVersion::Pv1,
            599
        ),
        Err(OutgoingError::Exhausted)
    ));
    assert!(
        matches!(bank.start(b"ok", 10, AcknowledgmentWindow::Last, ProtocolVersion::Is136Rev0, 599).unwrap(), Start::Unsegmented(bytes) if bytes == b"ok")
    );
    let previous = start(&mut bank, 600);
    assert_eq!(previous.transaction_id(), 0);
    let mut ids = vec![previous.transaction_id()];
    for _ in 1..TRANSACTION_COUNT {
        ids.push(start(&mut bank, 600).transaction_id());
    }
    assert_eq!(ids, (0..TRANSACTION_COUNT as u8).collect::<Vec<_>>());
}
#[test]
fn foreign_banks_peers_services_and_dropped_handles_do_not_release_ids() {
    let mut own = bank();
    let mut foreign = bank();
    let mut peer = OutgoingTransactions::try_new(MobileIdentity::Min34(2), SMS, 100, 0).unwrap();
    let mut service = OutgoingTransactions::try_new(
        PEER,
        HigherLayerProtocol::Segmented(Teleservice::Oats),
        100,
        0,
    )
    .unwrap();
    let transactions = [
        start(&mut foreign, 0),
        start(&mut peer, 0),
        start(&mut service, 0),
    ];
    let original = start(&mut own, 0);
    for transaction in transactions {
        assert_eq!(
            own.dispatch(&transaction, SenderEvent::Cancel, 0),
            Err(OutgoingError::StaleTransaction)
        );
    }
    drop(original);
    for _ in 1..TRANSACTION_COUNT {
        start(&mut own, 0);
    }
    assert!(matches!(
        own.start(
            b"abcdefghijklmnopqrst",
            10,
            AcknowledgmentWindow::Last,
            ProtocolVersion::Pv1,
            0
        ),
        Err(OutgoingError::Exhausted)
    ));
}
#[test]
fn completion_requires_real_handoffs_and_peer_receipt_before_retirement() {
    let mut bank = bank();
    let transaction = start(&mut bank, 0);
    for now in 1..10 {
        let Some(submission) = bank.sender(&transaction).unwrap().submission().cloned() else {
            break;
        };
        assert_eq!(
            submission.segment().transaction_id,
            transaction.transaction_id()
        );
        bank.dispatch(
            &transaction,
            SenderEvent::Submitted {
                submission: &submission,
                queued_monotonic_ms: now,
            },
            now,
        )
        .unwrap();
        bank.dispatch(&transaction, SenderEvent::Accepted(&submission), now)
            .unwrap();
    }
    assert_eq!(
        bank.sender(&transaction).unwrap().state(),
        SenderState::AwaitingResponse
    );
    assert_eq!(
        bank.retire_fenced(&transaction, 10),
        Err(OutgoingError::StillActive)
    );
    let response = DecodedResponse {
        response: Response {
            transaction_id: transaction.transaction_id(),
            status: Status::Received,
            acknowledgments: AcknowledgmentMap::try_new(vec![
                true;
                bank.sender(&transaction).unwrap().layer3_submissions()
                    as usize
            ])
            .unwrap(),
            optional_services_support: None,
        },
        unparsed_bits: vec![],
    };
    assert!(matches!(
        bank.dispatch(
            &transaction,
            SenderEvent::Response {
                peer: PEER,
                protocol: SMS,
                decoded: &response
            },
            10
        )
        .unwrap(),
        SenderState::Complete(super::super::Completion::PeerReceived)
    ));
    bank.retire_fenced(&transaction, 10).unwrap();
    assert!(matches!(
        bank.sender(&transaction),
        Err(OutgoingError::StaleTransaction)
    ));
}
#[test]
fn failed_plans_clock_regression_and_retirement_overflow_preserve_ownership() {
    let mut bank = bank();
    assert!(matches!(
        bank.start(
            b"abcdefghijklmnopqrst",
            1,
            AcknowledgmentWindow::Last,
            ProtocolVersion::Pv1,
            0
        ),
        Err(OutgoingError::Planning(_))
    ));
    let transaction = start(&mut bank, 10);
    assert_eq!(transaction.transaction_id(), 0);
    assert_eq!(
        bank.dispatch(&transaction, SenderEvent::Advance, 9),
        Err(OutgoingError::ClockRegression)
    );
    assert!(matches!(
        bank.start(b"x", 1, AcknowledgmentWindow::Last, ProtocolVersion::Pv1, 9),
        Err(OutgoingError::ClockRegression)
    ));
    bank.dispatch(&transaction, SenderEvent::Cancel, 10)
        .unwrap();
    assert_eq!(
        bank.retire_fenced(&transaction, u64::MAX),
        Err(OutgoingError::TimeOverflow)
    );
    assert_eq!(
        bank.sender(&transaction).unwrap().state(),
        SenderState::Cancelled
    );
    bank.retire_fenced(&transaction, 10).unwrap();
}
#[test]
fn constructor_rejects_invalid_scope_guard_and_restart_overflow() {
    assert!(matches!(
        OutgoingTransactions::try_new(PEER, SMS, 0, 0),
        Err(OutgoingError::GuardDuration)
    ));
    assert!(matches!(
        OutgoingTransactions::try_new(MobileIdentity::Min34(0), SMS, 1, 0),
        Err(OutgoingError::Identity)
    ));
    assert!(matches!(
        OutgoingTransactions::try_new(PEER, HigherLayerProtocol::Sms, 1, 0),
        Err(OutgoingError::Service)
    ));
    assert!(matches!(
        OutgoingTransactions::after_restart(PEER, SMS, 1, u64::MAX),
        Err(OutgoingError::TimeOverflow)
    ));
}
#[test]
fn lower_layer_failure_and_retry_callbacks_keep_transaction_and_submission_owners_separate() {
    let mut bank = bank();
    let first = start(&mut bank, 0);
    let second = start(&mut bank, 0);
    let attempt = bank.sender(&first).unwrap().submission().unwrap().clone();
    assert_eq!(
        bank.dispatch(
            &second,
            SenderEvent::Submitted {
                submission: &attempt,
                queued_monotonic_ms: 1
            },
            1
        ),
        Err(OutgoingError::Sender(SenderError::StaleSubmission))
    );
    bank.dispatch(
        &first,
        SenderEvent::Submitted {
            submission: &attempt,
            queued_monotonic_ms: 1,
        },
        1,
    )
    .unwrap();
    assert_eq!(
        bank.dispatch(&first, SenderEvent::Advance, 330_001)
            .unwrap(),
        SenderState::Prepared
    );
    assert_eq!(
        bank.dispatch(&first, SenderEvent::Accepted(&attempt), 330_001),
        Err(OutgoingError::Sender(SenderError::StaleSubmission))
    );
    let retry = bank.sender(&first).unwrap().submission().unwrap().clone();
    bank.dispatch(
        &first,
        SenderEvent::Submitted {
            submission: &retry,
            queued_monotonic_ms: 330_002,
        },
        330_002,
    )
    .unwrap();
    let failure = LowerLayerFailure::Rejected { cause: 4 };
    assert_eq!(
        bank.dispatch(
            &first,
            SenderEvent::LowerFailed {
                submission: &retry,
                failure
            },
            330_003
        )
        .unwrap(),
        SenderState::Failed(super::super::SenderFailure::LowerLayer(failure))
    );
    bank.retire_fenced(&first, 330_003).unwrap();
    assert_eq!(bank.sender(&second).unwrap().state(), SenderState::Prepared);
}
#[test]
fn wire_response_scope_is_still_checked_inside_the_owned_sender() {
    let mut bank = bank();
    let transaction = start(&mut bank, 0);
    let response = DecodedResponse {
        response: Response {
            transaction_id: transaction.transaction_id(),
            status: Status::Continuing,
            acknowledgments: AcknowledgmentMap::try_new(vec![false]).unwrap(),
            optional_services_support: None,
        },
        unparsed_bits: vec![],
    };
    assert_eq!(
        bank.dispatch(
            &transaction,
            SenderEvent::Response {
                peer: MobileIdentity::Min34(2),
                protocol: SMS,
                decoded: &response
            },
            1
        ),
        Err(OutgoingError::Sender(SenderError::Peer))
    );
    assert_eq!(
        bank.dispatch(
            &transaction,
            SenderEvent::Response {
                peer: PEER,
                protocol: HigherLayerProtocol::Segmented(Teleservice::Oats),
                decoded: &response
            },
            1
        ),
        Err(OutgoingError::Sender(SenderError::Service))
    );
    assert_eq!(
        bank.sender(&transaction).unwrap().state(),
        SenderState::Prepared
    );
}
