use super::*;
use crate::{
    codec::vselp::VselpParameters,
    link::{
        traffic::{SignalingWord, TrafficMessageReceiver},
        voice::{FrameKind, TrafficFrameReceiver},
    },
};
const DVCC: u8 = 42;
fn message() -> TrafficMessage {
    TrafficMessage::Connect
}
#[test]
fn completion_waits_for_the_second_interleaved_half_and_is_reported_once() {
    let mut tx = TrafficTx::try_new(DVCC, 4, 100).unwrap();
    let command = tx.enqueue(&message()).unwrap();
    let mut receiver = TrafficFrameReceiver::new(DVCC, VselpParameters::default());
    let mut link = TrafficMessageReceiver::default();
    let first = tx.prepare(100).unwrap().clone();
    assert!(receiver.push(Some(first.data())).unwrap().is_none());
    assert_eq!(tx.transmitted(&first).unwrap(), None);
    assert_eq!(tx.pending_words(), 0);
    let second = tx.prepare(101).unwrap().clone();
    let output = receiver.push(Some(second.data())).unwrap().unwrap();
    let FrameKind::Facch(word) = output.kind else {
        panic!("message must decode as FACCH")
    };
    assert_eq!(
        link.push(&SignalingWord::from(&word)).unwrap(),
        Some(message())
    );
    assert_eq!(tx.transmitted(&second).unwrap(), Some(command));
    assert_eq!(tx.transmitted(&second), Err(TxError::StaleInterval));
    let next = tx.prepare(102).unwrap().clone();
    assert_eq!(tx.transmitted(&next).unwrap(), None);
}
#[test]
fn queue_capacity_rejection_is_atomic_and_prepared_intervals_freeze_mutations() {
    let mut tx = TrafficTx::try_new(DVCC, 2, 0).unwrap();
    let first = tx.enqueue(&message()).unwrap();
    let long = TrafficMessage::Release {
        information: crate::l3::legacy_traffic::ReleaseInformation {
            unparsed_bits: ([1; 20])
                .iter()
                .flat_map(|byte| {
                    (0..u8::BITS)
                        .rev()
                        .map(move |shift| byte & (1 << shift) != 0)
                })
                .collect(),
            ..Default::default()
        },
    };
    assert!(matches!(
        tx.enqueue(&long),
        Err(TxError::QueueFull {
            available_words: 1,
            ..
        })
    ));
    assert_eq!(tx.pending_words(), 1);
    let prepared = tx.prepare(0).unwrap().clone();
    assert_eq!(tx.enqueue(&message()), Err(TxError::Prepared));
    assert_eq!(tx.cancel(&first), Err(TxError::Prepared));
    assert_eq!(
        tx.offer_media(0, TransmitFrame::speech(&VselpParameters::default())),
        Err(TxError::Prepared)
    );
    assert_eq!(tx.prepare(0).unwrap().data(), prepared.data());
    tx.discard(&prepared).unwrap();
    let replacement = tx.prepare(0).unwrap().clone();
    assert_eq!(tx.transmitted(&prepared), Err(TxError::StaleInterval));
    assert_eq!(tx.transmitted(&replacement).unwrap(), None);
}
#[test]
fn facch_discards_stolen_interval_media_and_does_not_replay_it_after_signaling() {
    let mut tx = TrafficTx::try_new(DVCC, 4, 0).unwrap();
    tx.enqueue(&message()).unwrap();
    let media = TransmitFrame::speech(&VselpParameters::default());
    tx.offer_media(0, media.clone()).unwrap();
    assert_eq!(tx.offer_media(0, media), Err(TxError::MediaPending));
    let first = tx.prepare(0).unwrap().clone();
    tx.transmitted(&first).unwrap();
    let second = tx.prepare(1).unwrap().clone();
    let word = traffic::encode(&message()).unwrap().remove(0);
    let expected = TransmitFrame::interleave(
        &TransmitFrame::facch(&word, DVCC),
        &TransmitFrame::facch(&SignalingWord::filler(), DVCC),
    );
    assert_eq!(*second.data(), expected);
}
#[test]
fn cancellation_reports_a_partially_sent_multiword_message_and_removes_its_unsent_words() {
    let mut tx = TrafficTx::try_new(DVCC, 20, 0).unwrap();
    let long = TrafficMessage::Release {
        information: crate::l3::legacy_traffic::ReleaseInformation {
            unparsed_bits: ([1; 10])
                .iter()
                .flat_map(|byte| {
                    (0..u8::BITS)
                        .rev()
                        .map(move |shift| byte & (1 << shift) != 0)
                })
                .collect(),
            ..Default::default()
        },
    };
    let words = traffic::encode(&long).unwrap().len();
    let command = tx.enqueue(&long).unwrap();
    let other = tx.enqueue(&message()).unwrap();
    let first = tx.prepare(0).unwrap().clone();
    tx.transmitted(&first).unwrap();
    assert_eq!(
        tx.cancel(&command).unwrap(),
        Cancellation {
            removed_words: words - 1,
            in_flight: true
        }
    );
    assert_eq!(tx.pending_words(), 1);
    assert_eq!(
        tx.cancel(&other).unwrap(),
        Cancellation {
            removed_words: 1,
            in_flight: false
        }
    );
}
#[test]
fn invalid_intervals_and_foreign_receipts_cannot_change_queue_or_interleaver_history() {
    let mut tx = TrafficTx::try_new(DVCC, 4, 100).unwrap();
    let command = tx.enqueue(&message()).unwrap();
    assert!(matches!(
        tx.prepare(101),
        Err(TxError::Interval {
            expected: 100,
            received: 101
        })
    ));
    let mut foreign = TrafficTx::try_new(DVCC, 4, 100).unwrap();
    let foreign = foreign.prepare(100).unwrap().clone();
    assert_eq!(tx.transmitted(&foreign), Err(TxError::StaleInterval));
    assert_eq!(tx.discard(&foreign), Err(TxError::StaleInterval));
    assert_eq!(tx.pending_words(), 1);
    assert_eq!(
        tx.cancel(&command).unwrap(),
        Cancellation {
            removed_words: 1,
            in_flight: false
        }
    );
    let mut boundary = TrafficTx::try_new(DVCC, 4, u64::MAX).unwrap();
    assert!(matches!(
        boundary.prepare(u64::MAX),
        Err(TxError::IntervalOverflow)
    ));
    assert!(matches!(
        TrafficTx::try_new(DVCC, 0, 0),
        Err(TxError::Capacity)
    ));
}
