use super::*;
use crate::l3::relay::Teleservice;
const SMS: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
const OATS: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Oats);
use crate::l3::tsar::{
    AcknowledgmentMap, MessagePlan, OptionalServicesSupport, Response, plan_message,
};
fn sender(window: AcknowledgmentWindow) -> Sender {
    sender_at(window, ProtocolVersion::Pv1, 0)
}
fn sender_at(window: AcknowledgmentWindow, version: ProtocolVersion, now: u64) -> Sender {
    let message: Vec<u8> = (0..20).collect();
    let MessagePlan::Segmented(plan) = plan_message(&message, 10, 5, window, version).unwrap()
    else {
        unreachable!()
    };
    Sender::try_new(MobileIdentity::Min34(1), SMS, plan, version, now).unwrap()
}
fn response(status: Status, received: Vec<bool>) -> DecodedResponse {
    DecodedResponse {
        response: Response {
            transaction_id: 5,
            status,
            acknowledgments: AcknowledgmentMap::try_new(received).unwrap(),
            optional_services_support: None,
        },
        unparsed_bits: vec![],
    }
}
fn receive(
    sender: &mut Sender,
    response: &DecodedResponse,
    now: u64,
) -> Result<SenderState, Error> {
    sender.receive_response(MobileIdentity::Min34(1), SMS, response, now)
}
fn queue(sender: &mut Sender, now: u64) -> Submission {
    let submission = sender.submission().unwrap().clone();
    sender.submitted(&submission, now, now).unwrap();
    submission
}
fn send_all(sender: &mut Sender, mut now: u64) -> u64 {
    while sender.submission().is_some() {
        let tx = queue(sender, now);
        sender.accepted(&tx, now + 1).unwrap();
        now += 2;
    }
    now
}
#[test]
fn interval_starts_on_actual_layer3_handoff_and_survives_accept_and_delayed_next_receipt() {
    let mut sender = sender(AcknowledgmentWindow::Prohibited);
    assert_eq!(sender.advance(10_000).unwrap(), SenderState::Prepared);
    assert_eq!(sender.deadline_monotonic_ms(), None);
    let first = sender.submission().unwrap().clone();
    sender.submitted(&first, 10_010, 10_020).unwrap();
    assert_eq!(sender.deadline_monotonic_ms(), Some(340_010));
    sender.accepted(&first, 10_021).unwrap();
    let second = sender.submission().unwrap().clone();
    assert_eq!(sender.deadline_monotonic_ms(), Some(340_010));
    sender.submitted(&second, 340_000, 340_020).unwrap();
    assert_eq!(sender.deadline_monotonic_ms(), Some(670_000));
    assert_eq!(sender.state(), SenderState::AwaitingAccept);
    assert_eq!(sender.layer3_submissions(), 2);
}
#[test]
fn prohibited_response_completes_only_after_all_lower_layer_accepts_and_last_has_no_tsar_timer() {
    let mut sender = sender(AcknowledgmentWindow::Prohibited);
    let count = sender.plan.segments().len();
    for index in 0..count {
        let tx = queue(&mut sender, index as u64 * 2 + 1);
        if index == count - 1 {
            assert_eq!(sender.deadline_monotonic_ms(), None);
            assert_eq!(
                sender.advance(1_000_000).unwrap(),
                SenderState::AwaitingAccept
            );
            assert_eq!(
                sender.accepted(&tx, 1_000_001).unwrap(),
                SenderState::Complete(Completion::LowerLayerAccepted)
            );
        } else {
            sender.accepted(&tx, index as u64 * 2 + 2).unwrap();
        }
    }
    assert_eq!(sender.layer3_submissions(), count as u64);
    assert!(sender.submission().is_none());
}
#[test]
fn no_response_interval_expiry_aborts_even_when_next_segment_is_only_prepared() {
    let mut sender = sender(AcknowledgmentWindow::Prohibited);
    let first = queue(&mut sender, 10);
    sender.accepted(&first, 11).unwrap();
    let next = sender.submission().unwrap().clone();
    assert_eq!(sender.advance(330_009).unwrap(), SenderState::Prepared);
    assert_eq!(
        sender.advance(330_010).unwrap(),
        SenderState::Failed(SenderFailure::SegmentInterval)
    );
    assert_eq!(
        sender.submitted(&next, 330_011, 330_011),
        Err(Error::Closed)
    );
    assert!(sender.submission().is_none());
    assert_eq!(
        sender.cancel(330_012).unwrap(),
        SenderState::Failed(SenderFailure::SegmentInterval)
    );
}
#[test]
fn all_response_windows_require_peer_completion_except_quiet_exception_interval() {
    for window in [
        AcknowledgmentWindow::Every4,
        AcknowledgmentWindow::Every8,
        AcknowledgmentWindow::Every16,
        AcknowledgmentWindow::Last,
        AcknowledgmentWindow::Exceptions,
    ] {
        let mut sender = sender(window);
        let now = send_all(&mut sender, 1);
        assert_eq!(sender.state(), SenderState::AwaitingResponse);
        let deadline = sender.deadline_monotonic_ms().unwrap();
        if window == AcknowledgmentWindow::Exceptions {
            assert_eq!(
                sender.advance(deadline - 1).unwrap(),
                SenderState::AwaitingResponse
            );
            assert_eq!(
                sender.advance(deadline).unwrap(),
                SenderState::Complete(Completion::ExceptionIntervalElapsed)
            );
        } else {
            let proof = response(Status::Received, vec![true]);
            assert_eq!(
                receive(&mut sender, &proof, now + 1).unwrap(),
                SenderState::Complete(Completion::PeerReceived)
            );
        }
    }
    let mut sender = sender(AcknowledgmentWindow::Exceptions);
    let tx = queue(&mut sender, 1);
    assert_eq!(sender.advance(330_001).unwrap(), SenderState::Prepared);
    assert_eq!(sender.submission().unwrap().segment().sequence_number, 0);
    assert_eq!(sender.accepted(&tx, 330_002), Err(Error::StaleSubmission));
}
#[test]
fn selective_missing_and_crc_error_retransmissions_keep_the_original_stream() {
    let mut sender = sender(AcknowledgmentWindow::Last);
    let mut now = send_all(&mut sender, 1);
    let count = sender.plan.segments().len();
    let mut map = vec![true; count];
    map[1] = false;
    receive(
        &mut sender,
        &response(Status::InterSegmentTimeout, map),
        now,
    )
    .unwrap();
    assert_eq!(
        sender.submission().unwrap().segment(),
        &sender.plan.segments()[1]
    );
    let tx = queue(&mut sender, now + 1);
    sender.accepted(&tx, now + 2).unwrap();
    now += 3;
    receive(
        &mut sender,
        &response(Status::CrcError, vec![true; count]),
        now,
    )
    .unwrap();
    let mut sequences = Vec::new();
    while sender.submission().is_some() {
        now += 2;
        let tx = queue(&mut sender, now);
        sequences.push(tx.segment().sequence_number);
        sender.accepted(&tx, now + 1).unwrap();
    }
    assert_eq!(sequences, (0..count as u8).collect::<Vec<_>>());
    assert_eq!(
        receive(
            &mut sender,
            &response(Status::Received, vec![true; count]),
            now + 2
        )
        .unwrap(),
        SenderState::Complete(Completion::PeerReceived)
    );
}
#[test]
fn response_timeout_replays_unconfirmed_segments_and_a_late_final_response_withdraws_retry() {
    let mut sender = sender(AcknowledgmentWindow::Last);
    send_all(&mut sender, 1);
    let count = sender.plan.segments().len();
    let deadline = sender.deadline_monotonic_ms().unwrap();
    sender.advance(deadline).unwrap();
    assert_eq!(sender.deadline_monotonic_ms(), None);
    let mut sequences = Vec::new();
    let mut now = deadline + 1;
    while sender.submission().is_some() {
        let tx = queue(&mut sender, now);
        sequences.push(tx.segment().sequence_number);
        sender.accepted(&tx, now + 1).unwrap();
        now += 2;
    }
    assert_eq!(sequences, (0..count as u8).collect::<Vec<_>>());
    let deadline = sender.deadline_monotonic_ms().unwrap();
    sender.advance(deadline).unwrap();
    let stale = sender.submission().unwrap().clone();
    assert_eq!(
        receive(
            &mut sender,
            &response(Status::Received, vec![true; count]),
            deadline + 1
        )
        .unwrap(),
        SenderState::Complete(Completion::PeerReceived)
    );
    assert_eq!(
        sender.submitted(&stale, deadline + 2, deadline + 2),
        Err(Error::Closed)
    );
}
#[test]
fn continuing_response_cannot_leave_a_cancelled_retry_without_a_completion_timer() {
    let mut sender = sender(AcknowledgmentWindow::Last);
    send_all(&mut sender, 1);
    let count = sender.plan.segments().len();
    let deadline = sender.deadline_monotonic_ms().unwrap();
    sender.advance(deadline).unwrap();
    receive(
        &mut sender,
        &response(Status::Continuing, vec![true; count]),
        deadline + 1,
    )
    .unwrap();
    assert_eq!(sender.state(), SenderState::Prepared);
    assert_eq!(
        sender.submission().unwrap().segment().sequence_number,
        (count - 1) as u8
    );
    let tx = queue(&mut sender, deadline + 2);
    sender.accepted(&tx, deadline + 3).unwrap();
    assert_eq!(sender.deadline_monotonic_ms(), Some(deadline + 2 + 330_000));
}
#[test]
fn foreign_scope_unsolicited_and_malformed_responses_do_not_mutate_the_stream() {
    let mut sender = sender(AcknowledgmentWindow::Last);
    let foreign = sender_at(AcknowledgmentWindow::Last, ProtocolVersion::Pv1, 0);
    let token = foreign.submission().unwrap().clone();
    assert_eq!(sender.submitted(&token, 1, 1), Err(Error::StaleSubmission));
    assert_eq!(
        receive(&mut sender, &response(Status::Aborted, vec![false]), 1),
        Err(Error::NotSubmitted)
    );
    let tx = queue(&mut sender, 2);
    let deadline = sender.deadline_monotonic_ms();
    let proof = response(Status::Continuing, vec![true]);
    assert_eq!(
        sender.receive_response(MobileIdentity::Min34(2), SMS, &proof, 3),
        Err(Error::Peer)
    );
    assert_eq!(
        sender.receive_response(MobileIdentity::Min34(1), OATS, &proof, 3),
        Err(Error::Service)
    );
    let mut bad = proof.clone();
    bad.response.transaction_id = 4;
    assert_eq!(receive(&mut sender, &bad, 3), Err(Error::TransactionId));
    bad = proof.clone();
    bad.unparsed_bits.push(false);
    assert_eq!(receive(&mut sender, &bad, 3), Err(Error::UnparsedResponse));
    assert_eq!(
        receive(
            &mut sender,
            &response(Status::Continuing, vec![true, true]),
            3
        ),
        Err(Error::UnsubmittedAcknowledgment)
    );
    assert_eq!(
        receive(&mut sender, &response(Status::Received, vec![true]), 3),
        Err(Error::ContradictoryReceipt)
    );
    bad = response(Status::MultipleServicesUnsupported, vec![false]);
    bad.response.optional_services_support = Some(OptionalServicesSupport {
        compression: false,
        encryption: false,
        acknowledgment_window: false,
        crc: false,
    });
    assert_eq!(
        receive(&mut sender, &bad, 3),
        Err(Error::Codec(tsar::Error::Version))
    );
    assert_eq!(sender.state(), SenderState::AwaitingAccept);
    assert_eq!(sender.deadline_monotonic_ms(), deadline);
    assert_eq!(sender.accepted(&token, 3), Err(Error::StaleSubmission));
    sender.accepted(&tx, 4).unwrap();
}
#[test]
fn lower_layer_failure_peer_failure_and_cancellation_are_terminal() {
    let mut sender = sender(AcknowledgmentWindow::Last);
    let tx = queue(&mut sender, 1);
    let failed = SenderState::Failed(SenderFailure::LowerLayer(LowerLayerFailure::Rejected {
        cause: 4,
    }));
    assert_eq!(
        sender
            .lower_failed(&tx, LowerLayerFailure::Rejected { cause: 4 }, 2)
            .unwrap(),
        failed
    );
    assert_eq!(sender.advance(1_000_000).unwrap(), failed);
    for status in [
        Status::Aborted,
        Status::CrcSelectionUnsupported,
        Status::CompressionUnsupported,
        Status::EncryptionUnsupported,
        Status::AcknowledgmentWindowUnsupported,
        Status::ProtocolUnsupported,
        Status::MessageLengthError,
        Status::MessageTooLong,
        Status::DuplicateSizeMismatch,
        Status::MultipleServicesUnsupported,
    ] {
        let mut sender = sender_at(AcknowledgmentWindow::Last, ProtocolVersion::Pv1, 0);
        queue(&mut sender, 1);
        assert_eq!(
            receive(&mut sender, &response(status, vec![false]), 2).unwrap(),
            SenderState::Failed(SenderFailure::Peer(status))
        );
    }
    let mut cancelled = sender_at(AcknowledgmentWindow::Last, ProtocolVersion::Pv1, 0);
    let pending = cancelled.submission().unwrap().clone();
    cancelled.cancel(1).unwrap();
    assert_eq!(cancelled.submitted(&pending, 2, 2), Err(Error::Closed));
}
#[test]
fn invalid_times_overflow_and_late_no_response_handoffs_do_not_reset_deadlines() {
    let mut sender = sender(AcknowledgmentWindow::Last);
    let tx = sender.submission().unwrap().clone();
    assert_eq!(sender.submitted(&tx, 2, 1), Err(Error::ReceiptTime));
    sender.submitted(&tx, 10, 10).unwrap();
    assert_eq!(sender.advance(9), Err(Error::ClockRegression));
    let mut overflow = sender_at(
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
        u64::MAX - 1,
    );
    let tx = overflow.submission().unwrap().clone();
    assert_eq!(
        overflow.submitted(&tx, u64::MAX - 1, u64::MAX - 1),
        Err(Error::TimeOverflow)
    );
    assert_eq!(overflow.layer3_submissions(), 0);
    assert_eq!(overflow.state(), SenderState::Prepared);
    let mut late = sender_at(AcknowledgmentWindow::Prohibited, ProtocolVersion::Pv1, 0);
    let first = queue(&mut late, 1);
    late.accepted(&first, 2).unwrap();
    let next = late.submission().unwrap().clone();
    assert_eq!(
        late.submitted(&next, 330_001, 330_010),
        Err(Error::ReceiptTime)
    );
    assert_eq!(
        late.state(),
        SenderState::Failed(SenderFailure::SegmentInterval)
    );
}

#[test]
fn progress_maps_preserve_unchanged_prepared_ownership_but_missing_earlier_data_preempts_it() {
    let mut first = sender(AcknowledgmentWindow::Last);
    let tx = queue(&mut first, 1);
    first.accepted(&tx, 2).unwrap();
    let next = first.submission().unwrap().clone();
    receive(&mut first, &response(Status::Continuing, vec![true]), 3).unwrap();
    first.submitted(&next, 4, 4).unwrap();
    assert_eq!(first.layer3_submissions(), 2);
    let mut second = sender(AcknowledgmentWindow::Last);
    let tx = queue(&mut second, 1);
    second.accepted(&tx, 2).unwrap();
    let next = second.submission().unwrap().clone();
    receive(&mut second, &response(Status::Continuing, vec![false]), 3).unwrap();
    assert_eq!(second.submission().unwrap().segment().sequence_number, 0);
    assert_eq!(second.submitted(&next, 4, 4), Err(Error::StaleSubmission));
}
#[test]
fn an_exception_received_at_the_deadline_prevents_quiet_success_and_retries_the_missing_segment() {
    let mut sender = sender(AcknowledgmentWindow::Exceptions);
    send_all(&mut sender, 1);
    let count = sender.plan.segments().len();
    let deadline = sender.deadline_monotonic_ms().unwrap();
    let mut map = vec![true; count];
    map[1] = false;
    receive(
        &mut sender,
        &response(Status::InterSegmentTimeout, map),
        deadline,
    )
    .unwrap();
    assert_eq!(sender.state(), SenderState::Prepared);
    assert_eq!(sender.submission().unwrap().segment().sequence_number, 1);
    assert_eq!(sender.advance(deadline).unwrap(), SenderState::Prepared);
    let tx = queue(&mut sender, deadline + 1);
    sender.accepted(&tx, deadline + 2).unwrap();
    assert_eq!(
        sender.advance(deadline + 1 + 330_000).unwrap(),
        SenderState::Complete(Completion::ExceptionIntervalElapsed)
    );
}
#[test]
fn a_positive_progress_map_is_not_final_proof_while_last_lower_layer_accept_is_missing() {
    let mut sender = sender(AcknowledgmentWindow::Exceptions);
    let count = sender.plan.segments().len();
    let mut now = 1;
    for _ in 0..count - 1 {
        let tx = queue(&mut sender, now);
        sender.accepted(&tx, now + 1).unwrap();
        now += 2;
    }
    let last = queue(&mut sender, now);
    let deadline = sender.deadline_monotonic_ms().unwrap();
    receive(
        &mut sender,
        &response(Status::Continuing, vec![true; count]),
        now + 1,
    )
    .unwrap();
    assert_eq!(sender.advance(deadline).unwrap(), SenderState::Prepared);
    assert_eq!(
        sender.submission().unwrap().segment().sequence_number,
        (count - 1) as u8
    );
    assert_eq!(
        sender.accepted(&last, deadline + 1),
        Err(Error::StaleSubmission)
    );
}
#[test]
fn logical_full_sequence_maps_do_not_choose_a_wire_encoding_and_no_response_rejects_peer_responses()
{
    let MessagePlan::Segmented(plan) = plan_message(
        &[0xaa; 510],
        5,
        5,
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
    )
    .unwrap() else {
        unreachable!()
    };
    let mut large =
        Sender::try_new(MobileIdentity::Min34(1), SMS, plan, ProtocolVersion::Pv1, 0).unwrap();
    let now = send_all(&mut large, 1);
    assert_eq!(large.layer3_submissions(), 256);
    let proof = response(Status::Received, vec![true; 256]);
    assert_eq!(
        proof.response.encode(ProtocolVersion::Pv1),
        Err(tsar::Error::AmbiguousMapLength)
    );
    assert_eq!(
        receive(&mut large, &proof, now).unwrap(),
        SenderState::Complete(Completion::PeerReceived)
    );
    let mut prohibited = sender(AcknowledgmentWindow::Prohibited);
    queue(&mut prohibited, 1);
    assert_eq!(
        receive(&mut prohibited, &response(Status::Received, vec![true]), 2),
        Err(Error::ResponsesProhibited)
    );
}
#[test]
fn periodic_response_windows_do_not_block_original_progress_and_all_received_progress_still_needs_final_status()
 {
    for window in [
        AcknowledgmentWindow::Every4,
        AcknowledgmentWindow::Every8,
        AcknowledgmentWindow::Every16,
    ] {
        let MessagePlan::Segmented(plan) =
            plan_message(&[0xaa; 100], 16, 5, window, ProtocolVersion::Pv1).unwrap()
        else {
            unreachable!()
        };
        let mut sender =
            Sender::try_new(MobileIdentity::Min34(1), SMS, plan, ProtocolVersion::Pv1, 0).unwrap();
        let count = sender.plan.segments().len();
        assert_eq!(count, 8);
        let mut now = 1;
        for index in 0..count {
            let tx = queue(&mut sender, now);
            sender.accepted(&tx, now + 1).unwrap();
            now += 2;
            if index + 1 < count {
                assert_eq!(
                    sender.submission().unwrap().segment().sequence_number,
                    (index + 1) as u8
                );
            }
        }
        receive(
            &mut sender,
            &response(Status::Continuing, vec![true; count]),
            now,
        )
        .unwrap();
        assert_eq!(sender.state(), SenderState::AwaitingResponse);
        let deadline = sender.deadline_monotonic_ms().unwrap();
        sender.advance(deadline).unwrap();
        assert_eq!(
            sender.submission().unwrap().segment().sequence_number,
            (count - 1) as u8
        );
    }
}

#[test]
fn constructor_validation_and_counter_overflow_do_not_create_partial_handoffs() {
    let MessagePlan::Segmented(plan) = plan_message(
        &[0xaa; 20],
        10,
        5,
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
    )
    .unwrap() else {
        unreachable!()
    };
    for peer in [MobileIdentity::Min34(0), MobileIdentity::Min34(1 << 34)] {
        assert!(matches!(
            Sender::try_new(peer, SMS, plan.clone(), ProtocolVersion::Pv1, 0),
            Err(Error::Identity)
        ));
    }
    assert!(matches!(
        Sender::try_new(
            MobileIdentity::Min34(1),
            SMS,
            plan,
            ProtocolVersion::Is136RevA,
            0
        ),
        Err(Error::Codec(tsar::Error::Version))
    ));
    let mut sender = sender(AcknowledgmentWindow::Last);
    sender.layer3_submissions = u64::MAX;
    let tx = sender.submission().unwrap().clone();
    assert_eq!(sender.submitted(&tx, 1, 1), Err(Error::CounterOverflow));
    assert_eq!(sender.deadline_monotonic_ms(), None);
    assert_eq!(sender.state(), SenderState::Prepared);
    assert!(!sender.submitted.iter().any(|value| *value));
}

#[test]
fn only_segmented_standard_or_carrier_protocols_can_own_a_sender() {
    let MessagePlan::Segmented(plan) = plan_message(
        &[0xaa; 20],
        10,
        5,
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
    )
    .unwrap() else {
        unreachable!()
    };
    for raw in 0..=u8::MAX {
        let result = Sender::try_new(
            MobileIdentity::Min34(1),
            HigherLayerProtocol::CarrierSpecific(raw),
            plan.clone(),
            ProtocolVersion::Pv1,
            0,
        );
        if raw >= 0xc0 {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(Error::Service)));
        }
    }
    assert!(matches!(
        Sender::try_new(
            MobileIdentity::Min34(1),
            HigherLayerProtocol::Sms,
            plan.clone(),
            ProtocolVersion::Pv1,
            0
        ),
        Err(Error::Service)
    ));
    let protocol = HigherLayerProtocol::CarrierSpecific(0xc1);
    let mut sender = Sender::try_new(
        MobileIdentity::Min34(1),
        protocol,
        plan,
        ProtocolVersion::Pv1,
        0,
    )
    .unwrap();
    let tx = queue(&mut sender, 1);
    let proof = response(Status::Continuing, vec![true]);
    assert_eq!(receive(&mut sender, &proof, 2), Err(Error::Service));
    assert_eq!(
        sender
            .receive_response(MobileIdentity::Min34(1), protocol, &proof, 2)
            .unwrap(),
        SenderState::AwaitingAccept
    );
    sender.accepted(&tx, 3).unwrap();
}
