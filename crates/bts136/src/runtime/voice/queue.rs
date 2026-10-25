use crate::{
    l3::legacy_traffic::TrafficMessage,
    link::{
        bearer_tx::{BearerTx, TxError},
        sacch_tx, traffic_tx,
    },
};
pub(super) fn facch(
    tx: &mut BearerTx,
    message: &TrafficMessage,
) -> Result<Option<traffic_tx::CommandHandle>, TxError> {
    match tx.enqueue_facch(message) {
        Ok(handle) => Ok(Some(handle)),
        Err(TxError::Traffic(traffic_tx::TxError::QueueFull { .. })) => Ok(None),
        Err(error) => Err(error),
    }
}
pub(super) fn sacch(
    tx: &mut BearerTx,
    message: &TrafficMessage,
) -> Result<Option<sacch_tx::CommandHandle>, TxError> {
    match tx.enqueue_sacch(message) {
        Ok(handle) => Ok(Some(handle)),
        Err(TxError::Sacch(sacch_tx::TxError::QueueFull { .. })) => Ok(None),
        Err(error) => Err(error),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        l3::legacy_traffic::{CallInformation, CallingNumber, FlashMessage},
        link::bearer_tx::QueueLimits,
        phy::sync::SyncWord,
    };
    fn transmitter() -> BearerTx {
        BearerTx::try_new(
            1,
            0,
            QueueLimits {
                facch_words: 2,
                sacch_words: 2,
            },
            0,
        )
        .unwrap()
    }
    #[test]
    fn full_facch_defers_information_until_capacity_is_available_without_changing_prepared_iq() {
        let mut tx = transmitter();
        for _ in 0..2 {
            facch(&mut tx, &TrafficMessage::Connect).unwrap().unwrap();
        }
        let message = TrafficMessage::Flash(FlashMessage::Forward {
            request_number: 0,
            information: CallInformation {
                calling_numbers: vec![CallingNumber::network("1001").unwrap()],
                ..Default::default()
            },
        });
        assert!(facch(&mut tx, &message).unwrap().is_none());
        for interval in 0..2 {
            let prepared = tx.prepare(interval, SyncWord::Two).unwrap().clone();
            assert!(matches!(facch(&mut tx, &message), Err(TxError::Prepared)));
            assert_eq!(
                tx.prepare(interval, SyncWord::Two).unwrap().bits(),
                prepared.bits()
            );
            tx.submitted(&prepared).unwrap();
        }
        assert!(facch(&mut tx, &message).unwrap().is_some());
    }
    #[test]
    fn full_sacch_defers_only_capacity_errors_and_preserves_channel_validation() {
        let mut tx = transmitter();
        for _ in 0..2 {
            sacch(&mut tx, &TrafficMessage::Audit).unwrap().unwrap();
        }
        assert!(sacch(&mut tx, &TrafficMessage::Audit).unwrap().is_none());
        assert!(matches!(
            sacch(&mut tx, &TrafficMessage::Connect),
            Err(TxError::SacchMessage(_))
        ));
        let prepared = tx.prepare(0, SyncWord::Two).unwrap().clone();
        tx.submitted(&prepared).unwrap();
        assert!(sacch(&mut tx, &TrafficMessage::Audit).unwrap().is_some());
    }
}
