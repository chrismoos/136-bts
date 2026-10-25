use super::{
    messaging::DeliveryOffer,
    relay_exchange::{ExchangeError, RelayExchange, RelayOutcome},
    relay_ids::{IdentifierError, RelayIdentifier, RelayIdentifiers},
    store::StateStore,
};
use crate::{l3::relay::addressing::AddressedRelay, link::rach::MobileIdentity};
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptState {
    Prepared,
    AwaitingResponse { deadline_monotonic_ms: u64 },
    Completed(RelayOutcome),
    Expired,
    Cancelled,
}
#[derive(Debug)]
pub enum AttemptError {
    Identifier(IdentifierError),
    Exchange(ExchangeError),
    Recipient,
    ClockRegression,
    TimeoutDuration,
    ReceiptTime,
    TimeOverflow,
    NotTransmitted,
    Closed,
    AlreadyTransmitted,
}
impl fmt::Display for AttemptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SMS relay attempt failed: {self:?}")
    }
}
impl Error for AttemptError {}
#[derive(Debug)]
pub struct RelayAttempt {
    exchange: RelayExchange,
    identifier: RelayIdentifier,
    state: AttemptState,
    last_monotonic_ms: u64,
    prepared_monotonic_ms: u64,
}
impl RelayAttempt {
    /// Cancel or reach a terminal state before dropping, otherwise the identifier stays reserved.
    pub fn try_new(
        offer: DeliveryOffer,
        identifiers: &mut RelayIdentifiers,
        monotonic_ms: u64,
    ) -> Result<Self, AttemptError> {
        if offer.recipient != identifiers.recipient() {
            return Err(AttemptError::Recipient);
        }
        let identifier = identifiers
            .reserve(monotonic_ms)
            .map_err(AttemptError::Identifier)?;
        let exchange = RelayExchange::new(offer, identifier.value());
        Ok(Self {
            exchange,
            identifier,
            state: AttemptState::Prepared,
            last_monotonic_ms: monotonic_ms,
            prepared_monotonic_ms: monotonic_ms,
        })
    }
    pub fn relay(&self) -> AddressedRelay {
        self.exchange.relay()
    }
    pub fn state(&self) -> AttemptState {
        self.state
    }
    /// Call when the final forward burst has been transmitted, not when it is queued.
    pub fn transmitted(
        &mut self,
        monotonic_ms: u64,
        response_timeout_ms: u64,
    ) -> Result<(), AttemptError> {
        self.transmitted_at(monotonic_ms, monotonic_ms, response_timeout_ms)
    }
    /// Completion time and callback observation use the same monotonic epoch. Observation delay must not extend the response interval.
    pub fn transmitted_at(
        &mut self,
        completed_monotonic_ms: u64,
        observed_monotonic_ms: u64,
        response_timeout_ms: u64,
    ) -> Result<(), AttemptError> {
        self.check_clock(observed_monotonic_ms)?;
        if self.state != AttemptState::Prepared {
            return Err(AttemptError::AlreadyTransmitted);
        }
        if completed_monotonic_ms < self.prepared_monotonic_ms
            || completed_monotonic_ms > observed_monotonic_ms
        {
            return Err(AttemptError::ReceiptTime);
        }
        if response_timeout_ms == 0 {
            return Err(AttemptError::TimeoutDuration);
        }
        let deadline_monotonic_ms = completed_monotonic_ms
            .checked_add(response_timeout_ms)
            .ok_or(AttemptError::TimeOverflow)?;
        self.state = AttemptState::AwaitingResponse {
            deadline_monotonic_ms,
        };
        self.last_monotonic_ms = observed_monotonic_ms;
        Ok(())
    }
    pub fn advance(
        &mut self,
        identifiers: &mut RelayIdentifiers,
        monotonic_ms: u64,
    ) -> Result<AttemptState, AttemptError> {
        self.check_clock(monotonic_ms)?;
        if matches!(
            self.state,
            AttemptState::Prepared | AttemptState::AwaitingResponse { .. }
        ) {
            identifiers
                .retirement_deadline(&self.identifier, monotonic_ms)
                .map_err(AttemptError::Identifier)?;
        }
        if matches!(self.state, AttemptState::AwaitingResponse { deadline_monotonic_ms } if monotonic_ms >= deadline_monotonic_ms)
        {
            identifiers
                .retire(&self.identifier, monotonic_ms)
                .map_err(AttemptError::Identifier)?;
            self.state = AttemptState::Expired;
        }
        self.last_monotonic_ms = monotonic_ms;
        Ok(self.state)
    }
    pub fn receive(
        &mut self,
        store: &mut StateStore,
        identifiers: &mut RelayIdentifiers,
        recipient: MobileIdentity,
        bytes: &[u8],
        monotonic_ms: u64,
    ) -> Result<RelayOutcome, AttemptError> {
        match self.advance(identifiers, monotonic_ms)? {
            AttemptState::Prepared => return Err(AttemptError::NotTransmitted),
            AttemptState::AwaitingResponse { .. } => {}
            _ => return Err(AttemptError::Closed),
        }
        self.receive_ready(store, identifiers, recipient, bytes, monotonic_ms)
    }
    /// The enclosing delivery must have submitted every frame before accepting a reply in Prepared.
    pub(crate) fn receive_submitted(
        &mut self,
        store: &mut StateStore,
        identifiers: &mut RelayIdentifiers,
        recipient: MobileIdentity,
        bytes: &[u8],
        monotonic_ms: u64,
    ) -> Result<RelayOutcome, AttemptError> {
        match self.advance(identifiers, monotonic_ms)? {
            AttemptState::Prepared | AttemptState::AwaitingResponse { .. } => {}
            _ => return Err(AttemptError::Closed),
        }
        self.receive_ready(store, identifiers, recipient, bytes, monotonic_ms)
    }
    fn receive_ready(
        &mut self,
        store: &mut StateStore,
        identifiers: &mut RelayIdentifiers,
        recipient: MobileIdentity,
        bytes: &[u8],
        monotonic_ms: u64,
    ) -> Result<RelayOutcome, AttemptError> {
        let outcome = self
            .exchange
            .receive(store, recipient, bytes)
            .map_err(AttemptError::Exchange)?;
        // Retirement is validated before the durable acceptance, while this call owns the bank.
        identifiers
            .retire(&self.identifier, monotonic_ms)
            .map_err(AttemptError::Identifier)?;
        self.state = AttemptState::Completed(outcome);
        Ok(outcome)
    }
    pub fn cancel(
        &mut self,
        identifiers: &mut RelayIdentifiers,
        monotonic_ms: u64,
    ) -> Result<(), AttemptError> {
        self.check_clock(monotonic_ms)?;
        if !matches!(
            self.state,
            AttemptState::Prepared | AttemptState::AwaitingResponse { .. }
        ) {
            return Err(AttemptError::Closed);
        }
        identifiers
            .retire(&self.identifier, monotonic_ms)
            .map_err(AttemptError::Identifier)?;
        self.state = AttemptState::Cancelled;
        self.last_monotonic_ms = monotonic_ms;
        Ok(())
    }
    fn check_clock(&self, monotonic_ms: u64) -> Result<(), AttemptError> {
        if monotonic_ms < self.last_monotonic_ms {
            return Err(AttemptError::ClockRegression);
        }
        Ok(())
    }
}
