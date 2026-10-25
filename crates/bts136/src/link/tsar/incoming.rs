mod recovery;
use super::{ReceiveError, Receiver, ReceiverState, Reception};
use crate::{
    l3::{
        broadcast::ProtocolVersion,
        relay::HigherLayerProtocol,
        tsar::{DecodedSegment, Response, SegmentKind},
    },
    link::rach::MobileIdentity,
};
pub use recovery::{RecoveryContext, RecoveryEvent, RecoveryObservation};
use std::{error::Error as StdError, fmt, sync::Arc};

// TIA-136-620-A §§3.1–3.4 assign three bits to the originator's transaction ID.
const TRANSACTION_COUNT: usize = 8;
const SEGMENTED_CARRIER_BITS: u8 = 0xc0;
const PAYLOAD_COPIES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingLimits {
    pub maximum_message_octets: usize,
    pub maximum_reserved_payload_octets: usize,
}
#[derive(Debug, Clone)]
pub struct IncomingTransaction {
    id: u8,
    owner: Arc<()>,
}
impl IncomingTransaction {
    pub fn transaction_id(&self) -> u8 {
        self.id
    }
}
#[derive(Debug, Clone)]
pub struct ApplicationMessage {
    owner: Arc<()>,
    bytes: Arc<[u8]>,
}
impl ApplicationMessage {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationOutcome {
    Committed,
    Rejected,
}
#[derive(Debug, Clone)]
pub struct IncomingReception {
    pub response: Option<Response>,
    pub message: Option<ApplicationMessage>,
}
#[derive(Debug, Clone)]
pub struct IncomingStart {
    pub transaction: IncomingTransaction,
    pub reception: IncomingReception,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncomingError {
    Identity,
    Service,
    Limits,
    GuardDuration,
    Guarded,
    Occupied,
    Capacity,
    TransactionId,
    StaleTransaction,
    StillActive,
    PendingApplication,
    StaleApplication,
    ClockRegression,
    TimeOverflow,
    Receiver(ReceiveError),
}
impl fmt::Display for IncomingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "incoming TSAR transaction failed: {self:?}")
    }
}
impl StdError for IncomingError {}
pub struct RecoveredIncoming {
    context: RecoveryContext,
    active: OwnedReceiver,
    last_monotonic_ms: u64,
}
pub struct RecoveredBank {
    pub bank: IncomingTransactions,
    pub transactions: Vec<IncomingTransaction>,
}
enum Application {
    None,
    Pending(ApplicationMessage),
    Settled(ApplicationOutcome),
}
struct OwnedReceiver {
    owner: Arc<()>,
    receiver: Receiver,
    application: Application,
}
enum Slot {
    Available,
    Active(Box<OwnedReceiver>),
    Guarded { until_monotonic_ms: u64 },
}

/// Keep this bank for the peer/service transport session. Wire IDs cannot distinguish old and new local generations.
pub struct IncomingTransactions {
    peer: MobileIdentity,
    protocol: HigherLayerProtocol,
    limits: IncomingLimits,
    reservation_octets: usize,
    reuse_guard_ms: u64,
    last_monotonic_ms: u64,
    slots: [Slot; TRANSACTION_COUNT],
}
impl IncomingTransactions {
    pub const TRANSACTION_CAPACITY: usize = TRANSACTION_COUNT;
    /// A fresh session must exclude all older traffic. The guard must bound the transport/peer late-message lifetime.
    pub fn try_new(
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        limits: IncomingLimits,
        reuse_guard_ms: u64,
        now: u64,
    ) -> Result<Self, IncomingError> {
        let (_, width, value) = peer.parts();
        if value == 0 || value >= 1 << width {
            return Err(IncomingError::Identity);
        }
        if !matches!(protocol, HigherLayerProtocol::Segmented(_))
            && !matches!(protocol, HigherLayerProtocol::CarrierSpecific(raw) if raw & SEGMENTED_CARRIER_BITS == SEGMENTED_CARRIER_BITS)
        {
            return Err(IncomingError::Service);
        }
        if reuse_guard_ms == 0 {
            return Err(IncomingError::GuardDuration);
        }
        // Retained fragments and a pending application message can coexist after complete reassembly.
        let reservation_octets = limits
            .maximum_message_octets
            .checked_mul(PAYLOAD_COPIES)
            .ok_or(IncomingError::Limits)?;
        if reservation_octets == 0 || reservation_octets > limits.maximum_reserved_payload_octets {
            return Err(IncomingError::Limits);
        }
        Ok(Self {
            peer,
            protocol,
            limits,
            reservation_octets,
            reuse_guard_ms,
            last_monotonic_ms: now,
            slots: std::array::from_fn(|_| Slot::Available),
        })
    }
    pub fn after_restart(
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        limits: IncomingLimits,
        reuse_guard_ms: u64,
        now: u64,
    ) -> Result<Self, IncomingError> {
        let mut bank = Self::try_new(peer, protocol, limits, reuse_guard_ms, now)?;
        let until_monotonic_ms = now
            .checked_add(reuse_guard_ms)
            .ok_or(IncomingError::TimeOverflow)?;
        bank.slots = std::array::from_fn(|_| Slot::Guarded { until_monotonic_ms });
        Ok(bank)
    }
    /// Restore only validated durable histories before admitting live input. Other wire IDs retain their full restart guard.
    pub fn recover(
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        limits: IncomingLimits,
        reuse_guard_ms: u64,
        now: u64,
        recovered: Vec<RecoveredIncoming>,
    ) -> Result<RecoveredBank, IncomingError> {
        let mut bank = Self::after_restart(peer, protocol, limits, reuse_guard_ms, now)?;
        if recovered.len() > TRANSACTION_COUNT {
            return Err(IncomingError::Capacity);
        }
        let mut transactions = Vec::new();
        for recovered in recovered {
            let context = recovered.context;
            if context.peer != peer {
                return Err(IncomingError::Identity);
            }
            if context.protocol != protocol {
                return Err(IncomingError::Service);
            }
            if context.maximum_message_octets != limits.maximum_message_octets {
                return Err(IncomingError::Limits);
            }
            if recovered.last_monotonic_ms > now {
                return Err(IncomingError::ClockRegression);
            }
            let index = usize::from(context.transaction_id);
            if matches!(bank.slots[index], Slot::Active(_)) {
                return Err(IncomingError::Occupied);
            }
            if bank.reservation_octets
                > limits.maximum_reserved_payload_octets - bank.reserved_payload_octets()
            {
                return Err(IncomingError::Capacity);
            }
            transactions.push(IncomingTransaction {
                id: context.transaction_id,
                owner: recovered.active.owner.clone(),
            });
            bank.slots[index] = Slot::Active(Box::new(recovered.active));
        }
        Ok(RecoveredBank { bank, transactions })
    }
    pub fn peer(&self) -> MobileIdentity {
        self.peer
    }
    pub fn protocol(&self) -> HigherLayerProtocol {
        self.protocol
    }
    pub fn reserved_payload_octets(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, Slot::Active(_)))
            .count()
            * self.reservation_octets
    }
    fn clock(&self, now: u64) -> Result<(), IncomingError> {
        if now < self.last_monotonic_ms {
            return Err(IncomingError::ClockRegression);
        }
        Ok(())
    }
    fn index(&self, transaction: &IncomingTransaction) -> Result<usize, IncomingError> {
        let index = usize::from(transaction.id);
        match &self.slots[index] {
            Slot::Active(active) if Arc::ptr_eq(&active.owner, &transaction.owner) => Ok(index),
            _ => Err(IncomingError::StaleTransaction),
        }
    }
    fn owned(&self, transaction: &IncomingTransaction) -> Result<&OwnedReceiver, IncomingError> {
        let index = self.index(transaction)?;
        let Slot::Active(active) = &self.slots[index] else {
            unreachable!("validated active receiver")
        };
        Ok(active)
    }
    pub fn receiver(&self, transaction: &IncomingTransaction) -> Result<&Receiver, IncomingError> {
        Ok(&self.owned(transaction)?.receiver)
    }
    pub fn pending_message(
        &self,
        transaction: &IncomingTransaction,
    ) -> Result<Option<&ApplicationMessage>, IncomingError> {
        Ok(match &self.owned(transaction)?.application {
            Application::Pending(message) => Some(message),
            _ => None,
        })
    }
    pub fn application_outcome(
        &self,
        transaction: &IncomingTransaction,
    ) -> Result<Option<ApplicationOutcome>, IncomingError> {
        Ok(match self.owned(transaction)?.application {
            Application::Settled(outcome) => Some(outcome),
            _ => None,
        })
    }
    pub fn begin(
        &mut self,
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        decoded: &DecodedSegment,
        version: ProtocolVersion,
        now: u64,
    ) -> Result<IncomingStart, IncomingError> {
        self.clock(now)?;
        if peer != self.peer {
            return Err(IncomingError::Identity);
        }
        if protocol != self.protocol {
            return Err(IncomingError::Service);
        }
        let id = decoded.segment.transaction_id;
        let index = usize::from(id);
        if index >= TRANSACTION_COUNT {
            return Err(IncomingError::TransactionId);
        }
        if !matches!(decoded.segment.kind, SegmentKind::Begin { .. }) {
            return Err(IncomingError::Receiver(ReceiveError::BeginRequired));
        }
        match self.slots[index] {
            Slot::Active(_) => return Err(IncomingError::Occupied),
            Slot::Guarded { until_monotonic_ms } if now < until_monotonic_ms => {
                return Err(IncomingError::Guarded);
            }
            _ => {}
        }
        if self.reservation_octets
            > self.limits.maximum_reserved_payload_octets - self.reserved_payload_octets()
        {
            return Err(IncomingError::Capacity);
        }
        let mut receiver = Receiver::try_new(
            peer,
            protocol,
            id,
            version,
            self.limits.maximum_message_octets,
            now,
        )
        .map_err(IncomingError::Receiver)?;
        let reception = receiver
            .receive(peer, protocol, decoded, now)
            .map_err(IncomingError::Receiver)?;
        let owner = Arc::new(());
        let transaction = IncomingTransaction {
            id,
            owner: owner.clone(),
        };
        let mut active = OwnedReceiver {
            owner,
            receiver,
            application: Application::None,
        };
        let reception = Self::retain(&mut active, reception);
        self.slots[index] = Slot::Active(Box::new(active));
        self.last_monotonic_ms = now;
        Ok(IncomingStart {
            transaction,
            reception,
        })
    }
    fn retain(active: &mut OwnedReceiver, reception: Reception) -> IncomingReception {
        let message = reception.message.map(|bytes| ApplicationMessage {
            owner: Arc::new(()),
            bytes: bytes.into(),
        });
        if let Some(message) = &message {
            active.application = Application::Pending(message.clone());
        }
        IncomingReception {
            response: reception.response,
            message,
        }
    }
    pub fn receive(
        &mut self,
        transaction: &IncomingTransaction,
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        decoded: &DecodedSegment,
        now: u64,
    ) -> Result<IncomingReception, IncomingError> {
        self.clock(now)?;
        let index = self.index(transaction)?;
        self.last_monotonic_ms = now;
        let Slot::Active(active) = &mut self.slots[index] else {
            unreachable!("validated active receiver")
        };
        let reception = active
            .receiver
            .receive(peer, protocol, decoded, now)
            .map_err(IncomingError::Receiver)?;
        Ok(Self::retain(active, reception))
    }
    pub fn advance(
        &mut self,
        transaction: &IncomingTransaction,
        now: u64,
    ) -> Result<Option<Response>, IncomingError> {
        self.clock(now)?;
        let index = self.index(transaction)?;
        self.last_monotonic_ms = now;
        let Slot::Active(active) = &mut self.slots[index] else {
            unreachable!("validated active receiver")
        };
        active
            .receiver
            .advance(now)
            .map_err(IncomingError::Receiver)
    }
    pub fn cancel(
        &mut self,
        transaction: &IncomingTransaction,
        now: u64,
    ) -> Result<(), IncomingError> {
        self.clock(now)?;
        let index = self.index(transaction)?;
        self.last_monotonic_ms = now;
        let Slot::Active(active) = &mut self.slots[index] else {
            unreachable!("validated active receiver")
        };
        active.receiver.cancel(now).map_err(IncomingError::Receiver)
    }
    /// Settle only after the application has durably committed or explicitly rejected this exact completed message.
    pub fn settle_application(
        &mut self,
        transaction: &IncomingTransaction,
        message: &ApplicationMessage,
        outcome: ApplicationOutcome,
        now: u64,
    ) -> Result<(), IncomingError> {
        self.clock(now)?;
        let index = self.index(transaction)?;
        let Slot::Active(active) = &mut self.slots[index] else {
            unreachable!("validated active receiver")
        };
        if !matches!(&active.application, Application::Pending(pending) if Arc::ptr_eq(&pending.owner, &message.owner))
        {
            return Err(IncomingError::StaleApplication);
        }
        active.application = Application::Settled(outcome);
        self.last_monotonic_ms = now;
        Ok(())
    }
    /// Fence queued responses and old lower-layer deliveries before retiring this wire ID.
    pub fn retire_fenced(
        &mut self,
        transaction: &IncomingTransaction,
        now: u64,
    ) -> Result<(), IncomingError> {
        self.clock(now)?;
        let index = self.index(transaction)?;
        let active = self.owned(transaction)?;
        if matches!(
            active.receiver.state(),
            ReceiverState::AwaitingBegin | ReceiverState::Receiving
        ) {
            return Err(IncomingError::StillActive);
        }
        if matches!(active.application, Application::Pending(_)) {
            return Err(IncomingError::PendingApplication);
        }
        let until_monotonic_ms = now
            .checked_add(self.reuse_guard_ms)
            .ok_or(IncomingError::TimeOverflow)?;
        self.slots[index] = Slot::Guarded { until_monotonic_ms };
        self.last_monotonic_ms = now;
        Ok(())
    }
}
#[cfg(test)]
mod tests;
