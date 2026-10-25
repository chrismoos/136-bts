use super::*;
use crate::{
    core::store::incoming_journal::{ResponseHandle, ResponseOutcome},
    l3::relay::addressing::{AddressedRelay, RelayAddresses},
};

pub(super) struct IncomingResponseJob {
    handle: ResponseHandle,
    packet: AuxiliaryPacket,
}
impl SmsRuntime {
    pub(super) fn incoming_response_pending(
        &self,
        handle: &crate::core::store::incoming_journal::JournalHandle,
    ) -> bool {
        self.incoming_response_jobs
            .iter()
            .any(|job| job.handle.journal() == handle)
    }

    pub(super) fn owns_incoming_response(&self, packet: &AuxiliaryPacket) -> bool {
        self.incoming_response_jobs
            .iter()
            .any(|job| job.packet == *packet)
    }
    pub(crate) fn settle_incoming_responses(
        &mut self,
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<(), Failure> {
        let unix_ms = self.incoming_observed_unix(now_ms, unix_ms)?;
        let mut index = 0;
        while index < self.incoming_response_jobs.len() {
            let job = &self.incoming_response_jobs[index];
            let Some(state) = self.auxiliary.outcome(&job.packet) else {
                index += 1;
                continue;
            };
            let outcome = match state {
                AuxiliaryState::Accepted => Some(ResponseOutcome::Accepted),
                AuxiliaryState::Rejected { .. } => Some(ResponseOutcome::Rejected),
                AuxiliaryState::Expired => Some(ResponseOutcome::Expired),
                AuxiliaryState::Canceled => None,
                _ => return Err(Failure::Wire),
            };
            if let Some(outcome) = outcome {
                self.delivery
                    .core_mut()
                    .store_mut()
                    .settle_incoming_response(&job.handle, outcome, unix_ms)
                    .map_err(Failure::Store)?;
            }
            self.auxiliary.take_outcome(&job.packet);
            self.incoming_response_jobs.remove(index);
        }
        Ok(())
    }
    pub(crate) fn dispatch_incoming_responses(
        &mut self,
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<(), Failure> {
        self.settle_incoming_responses(now_ms, unix_ms)?;
        if now_ms < self.next_incoming_response_scan_ms {
            return Ok(());
        }
        self.next_incoming_response_scan_ms = now_ms
            .checked_add(RETRY_INTERVAL_MS)
            .ok_or(Failure::Clock)?;
        let pending = self
            .core()
            .store()
            .pending_incoming_responses(MAX_SUBMISSION_HISTORY as u32)
            .map_err(Failure::Store)?;
        for response in pending {
            if self.incoming_response_jobs.len() >= MAX_PENDING {
                break;
            }
            if self
                .incoming_response_jobs
                .iter()
                .any(|job| job.handle == response.handle)
            {
                continue;
            }
            let common_version = self
                .core()
                .registry()
                .subscribers()
                .iter()
                .find(|subscriber| {
                    subscriber.provisioning.identity == response.journal.binding.peer
                })
                .and_then(|subscriber| subscriber.registration)
                .map(|registration| {
                    crate::runtime::capabilities::SERVING_PROTOCOL
                        .common(registration.protocol_version)
                });
            if common_version
                .is_none_or(|version| version.code() < response.journal.binding.version.code())
            {
                continue;
            }
            let payload = match response.response.encode(response.journal.binding.version) {
                Ok(payload) => payload,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        peer = %response.journal.binding.peer,
                        "incoming SMS response cannot be encoded for the phone's protocol version, rejecting it"
                    );
                    let observed_unix_ms = self.incoming_observed_unix(now_ms, unix_ms)?;
                    self.delivery
                        .core_mut()
                        .store_mut()
                        .settle_incoming_response(
                            &response.handle,
                            ResponseOutcome::Rejected,
                            observed_unix_ms,
                        )
                        .map_err(Failure::Store)?;
                    continue;
                }
            };
            let relay = AddressedRelay {
                message: RelayMessage::Data {
                    transaction_id: 0,
                    protocol: response.journal.binding.protocol,
                    payload,
                },
                addresses: RelayAddresses::default(),
            };
            match self.send_auxiliary(response.journal.binding.peer, relay, now_ms) {
                Ok(packet) => self.incoming_response_jobs.push(IncomingResponseJob {
                    handle: response.handle,
                    packet,
                }),
                Err(
                    Failure::QueueFull
                    | Failure::Delivery(
                        DeliveryError::RecipientOffline
                        | DeliveryError::UnknownRecipient
                        | DeliveryError::Busy
                        | DeliveryError::Identifier(IdentifierError::Exhausted),
                    ),
                ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
