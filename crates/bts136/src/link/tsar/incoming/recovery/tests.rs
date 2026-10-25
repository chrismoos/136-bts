use super::*;
use crate::l3::{
    relay::Teleservice,
    tsar::{AcknowledgmentWindow, OptionalServices, Status},
};
const PEER: MobileIdentity = MobileIdentity::Min34(1);
const PROTOCOL: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
const LIMITS: IncomingLimits = IncomingLimits {
    maximum_message_octets: 10,
    maximum_reserved_payload_octets: 160,
};
const GUARD_MS: u64 = 100;
fn context(id: u8) -> RecoveryContext {
    RecoveryContext {
        peer: PEER,
        protocol: PROTOCOL,
        transaction_id: id,
        version: ProtocolVersion::Pv1,
        maximum_message_octets: LIMITS.maximum_message_octets,
        created_monotonic_ms: 0,
    }
}
fn begin(id: u8) -> Segment {
    Segment {
        transaction_id: id,
        sequence_number: 0,
        segment_data: b"ab".to_vec(),
        kind: SegmentKind::Begin {
            message_length: Some(4),
            services: Some(crate::l3::tsar::OptionalServices {
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
fn observation(now: u64, event: RecoveryEvent) -> RecoveryObservation {
    RecoveryObservation {
        monotonic_ms: now,
        event,
    }
}
fn replay(id: u8, history: Vec<RecoveryObservation>) -> RecoveredIncoming {
    RecoveredIncoming::replay(context(id), history).unwrap()
}
fn decoded(segment: Segment) -> DecodedSegment {
    DecodedSegment {
        segment,
        unparsed_octets: Vec::new(),
    }
}
#[test]
fn independent_histories_restore_without_global_clock_regression_and_keep_other_ids_guarded() {
    let a = replay(
        0,
        vec![
            observation(100, RecoveryEvent::Segment(begin(0))),
            observation(200, RecoveryEvent::Segment(end(0))),
            observation(
                300,
                RecoveryEvent::ApplicationSettled(ApplicationOutcome::Rejected),
            ),
        ],
    );
    let b = replay(1, vec![observation(5, RecoveryEvent::Segment(begin(1)))]);
    let RecoveredBank {
        mut bank,
        transactions,
    } = IncomingTransactions::recover(PEER, PROTOCOL, LIMITS, GUARD_MS, 1000, vec![a, b]).unwrap();
    assert_eq!(
        bank.application_outcome(&transactions[0]).unwrap(),
        Some(ApplicationOutcome::Rejected)
    );
    assert_eq!(
        bank.receiver(&transactions[1])
            .unwrap()
            .deadline_monotonic_ms(),
        Some(300_005)
    );
    assert!(matches!(
        bank.begin(
            PEER,
            PROTOCOL,
            &decoded(begin(2)),
            ProtocolVersion::Pv1,
            1000
        ),
        Err(IncomingError::Guarded)
    ));
    assert!(
        bank.begin(
            PEER,
            PROTOCOL,
            &decoded(begin(2)),
            ProtocolVersion::Pv1,
            1100
        )
        .is_ok()
    );
}
#[test]
fn pending_application_ownership_is_fresh_and_cannot_be_settled_by_an_old_handle() {
    let mut old = IncomingTransactions::try_new(PEER, PROTOCOL, LIMITS, GUARD_MS, 0).unwrap();
    let old_owner = old
        .begin(PEER, PROTOCOL, &decoded(begin(0)), ProtocolVersion::Pv1, 0)
        .unwrap()
        .transaction;
    let old_message = old
        .receive(&old_owner, PEER, PROTOCOL, &decoded(end(0)), 1)
        .unwrap()
        .message
        .unwrap();
    let recovered = replay(
        0,
        vec![
            observation(0, RecoveryEvent::Segment(begin(0))),
            observation(1, RecoveryEvent::Segment(end(0))),
        ],
    );
    let RecoveredBank {
        mut bank,
        transactions,
    } = IncomingTransactions::recover(PEER, PROTOCOL, LIMITS, GUARD_MS, 1000, vec![recovered])
        .unwrap();
    assert!(matches!(
        bank.receiver(&old_owner),
        Err(IncomingError::StaleTransaction)
    ));
    let owner = &transactions[0];
    assert_eq!(
        bank.pending_message(owner).unwrap().unwrap().bytes(),
        b"abcd"
    );
    assert!(matches!(
        bank.settle_application(owner, &old_message, ApplicationOutcome::Committed, 1000),
        Err(IncomingError::StaleApplication)
    ));
    let message = bank.pending_message(owner).unwrap().unwrap().clone();
    bank.settle_application(owner, &message, ApplicationOutcome::Committed, 1000)
        .unwrap();
    assert_eq!(
        bank.application_outcome(owner).unwrap(),
        Some(ApplicationOutcome::Committed)
    );
}
#[test]
fn settled_application_is_not_delivered_again_after_replay_or_duplicate_end() {
    let recovered = replay(
        0,
        vec![
            observation(0, RecoveryEvent::Segment(begin(0))),
            observation(1, RecoveryEvent::Segment(end(0))),
            observation(
                2,
                RecoveryEvent::ApplicationSettled(ApplicationOutcome::Committed),
            ),
            observation(
                3,
                RecoveryEvent::ApplicationSettled(ApplicationOutcome::Committed),
            ),
        ],
    );
    let RecoveredBank {
        mut bank,
        transactions,
    } = IncomingTransactions::recover(PEER, PROTOCOL, LIMITS, GUARD_MS, 1000, vec![recovered])
        .unwrap();
    let owner = &transactions[0];
    assert!(bank.pending_message(owner).unwrap().is_none());
    assert_eq!(
        bank.application_outcome(owner).unwrap(),
        Some(ApplicationOutcome::Committed)
    );
    assert!(
        bank.receive(owner, PEER, PROTOCOL, &decoded(end(0)), 1000)
            .unwrap()
            .message
            .is_none()
    );
}
#[test]
fn completed_pending_bytes_survive_a_later_conflicting_segment_during_replay() {
    let mut conflict = begin(0);
    conflict.segment_data = b"zz".to_vec();
    let recovered = replay(
        0,
        vec![
            observation(0, RecoveryEvent::Segment(begin(0))),
            observation(1, RecoveryEvent::Segment(end(0))),
            observation(2, RecoveryEvent::Segment(conflict)),
        ],
    );
    let RecoveredBank {
        mut bank,
        transactions,
    } = IncomingTransactions::recover(PEER, PROTOCOL, LIMITS, GUARD_MS, 1000, vec![recovered])
        .unwrap();
    let owner = &transactions[0];
    assert_eq!(
        bank.receiver(owner).unwrap().state(),
        ReceiverState::Failed(Status::Aborted)
    );
    assert_eq!(
        bank.pending_message(owner).unwrap().unwrap().bytes(),
        b"abcd"
    );
    assert!(matches!(
        bank.retire_fenced(owner, 1000),
        Err(IncomingError::PendingApplication)
    ));
}
#[test]
fn malformed_history_rejects_instead_of_publishing_partial_recovery() {
    assert!(matches!(
        RecoveredIncoming::replay(
            context(0),
            vec![
                observation(10, RecoveryEvent::Segment(begin(0))),
                observation(9, RecoveryEvent::Advance)
            ]
        ),
        Err(IncomingError::ClockRegression)
    ));
    assert!(matches!(
        RecoveredIncoming::replay(
            context(0),
            vec![observation(
                0,
                RecoveryEvent::ApplicationSettled(ApplicationOutcome::Committed)
            )]
        ),
        Err(IncomingError::StaleApplication)
    ));
    assert!(matches!(
        RecoveredIncoming::replay(
            context(0),
            vec![observation(0, RecoveryEvent::Segment(begin(1)))]
        ),
        Err(IncomingError::Receiver(ReceiveError::TransactionId))
    ));
    assert!(RecoveredIncoming::replay(context(8), Vec::new()).is_err());
    assert!(matches!(
        RecoveredIncoming::replay(
            context(0),
            vec![observation(0, RecoveryEvent::Segment(end(0)))]
        ),
        Err(IncomingError::Receiver(ReceiveError::BeginRequired))
    ));
}
#[test]
fn duplicate_slots_capacity_policy_and_future_history_reject_atomically() {
    let make = || replay(0, vec![observation(1, RecoveryEvent::Segment(begin(0)))]);
    assert!(matches!(
        IncomingTransactions::recover(PEER, PROTOCOL, LIMITS, GUARD_MS, 1000, vec![make(), make()]),
        Err(IncomingError::Occupied)
    ));
    assert!(matches!(
        IncomingTransactions::recover(PEER, PROTOCOL, LIMITS, GUARD_MS, 0, vec![make()]),
        Err(IncomingError::ClockRegression)
    ));
    assert!(matches!(
        IncomingTransactions::recover(
            PEER,
            PROTOCOL,
            IncomingLimits {
                maximum_message_octets: 11,
                ..LIMITS
            },
            GUARD_MS,
            1000,
            vec![make()]
        ),
        Err(IncomingError::Limits)
    ));
    let b = replay(1, vec![observation(1, RecoveryEvent::Segment(begin(1)))]);
    assert!(matches!(
        IncomingTransactions::recover(
            PEER,
            PROTOCOL,
            IncomingLimits {
                maximum_reserved_payload_octets: 20,
                ..LIMITS
            },
            GUARD_MS,
            1000,
            vec![make(), b]
        ),
        Err(IncomingError::Capacity)
    ));
    assert!(matches!(
        IncomingTransactions::recover(PEER, PROTOCOL, LIMITS, GUARD_MS, u64::MAX, Vec::new()),
        Err(IncomingError::TimeOverflow)
    ));
}
#[test]
fn restored_reassembly_timeout_keeps_its_original_deadline() {
    let recovered = replay(0, vec![observation(0, RecoveryEvent::Segment(begin(0)))]);
    let RecoveredBank {
        mut bank,
        transactions,
    } = IncomingTransactions::recover(PEER, PROTOCOL, LIMITS, GUARD_MS, 300_001, vec![recovered])
        .unwrap();
    let owner = &transactions[0];
    assert_eq!(
        bank.receiver(owner).unwrap().deadline_monotonic_ms(),
        Some(300_000)
    );
    assert_eq!(
        bank.advance(owner, 300_001).unwrap().unwrap().status,
        Status::InterSegmentTimeout
    );
    assert!(bank.advance(owner, 300_002).unwrap().is_none());
    assert_eq!(
        bank.receive(owner, PEER, PROTOCOL, &decoded(end(0)), 300_002)
            .unwrap()
            .message
            .unwrap()
            .bytes(),
        b"abcd"
    );
}
