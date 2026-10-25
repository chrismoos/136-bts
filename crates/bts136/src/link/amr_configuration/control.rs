use super::{BLOCK_DURATION_NS, Completion, ConfigurationContext, Error, Time};
use crate::{
    l3::{
        broadcast::ProtocolVersion,
        legacy_traffic::{
            ActiveAmrMode, AmrControlMessage, AmrFreeze, AmrPhase, AmrThresholds, TrafficMessage,
        },
        relay::Direction,
    },
    link::rach::MobileIdentity,
};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlState {
    Prepared,
    AwaitingAck,
    Applied,
    Cancelled,
}
impl ControlState {
    fn closed(self) -> bool {
        matches!(self, Self::Applied | Self::Cancelled)
    }
}
#[derive(Debug, Clone)]
pub struct ControlTransmission {
    token: Arc<()>,
    message: TrafficMessage,
}
impl ControlTransmission {
    pub fn message(&self) -> &TrafficMessage {
        &self.message
    }
}
#[derive(Debug, Clone, Copy)]
pub(super) enum ControlUpdate {
    Thresholds(AmrThresholds),
    Phase(AmrPhase),
}
impl ControlUpdate {
    fn message(self) -> AmrControlMessage {
        match self {
            Self::Thresholds(thresholds) => AmrControlMessage::Thresholds(thresholds),
            Self::Phase(phase) => AmrControlMessage::PhaseFreeze {
                phase,
                freeze: AmrFreeze::Maintain,
                mode: ActiveAmrMode::Mode1,
            },
        }
    }
}
/// TIA-136-133-E §8.5 and Table 35 specify single Threshold/Phase orders and Mobile Ack, without configuration retry/filler timers.
pub struct AcknowledgedControlExchange {
    recipient: MobileIdentity,
    update: ControlUpdate,
    context_token: Arc<()>,
    transmission: ControlTransmission,
    state: ControlState,
    prepared_at: Time,
    last_time: Time,
    was_transmitted: bool,
    committed: bool,
}
impl AcknowledgedControlExchange {
    pub(super) fn try_new(
        recipient: MobileIdentity,
        update: ControlUpdate,
        version: ProtocolVersion,
        context_token: Arc<()>,
        now: Time,
    ) -> Result<Self, Error> {
        let control = update.message();
        control.encode_for_version(version).map_err(Error::Codec)?;
        Ok(Self {
            recipient,
            update,
            context_token,
            transmission: ControlTransmission {
                token: Arc::new(()),
                message: TrafficMessage::AmrControl(control),
            },
            state: ControlState::Prepared,
            prepared_at: now,
            last_time: now,
            was_transmitted: false,
            committed: false,
        })
    }
    pub fn state(&self) -> ControlState {
        self.state
    }
    pub fn transmission(&self) -> Option<&ControlTransmission> {
        (self.state == ControlState::Prepared).then_some(&self.transmission)
    }
    pub(super) fn context_token(&self) -> &Arc<()> {
        &self.context_token
    }
    pub(super) fn was_transmitted(&self) -> bool {
        self.was_transmitted
    }
    fn check_clock(&self, now: Time) -> Result<(), Error> {
        if now.block_offset_ns >= BLOCK_DURATION_NS {
            return Err(Error::BlockOffset);
        }
        if (now.fdtc_block, now.block_offset_ns)
            < (self.last_time.fdtc_block, self.last_time.block_offset_ns)
            || now.monotonic_ms < self.last_time.monotonic_ms
        {
            return Err(Error::ClockRegression);
        }
        Ok(())
    }
    /// Supply actual final-portion completion separately from observation time. ACKs must follow validated transmission.
    pub fn transmitted(
        &mut self,
        transmission: &ControlTransmission,
        completion: Completion,
        now: Time,
    ) -> Result<ControlState, Error> {
        self.check_clock(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        if self.state != ControlState::Prepared
            || !Arc::ptr_eq(&transmission.token, &self.transmission.token)
        {
            return Err(Error::StaleTransmission);
        }
        if completion.last_fdtc_block < self.prepared_at.fdtc_block
            || completion.last_fdtc_block > now.fdtc_block
            || completion.completed_monotonic_ms < self.prepared_at.monotonic_ms
            || completion.completed_monotonic_ms > now.monotonic_ms
        {
            return Err(Error::TransmissionTime);
        }
        self.was_transmitted = true;
        self.state = ControlState::AwaitingAck;
        self.last_time = now;
        Ok(self.state)
    }
    /// Supply a CRC-valid reverse ACK from this bearer. Wire ACKs have no request ID, so serialize and fence reception.
    pub fn acknowledge(
        &mut self,
        recipient: MobileIdentity,
        message: &TrafficMessage,
        now: Time,
    ) -> Result<ControlState, Error> {
        self.check_clock(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        if recipient != self.recipient {
            return Err(Error::Recipient);
        }
        let TrafficMessage::Ack {
            ack_message_type,
            information,
        } = message
        else {
            return Err(Error::NotAck);
        };
        information
            .validate_in(Direction::Reverse)
            .map_err(Error::Codec)?;
        message.encode().map_err(Error::Codec)?;
        let expected = self.transmission.message.message_type();
        if *ack_message_type != expected {
            return Err(Error::AckType {
                expected,
                received: *ack_message_type,
            });
        }
        if !self.was_transmitted {
            return Err(Error::NotTransmitted);
        }
        self.state = ControlState::Applied;
        self.last_time = now;
        Ok(self.state)
    }
    pub(super) fn is_thresholds(&self) -> bool {
        matches!(self.update, ControlUpdate::Thresholds(_))
    }
    pub fn commit_thresholds(&mut self, context: &mut ConfigurationContext) -> Result<(), Error> {
        if !self.is_thresholds() {
            return Err(Error::ControlKind);
        }
        self.commit(context)
    }
    pub fn commit(&mut self, context: &mut ConfigurationContext) -> Result<(), Error> {
        if self.state != ControlState::Applied {
            return Err(Error::NotApplied);
        }
        if self.committed {
            return Err(Error::AlreadyApplied);
        }
        match self.update {
            ControlUpdate::Thresholds(thresholds) => {
                context.apply_thresholds(&self.context_token, thresholds, self.last_time)?
            }
            ControlUpdate::Phase(phase) => {
                context.apply_phase(&self.context_token, phase, self.last_time)?
            }
        }
        self.committed = true;
        Ok(())
    }
    pub(super) fn cancel(&mut self, now: Time) -> Result<ControlState, Error> {
        self.check_clock(now)?;
        if !self.state.closed() {
            self.state = ControlState::Cancelled;
        }
        self.last_time = now;
        Ok(self.state)
    }
}
#[cfg(test)]
mod phase_tests;
#[cfg(test)]
mod tests;
