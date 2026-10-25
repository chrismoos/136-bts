use crate::{
    core::messaging::{
        BoundSmsSubmission, CompleteSmsSubmission, LocalMessaging, MessagingError, SmsDestination,
    },
    l3::{
        address::AddressInfo,
        relay::{HigherLayerProtocol, Teleservice},
    },
    link::tsar::{ApplicationOutcome, IncomingError, IncomingTransaction, IncomingTransactions},
};
use std::{error::Error as StdError, fmt};

#[derive(Debug)]
pub enum Error {
    Ownership(IncomingError),
    NoPendingMessage,
    Protocol,
    Messaging(MessagingError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "reassembled SMS submission failed: {self:?}")
    }
}
impl StdError for Error {}
pub struct SubmissionContext<'a> {
    pub destination: &'a AddressInfo,
    pub submission_key: &'a str,
    pub created_at_unix_ms: i64,
    pub monotonic_ms: u64,
}

pub struct BoundSubmissionContext<'a> {
    pub destination: &'a SmsDestination,
    pub submission_key: &'a str,
    pub created_at_unix_ms: i64,
    pub monotonic_ms: u64,
}

/// Preserve the admitted recipient identity and number across reassembly and retry. Settle only after its durable application commit.
pub fn commit_bound(
    core: &mut LocalMessaging,
    incoming: &mut IncomingTransactions,
    transaction: &IncomingTransaction,
    context: BoundSubmissionContext<'_>,
) -> Result<CompleteSmsSubmission, Error> {
    commit_using(
        core,
        incoming,
        transaction,
        context.monotonic_ms,
        |core, peer, bytes| {
            core.submit_bound_sms(
                peer,
                context.destination,
                bytes,
                context.submission_key,
                context.created_at_unix_ms,
            )
        },
    )
}

pub fn commit_bound_with_sender(
    core: &mut LocalMessaging,
    incoming: &mut IncomingTransactions,
    transaction: &IncomingTransaction,
    context: BoundSubmissionContext<'_>,
    sender_number: &str,
) -> Result<CompleteSmsSubmission, Error> {
    commit_using(
        core,
        incoming,
        transaction,
        context.monotonic_ms,
        |core, peer, bytes| {
            core.submit_frozen_sms(
                BoundSmsSubmission {
                    sender: peer,
                    sender_number,
                    destination: context.destination,
                },
                bytes,
                context.submission_key,
                context.created_at_unix_ms,
            )
        },
    )
}

/// Bind destination and stable key to the admitted stream. A post-commit settlement error requires retry with the same key, not a second application submission.
pub fn commit(
    core: &mut LocalMessaging,
    incoming: &mut IncomingTransactions,
    transaction: &IncomingTransaction,
    context: SubmissionContext<'_>,
) -> Result<CompleteSmsSubmission, Error> {
    commit_using(
        core,
        incoming,
        transaction,
        context.monotonic_ms,
        |core, peer, bytes| {
            core.submit_complete_sms(
                peer,
                context.destination,
                bytes,
                context.submission_key,
                context.created_at_unix_ms,
            )
        },
    )
}
fn commit_using(
    core: &mut LocalMessaging,
    incoming: &mut IncomingTransactions,
    transaction: &IncomingTransaction,
    monotonic_ms: u64,
    submit: impl FnOnce(
        &mut LocalMessaging,
        crate::link::rach::MobileIdentity,
        &[u8],
    ) -> Result<CompleteSmsSubmission, MessagingError>,
) -> Result<CompleteSmsSubmission, Error> {
    if incoming.protocol() != HigherLayerProtocol::Segmented(Teleservice::Sms) {
        return Err(Error::Protocol);
    }
    let message = incoming
        .pending_message(transaction)
        .map_err(Error::Ownership)?
        .ok_or(Error::NoPendingMessage)?
        .clone();
    let stored = submit(core, incoming.peer(), message.bytes()).map_err(Error::Messaging)?;
    incoming
        .settle_application(
            transaction,
            &message,
            ApplicationOutcome::Committed,
            monotonic_ms,
        )
        .map_err(Error::Ownership)?;
    Ok(stored)
}
