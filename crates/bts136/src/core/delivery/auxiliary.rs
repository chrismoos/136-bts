use super::*;
use crate::{
    core::relay_ids::RelayIdentifier,
    l3::{
        legacy_traffic::TrafficRelay,
        relay::{self, Direction, RelayMessage},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxiliaryTransport {
    Control,
    Traffic,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxiliaryState {
    Prepared,
    Submitted,
    AwaitingResponse { deadline_monotonic_ms: u64 },
    Accepted,
    Rejected { cause: u8 },
    Expired,
    Canceled,
}
impl AuxiliaryState {
    pub(crate) fn closed(self) -> bool {
        matches!(
            self,
            Self::Accepted | Self::Rejected { .. } | Self::Expired | Self::Canceled
        )
    }
}
#[derive(Debug, Clone)]
pub struct AuxiliaryPacket {
    recipient: MobileIdentity,
    relay: AddressedRelay,
    transport: AuxiliaryTransport,
    owner: Arc<()>,
}
impl PartialEq for AuxiliaryPacket {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
    }
}
impl Eq for AuxiliaryPacket {}
impl AuxiliaryPacket {
    pub fn recipient(&self) -> MobileIdentity {
        self.recipient
    }
    pub fn relay(&self) -> &AddressedRelay {
        &self.relay
    }
    pub fn transport(&self) -> AuxiliaryTransport {
        self.transport
    }
    pub fn transaction_id(&self) -> u8 {
        let RelayMessage::Data { transaction_id, .. } = self.relay.message else {
            unreachable!("auxiliary packet is validated R-DATA")
        };
        transaction_id
    }
}
#[derive(Debug, Clone)]
pub struct AuxiliaryReply {
    pub packet: AuxiliaryPacket,
    pub state: AuxiliaryState,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxiliaryError {
    MessageType,
    Wire,
    StalePacket,
    NotSubmitted,
    ReceiptTime,
    AlreadySubmitted,
    AlreadyTransmitted,
    Closed,
    NotTerminal,
    AlreadyRetired,
    TimeoutDuration,
    TimeOverflow,
    ConflictingResponse,
}
#[derive(Debug)]
pub(super) struct AuxiliaryTransfer {
    packet: AuxiliaryPacket,
    identifier: RelayIdentifier,
    state: AuxiliaryState,
    reuse_after_monotonic_ms: Option<u64>,
    prepared_monotonic_ms: u64,
    submitted_monotonic_ms: Option<u64>,
}
impl AuxiliaryTransfer {
    pub(super) fn retained_at(&self, now: u64) -> bool {
        self.reuse_after_monotonic_ms
            .is_none_or(|until| now < until)
    }
    fn retire(
        &mut self,
        identifiers: &mut RelayIdentifiers,
        state: AuxiliaryState,
        now: u64,
    ) -> Result<(), DeliveryError> {
        let until = identifiers
            .retirement_deadline(&self.identifier, now)
            .map_err(DeliveryError::Identifier)?;
        identifiers
            .retire(&self.identifier, now)
            .map_err(DeliveryError::Identifier)?;
        self.state = state;
        self.reuse_after_monotonic_ms = Some(until);
        Ok(())
    }
    fn advance(&mut self, now: u64) -> AuxiliaryState {
        if matches!(self.state, AuxiliaryState::AwaitingResponse { deadline_monotonic_ms } if now >= deadline_monotonic_ms)
        {
            self.state = AuxiliaryState::Expired;
        }
        self.state
    }
}
impl SmsDelivery {
    pub fn prepare_auxiliary(
        &mut self,
        recipient: MobileIdentity,
        mut relay: AddressedRelay,
        transport: AuxiliaryTransport,
        now: u64,
    ) -> Result<AuxiliaryPacket, DeliveryError> {
        self.observe_clock(now)?;
        let RelayMessage::Data { transaction_id, .. } = &mut relay.message else {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::MessageType));
        };
        *transaction_id = 0;
        let valid = match transport {
            AuxiliaryTransport::Control => relay.encode(Direction::Forward).is_ok(),
            AuxiliaryTransport::Traffic => TrafficRelay::from_addressed(&relay).is_ok(),
        };
        if !valid {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::Wire));
        }
        let peer = self
            .recipients
            .iter_mut()
            .find(|peer| peer.identity == recipient)
            .ok_or(DeliveryError::UnknownRecipient)?;
        let identifier = peer
            .identifiers
            .reserve(now)
            .map_err(DeliveryError::Identifier)?;
        let RelayMessage::Data { transaction_id, .. } = &mut relay.message else {
            unreachable!("validated R-DATA")
        };
        *transaction_id = identifier.value();
        let packet = AuxiliaryPacket {
            recipient,
            relay,
            transport,
            owner: Arc::new(()),
        };
        peer.auxiliary.push(AuxiliaryTransfer {
            packet: packet.clone(),
            identifier,
            state: AuxiliaryState::Prepared,
            reuse_after_monotonic_ms: None,
            prepared_monotonic_ms: now,
            submitted_monotonic_ms: None,
        });
        Ok(packet)
    }
    fn auxiliary(&self, packet: &AuxiliaryPacket) -> Result<&AuxiliaryTransfer, DeliveryError> {
        self.recipients
            .iter()
            .filter(|peer| peer.identity == packet.recipient)
            .flat_map(|peer| &peer.auxiliary)
            .find(|transfer| Arc::ptr_eq(&transfer.packet.owner, &packet.owner))
            .ok_or(DeliveryError::Auxiliary(AuxiliaryError::StalePacket))
    }
    fn auxiliary_mut(
        &mut self,
        packet: &AuxiliaryPacket,
    ) -> Result<(&mut AuxiliaryTransfer, &mut RelayIdentifiers), DeliveryError> {
        let peer = self
            .recipients
            .iter_mut()
            .find(|peer| peer.identity == packet.recipient)
            .ok_or(DeliveryError::Auxiliary(AuxiliaryError::StalePacket))?;
        let transfer = peer
            .auxiliary
            .iter_mut()
            .find(|transfer| Arc::ptr_eq(&transfer.packet.owner, &packet.owner))
            .ok_or(DeliveryError::Auxiliary(AuxiliaryError::StalePacket))?;
        Ok((transfer, &mut peer.identifiers))
    }
    pub fn auxiliary_state(
        &self,
        packet: &AuxiliaryPacket,
    ) -> Result<AuxiliaryState, DeliveryError> {
        Ok(self.auxiliary(packet)?.state)
    }
    /// Supply actual all-frame handoff separately from observation time. Handoff does not start the RF response timer.
    pub fn auxiliary_submitted(
        &mut self,
        packet: &AuxiliaryPacket,
        queued_monotonic_ms: u64,
        observed_monotonic_ms: u64,
    ) -> Result<(), DeliveryError> {
        self.observe_clock(observed_monotonic_ms)?;
        let (transfer, _) = self.auxiliary_mut(packet)?;
        if transfer.state != AuxiliaryState::Prepared {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::AlreadySubmitted));
        }
        if queued_monotonic_ms < transfer.prepared_monotonic_ms
            || queued_monotonic_ms > observed_monotonic_ms
        {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::ReceiptTime));
        }
        transfer.state = AuxiliaryState::Submitted;
        transfer.submitted_monotonic_ms = Some(queued_monotonic_ms);
        Ok(())
    }
    /// Supply actual final-burst completion for this packet and the response interval required by its transport.
    pub fn auxiliary_transmitted(
        &mut self,
        packet: &AuxiliaryPacket,
        completed_monotonic_ms: u64,
        observed_monotonic_ms: u64,
        response_timeout_ms: u64,
    ) -> Result<AuxiliaryState, DeliveryError> {
        self.observe_clock(observed_monotonic_ms)?;
        let (transfer, _) = self.auxiliary_mut(packet)?;
        match transfer.state {
            AuxiliaryState::Prepared => {
                return Err(DeliveryError::Auxiliary(AuxiliaryError::NotSubmitted));
            }
            AuxiliaryState::Submitted => {}
            state if state.closed() => {
                return Err(DeliveryError::Auxiliary(AuxiliaryError::Closed));
            }
            _ => return Err(DeliveryError::Auxiliary(AuxiliaryError::AlreadyTransmitted)),
        }
        if !transfer
            .submitted_monotonic_ms
            .is_some_and(|queued| completed_monotonic_ms >= queued)
            || completed_monotonic_ms > observed_monotonic_ms
        {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::ReceiptTime));
        }
        if response_timeout_ms == 0 {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::TimeoutDuration));
        }
        let deadline_monotonic_ms = completed_monotonic_ms
            .checked_add(response_timeout_ms)
            .ok_or(DeliveryError::Auxiliary(AuxiliaryError::TimeOverflow))?;
        transfer.state = AuxiliaryState::AwaitingResponse {
            deadline_monotonic_ms,
        };
        Ok(transfer.advance(observed_monotonic_ms))
    }
    pub fn advance_auxiliary(
        &mut self,
        packet: &AuxiliaryPacket,
        now: u64,
    ) -> Result<AuxiliaryState, DeliveryError> {
        self.observe_clock(now)?;
        let (transfer, _) = self.auxiliary_mut(packet)?;
        Ok(transfer.advance(now))
    }
    /// Fence all queued/in-flight work for this packet before cancellation. Dropping the packet leaves its ID reserved.
    pub fn cancel_auxiliary_fenced(
        &mut self,
        packet: &AuxiliaryPacket,
        now: u64,
    ) -> Result<(), DeliveryError> {
        self.observe_clock(now)?;
        let (transfer, identifiers) = self.auxiliary_mut(packet)?;
        if transfer.state.closed() {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::Closed));
        }
        transfer.retire(identifiers, AuxiliaryState::Canceled, now)
    }
    /// Withdraw the source and wait for its radio queue fence before starting ID reuse protection. Terminal protocol status alone is not that fence.
    pub fn retire_auxiliary_fenced(
        &mut self,
        packet: &AuxiliaryPacket,
        now: u64,
    ) -> Result<(), DeliveryError> {
        self.observe_clock(now)?;
        let (transfer, identifiers) = self.auxiliary_mut(packet)?;
        if !transfer.state.closed() {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::NotTerminal));
        }
        if transfer.reuse_after_monotonic_ms.is_some() {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::AlreadyRetired));
        }
        transfer.retire(identifiers, transfer.state, now)
    }
    /// None leaves an unmatched response for another dispatcher. A match never changes SMS queue records.
    pub fn receive_auxiliary(
        &mut self,
        recipient: MobileIdentity,
        bytes: &[u8],
        now: u64,
    ) -> Result<Option<AuxiliaryReply>, DeliveryError> {
        self.observe_clock(now)?;
        let decoded = relay::decode_prefix(bytes, Direction::Reverse)
            .map_err(|_| DeliveryError::Auxiliary(AuxiliaryError::Wire))?;
        let (id, outcome) = match decoded.message {
            RelayMessage::Accept { transaction_id } => (transaction_id, AuxiliaryState::Accepted),
            RelayMessage::Reject {
                transaction_id,
                cause,
            } => (transaction_id, AuxiliaryState::Rejected { cause }),
            _ => return Err(DeliveryError::Auxiliary(AuxiliaryError::MessageType)),
        };
        let Some(peer) = self
            .recipients
            .iter_mut()
            .find(|peer| peer.identity == recipient)
        else {
            return Ok(None);
        };
        let Some(transfer) = peer
            .auxiliary
            .iter_mut()
            .find(|transfer| transfer.identifier.value() == id)
        else {
            return Ok(None);
        };
        if !decoded.optional_octets.is_empty() {
            return Err(DeliveryError::Auxiliary(AuxiliaryError::Wire));
        }
        let state = transfer.advance(now);
        match state {
            AuxiliaryState::Prepared => {
                return Err(DeliveryError::Auxiliary(AuxiliaryError::NotSubmitted));
            }
            AuxiliaryState::Submitted | AuxiliaryState::AwaitingResponse { .. } => {
                transfer.state = outcome
            }
            AuxiliaryState::Accepted | AuxiliaryState::Rejected { .. } if state != outcome => {
                return Err(DeliveryError::Auxiliary(
                    AuxiliaryError::ConflictingResponse,
                ));
            }
            _ => {}
        }
        Ok(Some(AuxiliaryReply {
            packet: transfer.packet.clone(),
            state: transfer.state,
        }))
    }
}
