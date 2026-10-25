use super::*;
use crate::{
    codec::vselp::VselpParameters, link::traffic::TrafficMessageReceiver, phy::sacch::SacchReceiver,
};
const DVCC: u8 = 42;
fn tx() -> BearerTx {
    BearerTx::try_new(
        DVCC,
        3,
        QueueLimits {
            facch_words: 4,
            sacch_words: 4,
        },
        0,
    )
    .unwrap()
}
#[test]
fn one_receipt_frames_both_channels_and_reports_each_completion_at_its_own_delay() {
    use crate::l3::{
        legacy_traffic::TrafficRelay,
        relay::{HigherLayerProtocol, RelayMessage},
    };
    for message in [
        TrafficMessage::Audit,
        TrafficMessage::StopMeasurements,
        TrafficMessage::Relay(TrafficRelay {
            message: RelayMessage::Data {
                transaction_id: 42,
                protocol: HigherLayerProtocol::Sms,
                payload: vec![0xaa],
            },
            optional_octets: vec![],
        }),
        TrafficMessage::Relay(TrafficRelay {
            message: RelayMessage::Accept { transaction_id: 42 },
            optional_octets: vec![],
        }),
        TrafficMessage::Relay(TrafficRelay {
            message: RelayMessage::Reject {
                transaction_id: 42,
                cause: 4,
            },
            optional_octets: vec![],
        }),
    ] {
        let mut tx = tx();
        let facch = tx.enqueue_facch(&TrafficMessage::Connect).unwrap();
        let sacch = tx.enqueue_sacch(&message).unwrap();
        let mut receiver = SacchReceiver::default();
        let mut messages = TrafficMessageReceiver::new(crate::l3::relay::Direction::Forward);
        let mut received = Vec::new();
        for interval in 0..23 {
            let burst = tx.prepare(interval, SyncWord::Two).unwrap().clone();
            let decoded = phy_traffic::decode_forward(burst.bits()).unwrap();
            assert_eq!(decoded.fields.sync, SyncWord::Two);
            assert_eq!(decoded.fields.color.dvcc, DVCC);
            assert_eq!(decoded.coded_locator, color::encode_locator(3).unwrap());
            if let Some(word) = receiver.push(decoded.fields.sacch, DVCC)
                && let Some(message) = messages.push(&SignalingWord::from(&word)).unwrap()
            {
                received.push(message);
            }
            let completed = tx.transmitted(&burst).unwrap();
            assert_eq!(completed.interval, interval);
            assert_eq!(
                completed.facch,
                if interval == 1 {
                    Some(facch.clone())
                } else {
                    None
                }
            );
            assert_eq!(
                completed.sacch,
                if interval == 21 {
                    Some(sacch.clone())
                } else {
                    None
                }
            );
        }
        assert_eq!(received, [message]);
    }
}
#[test]
fn preparation_freezes_both_queues_and_rejects_interval_or_sync_changes() {
    let mut tx = tx();
    let facch = tx.enqueue_facch(&TrafficMessage::Connect).unwrap();
    let sacch = tx.enqueue_sacch(&TrafficMessage::Audit).unwrap();
    let first = tx.prepare(0, SyncWord::One).unwrap().clone();
    assert_eq!(tx.prepare(0, SyncWord::One).unwrap().bits(), first.bits());
    assert!(matches!(
        tx.prepare(1, SyncWord::One),
        Err(TxError::Interval {
            expected: 0,
            received: 1
        })
    ));
    assert!(matches!(
        tx.prepare(0, SyncWord::Two),
        Err(TxError::Sync {
            expected: SyncWord::One,
            received: SyncWord::Two
        })
    ));
    assert_eq!(tx.cancel_facch(&facch), Err(TxError::Prepared));
    assert_eq!(tx.cancel_sacch(&sacch), Err(TxError::Prepared));
    assert_eq!(
        tx.enqueue_facch(&TrafficMessage::Connect),
        Err(TxError::Prepared)
    );
    assert_eq!(
        tx.enqueue_sacch(&TrafficMessage::Audit),
        Err(TxError::Prepared)
    );
    assert_eq!(
        tx.offer_media(0, TransmitFrame::speech(&VselpParameters::default())),
        Err(TxError::Prepared)
    );
    tx.discard(&first).unwrap();
    let replacement = tx.prepare(0, SyncWord::One).unwrap().clone();
    assert_eq!(replacement.bits(), first.bits());
    assert_eq!(tx.transmitted(&first), Err(TxError::StaleBurst));
    tx.transmitted(&replacement).unwrap();
    assert_eq!(tx.transmitted(&replacement), Err(TxError::StaleBurst));
}
#[test]
fn sacch_prepare_failure_rolls_back_traffic_preparation_without_consuming_queues() {
    let mut tx = BearerTx::try_new(DVCC, 0, QueueLimits::default(), u64::MAX - 10).unwrap();
    let command = tx.enqueue_sacch(&TrafficMessage::Audit).unwrap();
    assert!(matches!(
        tx.prepare(u64::MAX - 10, SyncWord::One),
        Err(TxError::Sacch(sacch_tx::TxError::IntervalOverflow))
    ));
    assert_eq!(tx.cancel_sacch(&command).unwrap().removed_words, 1);
    assert!(tx.enqueue_facch(&TrafficMessage::Connect).is_ok());
    let burst = tx.prepare(u64::MAX - 10, SyncWord::One).unwrap().clone();
    tx.discard(&burst).unwrap();
}
#[test]
fn invalid_child_receipt_is_detected_before_advancing_the_other_channel() {
    let mut tx = tx();
    tx.enqueue_facch(&TrafficMessage::Connect).unwrap();
    tx.enqueue_sacch(&TrafficMessage::Audit).unwrap();
    let burst = tx.prepare(0, SyncWord::One).unwrap().clone();
    tx.sacch.discard(&burst.sacch).unwrap();
    assert_eq!(
        tx.transmitted(&burst),
        Err(TxError::Sacch(sacch_tx::TxError::StaleField))
    );
    assert_eq!(tx.traffic.pending_words(), 1);
    assert!(tx.traffic.check_prepared(&burst.traffic).is_ok());
}
#[test]
fn foreign_receipts_invalid_locator_and_unsupported_sacch_messages_are_rejected() {
    let mut tx = tx();
    let mut other = self::tx();
    let foreign = other.prepare(0, SyncWord::One).unwrap().clone();
    assert_eq!(tx.transmitted(&foreign), Err(TxError::StaleBurst));
    assert_eq!(tx.discard(&foreign), Err(TxError::StaleBurst));
    assert_eq!(
        tx.enqueue_sacch(&TrafficMessage::Connect),
        Err(TxError::SacchMessage(0xc1))
    );
    assert_eq!(
        tx.enqueue_facch(&TrafficMessage::Audit),
        Err(TxError::Traffic(traffic_tx::TxError::SacchOnly))
    );
    assert!(matches!(
        BearerTx::try_new(DVCC, 128, QueueLimits::default(), 0),
        Err(TxError::Locator(128))
    ));
}

#[test]
fn facch_pending_distinguishes_queued_words_from_the_interleaver_tail() {
    let mut tx = tx();
    assert!(!tx.facch_pending());
    let handle = tx.enqueue_facch(&TrafficMessage::Connect).unwrap();
    assert!(tx.facch_pending());
    let burst = tx.prepare(0, SyncWord::One).unwrap().clone();
    assert!(tx.facch_pending());
    assert!(tx.submitted(&burst).unwrap().facch.is_none());
    assert!(!tx.facch_pending());
    let burst = tx.prepare(1, SyncWord::One).unwrap().clone();
    assert_eq!(tx.submitted(&burst).unwrap().facch, Some(handle));
    let handle = tx.enqueue_facch(&TrafficMessage::Connect).unwrap();
    assert!(tx.facch_pending());
    tx.cancel_facch(&handle).unwrap();
    assert!(!tx.facch_pending());
}
