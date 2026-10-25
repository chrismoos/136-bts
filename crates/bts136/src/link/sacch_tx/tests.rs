use super::*;
use crate::{l3::legacy_traffic::TrafficMessage, link::traffic, phy::sacch::SacchReceiver};
const DVCC: u8 = 42;
fn words() -> Vec<SignalingWord> {
    traffic::encode(&TrafficMessage::Audit).unwrap()
}
#[test]
fn first_word_completes_after_twenty_two_assigned_slots_and_decodes_as_audit() {
    let mut tx = SacchTx::try_new(DVCC, 4, 100).unwrap();
    let command = tx.enqueue(&words()).unwrap();
    let mut receiver = SacchReceiver::default();
    let mut signaling = traffic::TrafficMessageReceiver::new(crate::l3::relay::Direction::Forward);
    let mut messages = Vec::new();
    for interval in 100..123 {
        let prepared = tx.prepare(interval).unwrap().clone();
        if let Some(word) = receiver.push(*prepared.field(), DVCC)
            && let Some(message) = signaling.push(&SignalingWord::from(&word)).unwrap()
        {
            messages.push(message);
        }
        let completed = tx.transmitted(&prepared).unwrap();
        if interval == 121 {
            assert_eq!(completed, Some(command.clone()));
        } else {
            assert_eq!(completed, None);
        }
    }
    assert_eq!(messages, [TrafficMessage::Audit]);
}
#[test]
fn discard_foreign_and_repeated_receipts_cannot_advance_continuous_encoder_state() {
    let mut tx = SacchTx::try_new(DVCC, 4, 0).unwrap();
    tx.enqueue(&words()).unwrap();
    let first = tx.prepare(0).unwrap().clone();
    let data = *first.field();
    assert_eq!(*tx.prepare(0).unwrap().field(), data);
    assert_eq!(tx.enqueue(&words()), Err(TxError::Prepared));
    tx.discard(&first).unwrap();
    let next = tx.prepare(0).unwrap().clone();
    assert_eq!(*next.field(), data);
    assert_eq!(tx.transmitted(&first), Err(TxError::StaleField));
    let mut foreign = SacchTx::try_new(DVCC, 4, 0).unwrap();
    let foreign = foreign.prepare(0).unwrap().clone();
    assert_eq!(tx.discard(&foreign), Err(TxError::StaleField));
    assert_eq!(tx.transmitted(&foreign), Err(TxError::StaleField));
    assert_eq!(tx.transmitted(&next).unwrap(), None);
    assert_eq!(tx.transmitted(&next), Err(TxError::StaleField));
}
#[test]
fn queued_messages_preserve_word_phase_coding_memory_and_delayed_completion_order() {
    let mut tx = SacchTx::try_new(DVCC, 4, 0).unwrap();
    let first = tx.enqueue(&words()).unwrap();
    let mut receiver = SacchReceiver::default();
    let mut encoded = SacchEncoder::default();
    let filler = SignalingWord::filler();
    let source = words().remove(0);
    let mut expected = Vec::new();
    expected.extend(encoded.encode_word(source.continuation, &source.contents, DVCC));
    expected.extend(encoded.encode_word(source.continuation, &source.contents, DVCC));
    for _ in 0..2 {
        expected.extend(encoded.encode_word(filler.continuation, &filler.contents, DVCC));
    }
    let mut second = None;
    let mut completed = Vec::new();
    let mut received = Vec::new();
    for (interval, expected) in expected.iter().enumerate() {
        if interval == 5 {
            second = Some(tx.enqueue(&words()).unwrap());
        }
        let prepared = tx.prepare(interval as u64).unwrap().clone();
        assert_eq!(prepared.field(), expected);
        if let Some(word) = receiver.push(*prepared.field(), DVCC) {
            received.push(word);
        }
        if let Some(command) = tx.transmitted(&prepared).unwrap() {
            completed.push((interval, command));
        }
    }
    assert_eq!(completed, [(21, first), (32, second.unwrap())]);
    assert_eq!(received[0].message, source.contents);
    assert_eq!(received[1].message, source.contents);
}
#[test]
fn cancellation_cannot_remove_an_active_word_or_its_diagonal_pipeline() {
    let mut tx = SacchTx::try_new(DVCC, 4, 0).unwrap();
    let mut stream = words();
    let mut continuation = stream[0].clone();
    continuation.continuation = true;
    stream.push(continuation);
    let command = tx.enqueue(&stream).unwrap();
    let first = tx.prepare(0).unwrap().clone();
    tx.transmitted(&first).unwrap();
    assert_eq!(
        tx.cancel(&command).unwrap(),
        Cancellation {
            removed_words: 1,
            in_flight: true
        }
    );
    for interval in 1..22 {
        let prepared = tx.prepare(interval).unwrap().clone();
        assert_eq!(tx.transmitted(&prepared).unwrap(), None);
    }
    assert_eq!(
        tx.cancel(&command).unwrap(),
        Cancellation {
            removed_words: 0,
            in_flight: false
        }
    );
}
#[test]
fn invalid_word_stream_capacity_and_interval_overflow_preserve_the_queue() {
    let mut tx = SacchTx::try_new(DVCC, 1, 0).unwrap();
    assert_eq!(tx.enqueue(&[]), Err(TxError::Empty));
    let mut continuation = words();
    continuation[0].continuation = true;
    assert_eq!(tx.enqueue(&continuation), Err(TxError::Continuation));
    tx.enqueue(&words()).unwrap();
    assert_eq!(
        tx.enqueue(&words()),
        Err(TxError::QueueFull {
            available_words: 0,
            required_words: 1
        })
    );
    assert!(matches!(
        tx.prepare(1),
        Err(TxError::Interval {
            expected: 0,
            received: 1
        })
    ));
    assert_eq!(tx.pending_words(), 1);
    let mut boundary = SacchTx::try_new(DVCC, 1, u64::MAX - 10).unwrap();
    boundary.enqueue(&words()).unwrap();
    assert!(matches!(
        boundary.prepare(u64::MAX - 10),
        Err(TxError::IntervalOverflow)
    ));
    assert_eq!(boundary.pending_words(), 1);
    assert!(matches!(
        SacchTx::try_new(DVCC, 0, 0),
        Err(TxError::Capacity)
    ));
}
#[test]
fn audit_is_not_accepted_by_the_facch_scheduler() {
    let mut tx = crate::link::traffic_tx::TrafficTx::try_new(DVCC, 4, 0).unwrap();
    assert_eq!(
        tx.enqueue(&TrafficMessage::Audit),
        Err(crate::link::traffic_tx::TxError::SacchOnly)
    );
    assert_eq!(tx.pending_words(), 0);
}
