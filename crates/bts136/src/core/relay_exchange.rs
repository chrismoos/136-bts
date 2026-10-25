use super::{
    messaging::DeliveryOffer,
    relay_ids::RelayIdentifier,
    store::{QueueState, SmsId, StateStore, StoreError},
};
use crate::{
    l3::relay::{self, Direction, RelayError, RelayMessage, addressing::AddressedRelay},
    link::rach::MobileIdentity,
};
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayOutcome {
    Accepted { id: SmsId, state: QueueState },
    Rejected { id: SmsId, cause: u8 },
}
#[derive(Debug)]
pub enum ExchangeError {
    Relay(RelayError),
    Store(StoreError),
    Recipient,
    Transaction,
    MessageType,
    UnsupportedOptions,
    ConflictingResponse,
}
impl fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SMS relay exchange failed: {self:?}")
    }
}
impl Error for ExchangeError {}
#[derive(Debug)]
pub struct RelayExchange {
    offer: DeliveryOffer,
    transaction_id: u8,
    outcome: Option<RelayOutcome>,
}
impl RelayExchange {
    /// The caller must reserve this identifier for the recipient until late responses cannot match a new exchange.
    pub fn new(offer: DeliveryOffer, transaction_id: u8) -> Self {
        Self {
            offer,
            transaction_id,
            outcome: None,
        }
    }
    /// Retire the identifier only after this exchange stops accepting responses.
    pub fn from_reserved(
        offer: DeliveryOffer,
        identifier: &RelayIdentifier,
    ) -> Result<Self, ExchangeError> {
        if offer.recipient != identifier.recipient() {
            return Err(ExchangeError::Recipient);
        }
        Ok(Self::new(offer, identifier.value()))
    }
    pub fn relay(&self) -> AddressedRelay {
        self.offer.relay(self.transaction_id)
    }
    pub fn recipient(&self) -> MobileIdentity {
        self.offer.recipient
    }
    pub fn outcome(&self) -> Option<RelayOutcome> {
        self.outcome
    }
    pub fn receive(
        &mut self,
        store: &mut StateStore,
        recipient: MobileIdentity,
        bytes: &[u8],
    ) -> Result<RelayOutcome, ExchangeError> {
        if recipient != self.offer.recipient {
            return Err(ExchangeError::Recipient);
        }
        let decoded =
            relay::decode_prefix(bytes, Direction::Reverse).map_err(ExchangeError::Relay)?;
        if !decoded.optional_octets.is_empty() {
            return Err(ExchangeError::UnsupportedOptions);
        }
        let response = match decoded.message {
            RelayMessage::Accept { transaction_id } => {
                if transaction_id != self.transaction_id {
                    return Err(ExchangeError::Transaction);
                }
                None
            }
            RelayMessage::Reject {
                transaction_id,
                cause,
            } => {
                if transaction_id != self.transaction_id {
                    return Err(ExchangeError::Transaction);
                }
                Some(cause)
            }
            _ => return Err(ExchangeError::MessageType),
        };
        if let Some(outcome) = self.outcome {
            return match (outcome, response) {
                (RelayOutcome::Accepted { .. }, None) => Ok(outcome),
                (RelayOutcome::Rejected { cause, .. }, Some(repeated)) if cause == repeated => {
                    Ok(outcome)
                }
                _ => Err(ExchangeError::ConflictingResponse),
            };
        }
        let id = self.offer.queued.id;
        let outcome = match response {
            None => RelayOutcome::Accepted {
                id,
                state: store
                    .record_relay_accept(id, recipient)
                    .map_err(ExchangeError::Store)?,
            },
            Some(cause) => RelayOutcome::Rejected { id, cause },
        };
        self.outcome = Some(outcome);
        Ok(outcome)
    }
}
