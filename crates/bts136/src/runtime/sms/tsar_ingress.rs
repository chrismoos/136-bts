use super::tsar_recovery::{self, RecoveryClock, RestoredIncoming};
use crate::{
    core::store::{
        StateStore, StoreError,
        incoming_journal::{JournalEvent, JournalHandle},
    },
    l3::{
        relay::HigherLayerProtocol,
        tsar::{DecodedSegment, Response},
    },
    link::{
        rach::MobileIdentity,
        tsar::{ApplicationOutcome, IncomingError, IncomingLimits},
    },
};
use std::{error::Error as StdError, fmt};

pub struct Transition<'a> {
    pub peer: MobileIdentity,
    pub protocol: HigherLayerProtocol,
    pub limits: IncomingLimits,
    pub reuse_guard_ms: u64,
    pub clock: RecoveryClock,
    pub target: &'a JournalHandle,
    pub expected_revision: u32,
    pub event: JournalEvent,
}
pub struct PersistedTransition {
    pub incoming: RestoredIncoming,
    /// Use the durable response outbox for transmission. Final responses remain held there until application settlement.
    pub response: Option<Response>,
}
#[derive(Debug)]
pub enum Error {
    Recovery(tsar_recovery::Error),
    Ownership(IncomingError),
    Store(StoreError),
    Target,
    Revision,
    NoPendingMessage,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "incoming TSAR transition failed: {self:?}")
    }
}
impl StdError for Error {}

/// Serialize writers for this peer/service bank. Publish state only after success and transmit responses through the application-gated durable outbox.
pub fn apply(
    store: &mut StateStore,
    transition: Transition<'_>,
) -> Result<PersistedTransition, Error> {
    let handles: Vec<_> = store
        .incoming_bank_journals(transition.peer, transition.protocol)
        .map_err(Error::Store)?
        .into_iter()
        .map(|journal| journal.handle)
        .collect();
    let mut incoming = tsar_recovery::restore(
        store,
        transition.peer,
        transition.protocol,
        transition.limits,
        transition.reuse_guard_ms,
        transition.clock,
        &handles,
    )
    .map_err(Error::Recovery)?;
    let target = incoming
        .transactions
        .iter_mut()
        .find(|entry| entry.journal.handle == *transition.target)
        .ok_or(Error::Target)?;
    if target.journal.revision != transition.expected_revision {
        return Err(Error::Revision);
    }
    let now = transition.clock.now_monotonic_ms;
    let response = match &transition.event {
        JournalEvent::Segment(segment) => {
            incoming
                .bank
                .receive(
                    &target.transaction,
                    transition.peer,
                    transition.protocol,
                    &DecodedSegment {
                        segment: segment.clone(),
                        unparsed_octets: Vec::new(),
                    },
                    now,
                )
                .map_err(Error::Ownership)?
                .response
        }
        JournalEvent::Advance => incoming
            .bank
            .advance(&target.transaction, now)
            .map_err(Error::Ownership)?,
        JournalEvent::Cancel => {
            incoming
                .bank
                .cancel(&target.transaction, now)
                .map_err(Error::Ownership)?;
            None
        }
        JournalEvent::ApplicationCommitted(_) | JournalEvent::ApplicationRejected => {
            let message = incoming
                .bank
                .pending_message(&target.transaction)
                .map_err(Error::Ownership)?
                .ok_or(Error::NoPendingMessage)?
                .clone();
            let outcome = match transition.event {
                JournalEvent::ApplicationCommitted(_) => ApplicationOutcome::Committed,
                _ => ApplicationOutcome::Rejected,
            };
            incoming
                .bank
                .settle_application(&target.transaction, &message, outcome, now)
                .map_err(Error::Ownership)?;
            None
        }
    };
    let receipt = store
        .append_incoming_transition(
            transition.target,
            transition.expected_revision,
            &transition.event,
            transition.clock.now_unix_ms,
            response.as_ref(),
        )
        .map_err(Error::Store)?;
    target.journal.revision = receipt.revision;
    target.journal.observed_at_unix_ms = transition.clock.now_unix_ms;
    Ok(PersistedTransition { incoming, response })
}
