use super::{Error as SenderError, LowerLayerFailure, Sender, SenderState, Submission};
use crate::{
    l3::{
        broadcast::ProtocolVersion,
        relay::HigherLayerProtocol,
        tsar::{AcknowledgmentWindow, DecodedResponse, MessagePlan, PlanningError, plan_message},
    },
    link::rach::MobileIdentity,
};
use std::{error::Error as StdError, fmt, sync::Arc};

// TIA-136-620-A §§3.1–3.4 assign three bits to the originator's transaction ID.
const TRANSACTION_COUNT: usize = 8;
const SEGMENTED_CARRIER_BITS: u8 = 0xc0;

#[derive(Debug, Clone)]
pub struct Transaction {
    id: u8,
    owner: Arc<()>,
}
impl Transaction {
    pub fn transaction_id(&self) -> u8 {
        self.id
    }
}
#[derive(Debug, Clone)]
pub enum Start {
    Unsegmented(Vec<u8>),
    Segmented(Transaction),
}
pub enum SenderEvent<'a> {
    Advance,
    Submitted {
        submission: &'a Submission,
        queued_monotonic_ms: u64,
    },
    Accepted(&'a Submission),
    LowerFailed {
        submission: &'a Submission,
        failure: LowerLayerFailure,
    },
    Response {
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        decoded: &'a DecodedResponse,
    },
    Cancel,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutgoingError {
    Identity,
    Service,
    GuardDuration,
    Exhausted,
    StaleTransaction,
    StillActive,
    ClockRegression,
    TimeOverflow,
    Planning(PlanningError),
    Sender(SenderError),
}
impl fmt::Display for OutgoingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "outgoing TSAR transaction failed: {self:?}")
    }
}
impl StdError for OutgoingError {}
enum Slot {
    Available,
    Active { owner: Arc<()>, sender: Box<Sender> },
    Guarded { until_monotonic_ms: u64 },
}

/// Keep one bank per peer/service for the transport session. A wire response has no local generation token.
pub struct OutgoingTransactions {
    peer: MobileIdentity,
    protocol: HigherLayerProtocol,
    reuse_guard_ms: u64,
    last_monotonic_ms: u64,
    next: usize,
    slots: [Slot; TRANSACTION_COUNT],
}
impl OutgoingTransactions {
    /// Immediate allocation requires a fresh session with no possible older traffic. The guard must cover the transport's maximum late-response lifetime, not just one TSAR timer.
    pub fn try_new(
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        reuse_guard_ms: u64,
        now: u64,
    ) -> Result<Self, OutgoingError> {
        let (_, width, value) = peer.parts();
        if value == 0 || value >= 1 << width {
            return Err(OutgoingError::Identity);
        }
        if !matches!(protocol, HigherLayerProtocol::Segmented(_))
            && !matches!(protocol, HigherLayerProtocol::CarrierSpecific(raw) if raw & SEGMENTED_CARRIER_BITS == SEGMENTED_CARRIER_BITS)
        {
            return Err(OutgoingError::Service);
        }
        if reuse_guard_ms == 0 {
            return Err(OutgoingError::GuardDuration);
        }
        Ok(Self {
            peer,
            protocol,
            reuse_guard_ms,
            last_monotonic_ms: now,
            next: 0,
            slots: std::array::from_fn(|_| Slot::Available),
        })
    }
    pub fn after_restart(
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
        reuse_guard_ms: u64,
        now: u64,
    ) -> Result<Self, OutgoingError> {
        let mut bank = Self::try_new(peer, protocol, reuse_guard_ms, now)?;
        let until_monotonic_ms = now
            .checked_add(reuse_guard_ms)
            .ok_or(OutgoingError::TimeOverflow)?;
        bank.slots = std::array::from_fn(|_| Slot::Guarded { until_monotonic_ms });
        Ok(bank)
    }
    fn clock(&self, now: u64) -> Result<(), OutgoingError> {
        if now < self.last_monotonic_ms {
            return Err(OutgoingError::ClockRegression);
        }
        Ok(())
    }
    fn index(&self, transaction: &Transaction) -> Result<usize, OutgoingError> {
        let index = usize::from(transaction.id);
        match &self.slots[index] {
            Slot::Active { owner, .. } if Arc::ptr_eq(owner, &transaction.owner) => Ok(index),
            _ => Err(OutgoingError::StaleTransaction),
        }
    }
    pub fn start(
        &mut self,
        message: &[u8],
        payload_budget_octets: usize,
        window: AcknowledgmentWindow,
        version: ProtocolVersion,
        now: u64,
    ) -> Result<Start, OutgoingError> {
        self.clock(now)?;
        let available = (0..TRANSACTION_COUNT)
            .map(|offset| (self.next + offset) % TRANSACTION_COUNT)
            .find(|index| match self.slots[*index] {
                Slot::Available => true,
                Slot::Guarded { until_monotonic_ms } => now >= until_monotonic_ms,
                Slot::Active { .. } => false,
            });
        let plan = plan_message(
            message,
            payload_budget_octets,
            available.unwrap_or(0) as u8,
            window,
            version,
        )
        .map_err(OutgoingError::Planning)?;
        let plan = match plan {
            MessagePlan::Unsegmented(bytes) => {
                self.last_monotonic_ms = now;
                return Ok(Start::Unsegmented(bytes));
            }
            MessagePlan::Segmented(plan) => plan,
        };
        let index = available.ok_or(OutgoingError::Exhausted)?;
        let sender = Sender::try_new(self.peer, self.protocol, plan, version, now)
            .map_err(OutgoingError::Sender)?;
        let owner = Arc::new(());
        self.slots[index] = Slot::Active {
            owner: owner.clone(),
            sender: Box::new(sender),
        };
        self.next = (index + 1) % TRANSACTION_COUNT;
        self.last_monotonic_ms = now;
        Ok(Start::Segmented(Transaction {
            id: index as u8,
            owner,
        }))
    }
    pub fn sender(&self, transaction: &Transaction) -> Result<&Sender, OutgoingError> {
        let index = self.index(transaction)?;
        let Slot::Active { sender, .. } = &self.slots[index] else {
            unreachable!("validated active transaction")
        };
        Ok(sender)
    }
    pub fn dispatch(
        &mut self,
        transaction: &Transaction,
        event: SenderEvent<'_>,
        now: u64,
    ) -> Result<SenderState, OutgoingError> {
        self.clock(now)?;
        let index = self.index(transaction)?;
        self.last_monotonic_ms = now;
        let Slot::Active { sender, .. } = &mut self.slots[index] else {
            unreachable!("validated active transaction")
        };
        match event {
            SenderEvent::Advance => sender.advance(now),
            SenderEvent::Submitted {
                submission,
                queued_monotonic_ms,
            } => sender.submitted(submission, queued_monotonic_ms, now),
            SenderEvent::Accepted(submission) => sender.accepted(submission, now),
            SenderEvent::LowerFailed {
                submission,
                failure,
            } => sender.lower_failed(submission, failure, now),
            SenderEvent::Response {
                peer,
                protocol,
                decoded,
            } => sender.receive_response(peer, protocol, decoded, now),
            SenderEvent::Cancel => sender.cancel(now),
        }
        .map_err(OutgoingError::Sender)
    }
    /// Stop and fence every queued/in-flight lower-layer attempt before retirement. Dropping a handle never frees its ID.
    pub fn retire_fenced(
        &mut self,
        transaction: &Transaction,
        now: u64,
    ) -> Result<(), OutgoingError> {
        self.clock(now)?;
        let index = self.index(transaction)?;
        if !matches!(
            self.sender(transaction)?.state(),
            SenderState::Complete(_) | SenderState::Failed(_) | SenderState::Cancelled
        ) {
            return Err(OutgoingError::StillActive);
        }
        let until_monotonic_ms = now
            .checked_add(self.reuse_guard_ms)
            .ok_or(OutgoingError::TimeOverflow)?;
        self.slots[index] = Slot::Guarded { until_monotonic_ms };
        self.last_monotonic_ms = now;
        Ok(())
    }
}
#[cfg(test)]
mod tests;
