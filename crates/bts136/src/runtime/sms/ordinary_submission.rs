use super::*;
use crate::core::{messaging::MessagingError, store::StoreError};
pub(super) struct OrdinaryResult {
    pub response: RelayMessage,
    pub until_ms: u64,
}
impl SmsRuntime {
    pub(super) fn submit_ordinary_sms(
        &mut self,
        sender: MobileIdentity,
        message: &RelayMessage,
        optional_octets: &[u8],
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<OrdinaryResult, Failure> {
        let RelayMessage::Data {
            transaction_id,
            payload,
            protocol: relay::HigherLayerProtocol::Sms,
        } = message
        else {
            return Err(Failure::Wire);
        };
        if !self.incoming_started() {
            self.start_incoming_sms(now_ms, unix_ms)
                .map_err(|error| Failure::Incoming(Box::new(error)))?;
        }
        let unix_ms = self.incoming_observed_unix(now_ms, unix_ms)?;
        let guard_ms = self.submission_retry_guard_ms;
        let mut bytes = Vec::with_capacity(payload.len() + 1);
        bytes.push(*transaction_id);
        bytes.extend_from_slice(payload);
        let request = self
            .core_mut()
            .store_mut()
            .claim_ordinary_request(sender, &bytes, optional_octets, unix_ms, guard_ms)
            .map_err(Failure::Store)?;
        let stored = self
            .core()
            .store()
            .ordinary_request_sms(&request)
            .map_err(Failure::Store)?;
        let response = if let Some(bytes) = &request.response {
            let decoded =
                relay::decode_prefix(bytes, Direction::Forward).map_err(|_| Failure::Wire)?;
            match decoded.message {
                RelayMessage::Accept { transaction_id: id }
                    if id == *transaction_id && stored.is_some() => {}
                RelayMessage::Reject {
                    transaction_id: id, ..
                } if id == *transaction_id && stored.is_none() => {}
                _ => return Err(Failure::Store(StoreError::CorruptState)),
            }
            if decoded
                .message
                .encode(Direction::Forward)
                .map_err(|_| Failure::Wire)?
                != *bytes
            {
                return Err(Failure::Store(StoreError::CorruptState));
            }
            decoded.message
        } else if stored.is_some() {
            tracing::info!(?sender, "Recovered committed ordinary SMS acceptance");
            RelayMessage::Accept {
                transaction_id: *transaction_id,
            }
        } else {
            let submission = relay::addressing::decode_fields(message.clone(), optional_octets)
                .map_err(MessagingError::Addressing)
                .and_then(|decoded| {
                    self.core_mut().submit_addressed_sms(
                        sender,
                        decoded,
                        &request.submission_key(),
                        request.observed_at_unix_ms,
                    )
                });
            match submission {
                Ok(result) => {
                    tracing::info!(?sender, id=?result.id, "SMS submission stored");
                    result.response
                }
                Err(MessagingError::Store(error)) if !permanent_store_rejection(&error) => {
                    return Err(Failure::Store(error));
                }
                Err(error) => {
                    tracing::warn!(%error, ?sender, "SMS submission rejected");
                    RelayMessage::Reject {
                        transaction_id: *transaction_id,
                        cause: rejection_cause(&error),
                    }
                }
            }
        };
        self.core_mut()
            .store_mut()
            .settle_ordinary_request(
                &request,
                &response
                    .encode(Direction::Forward)
                    .map_err(|_| Failure::Wire)?,
                unix_ms,
            )
            .map_err(Failure::Store)?;
        let remaining = request
            .expires_at_unix_ms
            .checked_sub(unix_ms)
            .and_then(|remaining| u64::try_from(remaining).ok())
            .ok_or(Failure::Clock)?;
        Ok(OrdinaryResult {
            response,
            until_ms: now_ms.checked_add(remaining).ok_or(Failure::Clock)?,
        })
    }
}

fn permanent_store_rejection(error: &StoreError) -> bool {
    if let StoreError::Hub(error) = error {
        return !matches!(error, crate::core::store::sms_hub::HubError::QueueFull);
    }
    matches!(
        error,
        StoreError::Traffic(_)
            | StoreError::Sms(_)
            | StoreError::Addressing(_)
            | StoreError::Registry(_)
            | StoreError::MissingSubscriber
            | StoreError::DisabledSubscriber
            | StoreError::MessageType
            | StoreError::SenderMismatch
            | StoreError::RecipientMismatch
    )
}
