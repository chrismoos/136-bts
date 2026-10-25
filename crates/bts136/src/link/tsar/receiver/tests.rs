use super::*;
use crate::l3::relay::Teleservice;
const SMS: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
const PEER: MobileIdentity = MobileIdentity::Min34(1);
fn receiver(window: AcknowledgmentWindow) -> (Receiver, DecodedSegment) {
    let receiver = Receiver::try_new(PEER, SMS, 3, ProtocolVersion::Pv1, 1000, 0).unwrap();
    let begin = packet(
        0,
        b"ab",
        SegmentKind::Begin {
            message_length: Some(6),
            services: Some(OptionalServices {
                acknowledgment_window: window,
                ..OptionalServices::default()
            }),
        },
    );
    (receiver, begin)
}
fn packet(sequence_number: u8, data: &[u8], kind: SegmentKind) -> DecodedSegment {
    DecodedSegment {
        segment: Segment {
            transaction_id: 3,
            sequence_number,
            segment_data: data.into(),
            kind,
        },
        unparsed_octets: vec![],
    }
}
fn middle() -> DecodedSegment {
    packet(
        1,
        b"cd",
        SegmentKind::Continue {
            message_length: None,
        },
    )
}
fn end() -> DecodedSegment {
    packet(
        2,
        b"ef",
        SegmentKind::End {
            message_length: None,
            crc: None,
        },
    )
}
fn receive(rx: &mut Receiver, packet: &DecodedSegment, now: u64) -> Reception {
    rx.receive(PEER, SMS, packet, now).unwrap()
}
#[test]
fn reassembles_in_order_once_and_answers_duplicate_end() {
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Last);
    assert_eq!(rx.deadline_monotonic_ms(), None);
    assert!(receive(&mut rx, &begin, 10).response.is_none());
    assert_eq!(rx.deadline_monotonic_ms(), Some(300_010));
    let early_end = receive(&mut rx, &end(), 20);
    let response = early_end.response.unwrap();
    assert_eq!(response.status, Status::Continuing);
    assert_eq!(response.acknowledgments.received(1), Some(false));
    assert!(early_end.message.is_none());
    assert_eq!(rx.deadline_monotonic_ms(), Some(300_020));
    let complete = receive(&mut rx, &middle(), 30);
    assert_eq!(complete.message, Some(b"abcdef".to_vec()));
    assert_eq!(complete.response.unwrap().status, Status::Received);
    assert_eq!(rx.deadline_monotonic_ms(), None);
    assert_eq!(rx.state(), ReceiverState::Complete);
    let duplicate = receive(&mut rx, &end(), 40);
    assert!(duplicate.message.is_none());
    assert_eq!(duplicate.response.unwrap().status, Status::Received);
}
#[test]
fn prohibited_timeout_discards_and_rejects_closed_stream() {
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Prohibited);
    receive(&mut rx, &begin, 0);
    assert_eq!(rx.advance(299_999).unwrap(), None);
    assert_eq!(rx.state(), ReceiverState::Receiving);
    assert_eq!(rx.advance(300_000).unwrap(), None);
    assert_eq!(rx.state(), ReceiverState::Expired);
    assert_eq!(rx.stored_octets(), 0);
    assert_eq!(
        rx.receive(PEER, SMS, &middle(), 300_001),
        Err(ReceiveError::Closed)
    );
}
#[test]
fn enabled_timeout_requests_missing_segment_and_resumes() {
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Exceptions);
    receive(&mut rx, &begin, 0);
    let response = rx.advance(300_000).unwrap().unwrap();
    assert_eq!(response.status, Status::InterSegmentTimeout);
    assert_eq!(response.acknowledgments.bit_count(), 2);
    assert_eq!(response.acknowledgments.received(0), Some(true));
    assert_eq!(response.acknowledgments.received(1), Some(false));
    assert_eq!(rx.state(), ReceiverState::Receiving);
    assert_eq!(rx.advance(300_001).unwrap(), None);
    receive(&mut rx, &middle(), 300_010);
    assert_eq!(rx.deadline_monotonic_ms(), Some(600_010));
    let complete = receive(&mut rx, &end(), 300_020);
    assert!(complete.response.is_none());
    assert_eq!(complete.message, Some(b"abcdef".to_vec()));
}
#[test]
fn identical_duplicate_refreshes_without_double_counting_and_conflict_fails() {
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Last);
    receive(&mut rx, &begin, 0);
    receive(&mut rx, &begin, 100);
    assert_eq!(rx.stored_octets(), 2);
    assert_eq!(rx.deadline_monotonic_ms(), Some(300_100));
    let mut changed = begin.clone();
    changed.segment.segment_data.push(0);
    let result = receive(&mut rx, &changed, 101);
    assert_eq!(
        result.response.unwrap().status,
        Status::DuplicateSizeMismatch
    );
    assert_eq!(
        rx.state(),
        ReceiverState::Failed(Status::DuplicateSizeMismatch)
    );
    assert_eq!(rx.stored_octets(), 0);
}
#[test]
fn scope_unknown_suffix_and_begin_admission_do_not_mutate() {
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Last);
    assert_eq!(
        rx.receive(PEER, SMS, &middle(), 1),
        Err(ReceiveError::BeginRequired)
    );
    assert_eq!(
        rx.receive(MobileIdentity::Min34(2), SMS, &begin, 1),
        Err(ReceiveError::Identity)
    );
    assert_eq!(
        rx.receive(
            PEER,
            HigherLayerProtocol::Segmented(Teleservice::Oats),
            &begin,
            1
        ),
        Err(ReceiveError::Service)
    );
    let mut bad = begin.clone();
    bad.segment.transaction_id = 2;
    assert_eq!(
        rx.receive(PEER, SMS, &bad, 1),
        Err(ReceiveError::TransactionId)
    );
    bad = begin.clone();
    bad.unparsed_octets.push(0xff);
    assert_eq!(
        rx.receive(PEER, SMS, &bad, 1),
        Err(ReceiveError::UnparsedSegment)
    );
    assert_eq!(rx.state(), ReceiverState::AwaitingBegin);
    assert_eq!(rx.stored_octets(), 0);
    assert_eq!(rx.deadline_monotonic_ms(), None);
    receive(&mut rx, &begin, 0);
}
#[test]
fn storage_length_and_unsupported_services_are_explicit_failures() {
    let (mut rx, mut begin) = receiver(AcknowledgmentWindow::Last);
    rx.maximum_message_octets = 5;
    assert_eq!(
        receive(&mut rx, &begin, 0).response.unwrap().status,
        Status::MessageTooLong
    );
    let (mut rx, _) = receiver(AcknowledgmentWindow::Last);
    if let SegmentKind::Begin {
        ref mut message_length,
        ..
    } = begin.segment.kind
    {
        *message_length = Some(7);
    }
    receive(&mut rx, &begin, 0);
    receive(&mut rx, &middle(), 1);
    assert_eq!(
        receive(&mut rx, &end(), 2).response.unwrap().status,
        Status::MessageLengthError
    );
    for (compression, crc_status, expected) in [
        (
            Compression::V42Bis,
            CrcStatus::None,
            Status::CompressionUnsupported,
        ),
        (
            Compression::None,
            CrcStatus::Crc16,
            Status::CrcSelectionUnsupported,
        ),
        (
            Compression::V42Bis,
            CrcStatus::Crc24,
            Status::MultipleServicesUnsupported,
        ),
    ] {
        let (mut rx, mut begin) = receiver(AcknowledgmentWindow::Last);
        rx.version = ProtocolVersion::Pv5;
        if let SegmentKind::Begin {
            services: Some(ref mut services),
            ..
        } = begin.segment.kind
        {
            services.compression = compression;
            services.crc_status = crc_status;
        }
        let result = receive(&mut rx, &begin, 0).response.unwrap();
        assert_eq!(result.status, expected);
        assert_eq!(
            result.optional_services_support.is_some(),
            expected == Status::MultipleServicesUnsupported
        );
        assert!(result.encode(ProtocolVersion::Pv5).is_ok());
    }
}
#[test]
fn response_windows_follow_sequence_boundary_and_final_receipt() {
    for window in [
        AcknowledgmentWindow::Prohibited,
        AcknowledgmentWindow::Every4,
        AcknowledgmentWindow::Every8,
        AcknowledgmentWindow::Every16,
        AcknowledgmentWindow::Last,
        AcknowledgmentWindow::Exceptions,
    ] {
        let (mut rx, mut begin) = receiver(window);
        if let SegmentKind::Begin {
            ref mut message_length,
            ..
        } = begin.segment.kind
        {
            *message_length = None;
        }
        receive(&mut rx, &begin, 0);
        for seq in 1..=16 {
            let packet = packet(
                seq,
                &[seq],
                SegmentKind::Continue {
                    message_length: None,
                },
            );
            let periodic = match window {
                AcknowledgmentWindow::Every4 => seq % 4 == 3,
                AcknowledgmentWindow::Every8 => seq % 8 == 7,
                AcknowledgmentWindow::Every16 => seq % 16 == 15,
                _ => false,
            };
            assert_eq!(
                receive(&mut rx, &packet, u64::from(seq)).response.is_some(),
                periodic
            );
        }
        let final_packet = packet(
            17,
            b"z",
            SegmentKind::End {
                message_length: None,
                crc: None,
            },
        );
        let result = receive(&mut rx, &final_packet, 17);
        assert!(result.message.is_some());
        assert_eq!(
            result.response.is_some(),
            !matches!(
                window,
                AcknowledgmentWindow::Prohibited | AcknowledgmentWindow::Exceptions
            )
        );
    }
}
#[test]
fn timer_boundaries_clock_overflow_and_cancel() {
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Prohibited);
    receive(&mut rx, &begin, 0);
    receive(&mut rx, &middle(), 300_000);
    assert_eq!(rx.deadline_monotonic_ms(), Some(600_000));
    assert_eq!(rx.advance(299_999), Err(ReceiveError::ClockRegression));
    rx.cancel(300_001).unwrap();
    assert_eq!(rx.state(), ReceiverState::Canceled);
    assert_eq!(rx.stored_octets(), 0);
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Last);
    assert_eq!(
        rx.receive(PEER, SMS, &begin, u64::MAX),
        Err(ReceiveError::TimeOverflow)
    );
    assert_eq!(rx.state(), ReceiverState::AwaitingBegin);
}
#[test]
fn overdue_enabled_timer_cannot_be_silently_extended_by_a_late_segment() {
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Last);
    receive(&mut rx, &begin, 0);
    assert_eq!(
        rx.receive(PEER, SMS, &middle(), 300_001),
        Err(ReceiveError::TimerDue)
    );
    assert_eq!(rx.stored_octets(), 2);
    assert_eq!(
        rx.advance(300_001).unwrap().unwrap().status,
        Status::InterSegmentTimeout
    );
    receive(&mut rx, &middle(), 300_001);
    assert_eq!(rx.deadline_monotonic_ms(), Some(600_001));
}
#[test]
fn sender_receiver_recover_a_missing_middle_without_duplicate_delivery() {
    use crate::{
        l3::tsar::{DecodedResponse, MessagePlan, plan_message},
        link::tsar::Sender,
    };
    let message: Vec<u8> = (0..20).collect();
    let MessagePlan::Segmented(plan) = plan_message(
        &message,
        10,
        3,
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
    )
    .unwrap() else {
        panic!("expected segmentation");
    };
    let mut tx = Sender::try_new(PEER, SMS, plan, ProtocolVersion::Pv1, 0).unwrap();
    let mut rx = Receiver::try_new(PEER, SMS, 3, ProtocolVersion::Pv1, 1000, 0).unwrap();
    let mut delivered = Vec::new();
    let mut dropped = false;
    for now in 1..20 {
        let Some(submission) = tx.submission().cloned() else {
            break;
        };
        tx.submitted(&submission, now, now).unwrap();
        tx.accepted(&submission, now).unwrap();
        if submission.segment().sequence_number == 1 && !dropped {
            dropped = true;
            continue;
        }
        let result = receive(
            &mut rx,
            &DecodedSegment {
                segment: submission.segment().clone(),
                unparsed_octets: vec![],
            },
            now,
        );
        if let Some(message) = result.message {
            delivered.push(message);
        }
        if let Some(response) = result.response {
            tx.receive_response(
                PEER,
                SMS,
                &DecodedResponse {
                    response,
                    unparsed_bits: vec![],
                },
                now,
            )
            .unwrap();
        }
    }
    assert!(dropped);
    assert_eq!(delivered, vec![message]);
    assert_eq!(
        tx.state(),
        crate::link::tsar::SenderState::Complete(crate::link::tsar::Completion::PeerReceived)
    );
}
#[test]
fn full_sequence_space_reassembles_without_guessing_response_wire_length() {
    let (mut rx, mut begin) = receiver(AcknowledgmentWindow::Last);
    if let SegmentKind::Begin {
        ref mut message_length,
        ..
    } = begin.segment.kind
    {
        *message_length = None;
    }
    receive(&mut rx, &begin, 0);
    for sequence in 1..u8::MAX {
        receive(
            &mut rx,
            &packet(
                sequence,
                &[sequence],
                SegmentKind::Continue {
                    message_length: None,
                },
            ),
            u64::from(sequence),
        );
    }
    let result = receive(
        &mut rx,
        &packet(
            u8::MAX,
            &[u8::MAX],
            SegmentKind::End {
                message_length: None,
                crc: None,
            },
        ),
        u64::from(u8::MAX),
    );
    assert_eq!(result.message.unwrap().len(), 257);
    let response = result.response.unwrap();
    assert_eq!(response.acknowledgments.bit_count(), SEQUENCE_COUNT);
    assert_eq!(
        response.encode(ProtocolVersion::Pv1),
        Err(tsar::Error::AmbiguousMapLength)
    );
}
#[test]
fn contradictory_end_and_data_beyond_end_fail_without_delivering() {
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Last);
    receive(&mut rx, &begin, 0);
    receive(&mut rx, &end(), 1);
    let beyond = packet(
        3,
        b"x",
        SegmentKind::Continue {
            message_length: None,
        },
    );
    assert_eq!(
        receive(&mut rx, &beyond, 2).response.unwrap().status,
        Status::Aborted
    );
    assert_eq!(rx.state(), ReceiverState::Failed(Status::Aborted));
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Last);
    receive(&mut rx, &begin, 0);
    receive(
        &mut rx,
        &packet(
            3,
            b"x",
            SegmentKind::Continue {
                message_length: None,
            },
        ),
        1,
    );
    assert_eq!(
        receive(&mut rx, &end(), 2).response.unwrap().status,
        Status::Aborted
    );
}
#[test]
fn impossible_nonfinal_sequence_and_early_length_overrun_fail() {
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Last);
    receive(&mut rx, &begin, 0);
    let last_continue = packet(
        u8::MAX,
        b"x",
        SegmentKind::Continue {
            message_length: None,
        },
    );
    assert_eq!(
        receive(&mut rx, &last_continue, 1).response.unwrap().status,
        Status::Aborted
    );
    let (mut rx, begin) = receiver(AcknowledgmentWindow::Last);
    receive(&mut rx, &begin, 0);
    let too_long = packet(
        1,
        b"12345",
        SegmentKind::Continue {
            message_length: None,
        },
    );
    assert_eq!(
        receive(&mut rx, &too_long, 1).response.unwrap().status,
        Status::MessageLengthError
    );
    assert_eq!(rx.stored_octets(), 0);
}
