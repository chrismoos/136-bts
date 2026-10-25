use crate::{
    core::store::{
        StateStore, StoreError,
        incoming_journal::{IncomingJournal, JournalEvent, JournalHandle},
    },
    l3::relay::HigherLayerProtocol,
    link::{
        rach::MobileIdentity,
        tsar::{
            ApplicationOutcome, IncomingError, IncomingLimits, IncomingTransaction,
            IncomingTransactions, RecoveredBank, RecoveredIncoming, RecoveryContext, RecoveryEvent,
            RecoveryObservation,
        },
    },
};
use std::{error::Error as StdError, fmt};

#[derive(Debug)]
pub enum Error {
    Store(StoreError),
    Ownership(IncomingError),
    ClockRange,
    Retired,
    Binding,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "incoming TSAR recovery failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug, Clone, Copy)]
pub struct RecoveryClock {
    pub now_monotonic_ms: u64,
    pub now_unix_ms: i64,
}
impl RecoveryClock {
    fn map(self, unix_ms: i64) -> Result<u64, Error> {
        if unix_ms < 0 || unix_ms > self.now_unix_ms {
            return Err(Error::ClockRange);
        }
        let age = self
            .now_unix_ms
            .checked_sub(unix_ms)
            .ok_or(Error::ClockRange)?;
        self.now_monotonic_ms
            .checked_sub(age as u64)
            .ok_or(Error::ClockRange)
    }
}
pub struct JournalTransaction {
    pub journal: IncomingJournal,
    pub transaction: IncomingTransaction,
}
pub struct RestoredIncoming {
    pub bank: IncomingTransactions,
    pub transactions: Vec<JournalTransaction>,
}
/// Establish the downtime/clock mapping before recovery. An epoch too recent to represent retained history rejects instead of restarting protocol timers.
pub fn restore(
    store: &StateStore,
    peer: MobileIdentity,
    protocol: HigherLayerProtocol,
    limits: IncomingLimits,
    reuse_guard_ms: u64,
    clock: RecoveryClock,
    handles: &[JournalHandle],
) -> Result<RestoredIncoming, Error> {
    if clock.now_unix_ms < 0 {
        return Err(Error::ClockRange);
    }
    if handles.len() > IncomingTransactions::TRANSACTION_CAPACITY {
        return Err(Error::Ownership(IncomingError::Capacity));
    }
    let mut journals = Vec::new();
    let mut recovered = Vec::new();
    for handle in handles {
        let snapshot = store.incoming_snapshot(handle).map_err(Error::Store)?;
        let journal = snapshot.journal;
        if journal.reuse_after_unix_ms.is_some() {
            return Err(Error::Retired);
        }
        if journal.binding.peer != peer || journal.binding.protocol != protocol {
            return Err(Error::Binding);
        }
        let context = RecoveryContext {
            peer,
            protocol,
            transaction_id: journal.binding.transaction_id,
            version: journal.binding.version,
            maximum_message_octets: limits.maximum_message_octets,
            created_monotonic_ms: clock.map(journal.created_at_unix_ms)?,
        };
        let observations = snapshot
            .events
            .into_iter()
            .map(|observation| {
                let event = match observation.event {
                    JournalEvent::Segment(segment) => RecoveryEvent::Segment(segment),
                    JournalEvent::Advance => RecoveryEvent::Advance,
                    JournalEvent::Cancel => RecoveryEvent::Cancel,
                    JournalEvent::ApplicationCommitted(_) => {
                        RecoveryEvent::ApplicationSettled(ApplicationOutcome::Committed)
                    }
                    JournalEvent::ApplicationRejected => {
                        RecoveryEvent::ApplicationSettled(ApplicationOutcome::Rejected)
                    }
                };
                Ok(RecoveryObservation {
                    monotonic_ms: clock.map(observation.observed_at_unix_ms)?,
                    event,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        recovered.push(RecoveredIncoming::replay(context, observations).map_err(Error::Ownership)?);
        journals.push(journal);
    }
    let RecoveredBank { bank, transactions } = IncomingTransactions::recover(
        peer,
        protocol,
        limits,
        reuse_guard_ms,
        clock.now_monotonic_ms,
        recovered,
    )
    .map_err(Error::Ownership)?;
    let transactions = journals
        .into_iter()
        .map(|journal| {
            let transaction = transactions
                .iter()
                .find(|transaction| transaction.transaction_id() == journal.binding.transaction_id)
                .cloned()
                .ok_or(Error::Ownership(IncomingError::StaleTransaction))?;
            Ok(JournalTransaction {
                journal,
                transaction,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(RestoredIncoming { bank, transactions })
}
