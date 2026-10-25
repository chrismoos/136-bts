mod context;
mod control;
use crate::{
    l3::{
        broadcast::ProtocolVersion,
        legacy_traffic::{
            AmrConfiguration, AmrControlMessage, TrafficMessage, TrafficMessageError,
        },
        relay::Direction,
    },
    link::rach::MobileIdentity,
};
pub use context::{ConfigurationContext, DownlinkPhase, LinkConfiguration};
pub use control::{AcknowledgedControlExchange, ControlState, ControlTransmission};
pub use control::{
    AcknowledgedControlExchange as ThresholdExchange, ControlState as ThresholdState,
    ControlTransmission as ThresholdTransmission,
};
use std::{error::Error as StdError, fmt, sync::Arc};
// TIA-136-133-E §8.5 specifies ACK by block 12, application at 13, 100 ms retries and a 5 s filler abort.
const ACK_BLOCKS: u64 = 12;
const APPLY_BLOCKS: u64 = 13;
const ACK_TIMEOUT_MS: u64 = 100;
const FILLER_ABORT_MS: u64 = 5000;
const BLOCK_DURATION_NS: u32 = 20_000_000;
/// FDTC blocks are unwrapped. Offsets are from block start. Supply observations in nondecreasing air and monotonic time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    pub fdtc_block: u64,
    pub block_offset_ns: u32,
    pub monotonic_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion {
    /// The block containing the final portion can precede the completion boundary's block.
    pub last_fdtc_block: u64,
    pub completed_monotonic_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Prepared,
    Countdown { ack_block: u64, apply_block: u64 },
    CountdownAcknowledged { apply_block: u64 },
    AwaitingAck { deadline_monotonic_ms: u64 },
    Applied,
    Aborted,
    Cancelled,
}
impl State {
    fn closed(self) -> bool {
        matches!(self, Self::Applied | Self::Aborted | Self::Cancelled)
    }
}
#[derive(Debug, Clone)]
pub struct FillerHandle {
    token: Arc<()>,
}
impl FillerHandle {
    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
}
#[derive(Debug, Clone)]
enum Protection {
    None,
    Required {
        handle: FillerHandle,
        requested_ms: u64,
    },
    Active {
        handle: FillerHandle,
        requested_ms: u64,
        started_ms: u64,
        abort_monotonic_ms: u64,
    },
}
#[derive(Debug, Clone)]
pub struct Transmission {
    token: Arc<()>,
    message: TrafficMessage,
    prepared_at: Time,
}
impl Transmission {
    pub fn message(&self) -> &TrafficMessage {
        &self.message
    }
    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
}
#[derive(Debug, Clone, Copy)]
enum Sent {
    Countdown { ack_block: u64, apply_block: u64 },
    Immediate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Identity,
    Codec(TrafficMessageError),
    ClockRegression,
    BlockOffset,
    TimeOverflow,
    Closed,
    StaleTransmission,
    TransmissionTime,
    NotTransmitted,
    NotApplied,
    AlreadyApplied,
    Context,
    ControlKind,
    Busy,
    Recipient,
    NotAck,
    AckType { expected: u8, received: u8 },
    LateCountdownAck,
    FillerNotRequested,
    StaleFiller,
    FillerTime,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AMR control exchange failed: {self:?}")
    }
}
impl StdError for Error {}
pub struct Exchange {
    recipient: MobileIdentity,
    configuration: AmrConfiguration,
    state: State,
    protection: Protection,
    pending: Option<Transmission>,
    sent: Option<Sent>,
    transmissions: u64,
    last_time: Time,
    application_committed: bool,
    context_token: Option<Arc<()>>,
}
impl Exchange {
    pub fn try_new(
        recipient: MobileIdentity,
        configuration: AmrConfiguration,
        version: ProtocolVersion,
        now: Time,
    ) -> Result<Self, Error> {
        if now.block_offset_ns >= BLOCK_DURATION_NS {
            return Err(Error::BlockOffset);
        }
        let (_, width, value) = recipient.parts();
        if value == 0 || value >= 1 << width {
            return Err(Error::Identity);
        }
        let message = AmrControlMessage::Configuration(configuration);
        message.encode_for_version(version).map_err(Error::Codec)?;
        Ok(Self {
            recipient,
            configuration,
            state: State::Prepared,
            protection: Protection::None,
            pending: Some(Transmission {
                token: Arc::new(()),
                message: TrafficMessage::AmrControl(message),
                prepared_at: now,
            }),
            sent: None,
            transmissions: 0,
            last_time: now,
            application_committed: false,
            context_token: None,
        })
    }
    /// Commit once to this bearer's configuration context after the protocol application boundary.
    pub fn commit_configuration(
        &mut self,
        context: &mut ConfigurationContext,
    ) -> Result<(), Error> {
        if self.state != State::Applied {
            return Err(Error::NotApplied);
        }
        if self.application_committed {
            return Err(Error::AlreadyApplied);
        }
        if context.recipient() != self.recipient {
            return Err(Error::Recipient);
        }
        let token = self.context_token.as_ref().ok_or(Error::Context)?;
        context.apply(token, self.configuration, self.last_time)?;
        self.application_committed = true;
        Ok(())
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn configuration(&self) -> &AmrConfiguration {
        &self.configuration
    }
    pub fn transmissions(&self) -> u64 {
        self.transmissions
    }
    pub fn transmission(&self) -> Option<&Transmission> {
        self.pending.as_ref()
    }
    pub fn filler_handle(&self) -> Option<&FillerHandle> {
        if self.state.closed() {
            return None;
        }
        match &self.protection {
            Protection::None => None,
            Protection::Required { handle, .. } | Protection::Active { handle, .. } => Some(handle),
        }
    }
    pub fn filler_requested(&self) -> bool {
        self.filler_handle().is_some()
    }
    pub fn reverse_muted(&self) -> bool {
        !matches!(self.protection, Protection::None) || self.state == State::Aborted
    }
    fn require_protection(&mut self, now: Time) {
        if matches!(self.protection, Protection::None) {
            self.protection = Protection::Required {
                handle: FillerHandle {
                    token: Arc::new(()),
                },
                requested_ms: now.monotonic_ms,
            };
        }
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
    fn retry(&mut self, now: Time) {
        self.configuration.countdown = false;
        self.pending = Some(Transmission {
            token: Arc::new(()),
            message: TrafficMessage::AmrControl(AmrControlMessage::Configuration(
                self.configuration,
            )),
            prepared_at: now,
        });
        self.require_protection(now);
        self.state = State::Prepared;
    }
    pub fn advance(&mut self, now: Time) -> Result<State, Error> {
        self.check_clock(now)?;
        if !self.state.closed() {
            if matches!(&self.protection, Protection::Active { abort_monotonic_ms, .. } if now.monotonic_ms >= *abort_monotonic_ms)
            {
                self.state = State::Aborted;
                self.pending = None;
            } else {
                match self.state {
                    State::Countdown { ack_block, .. } if now.fdtc_block >= ack_block => {
                        self.retry(now)
                    }
                    State::CountdownAcknowledged { apply_block }
                        if now.fdtc_block >= apply_block =>
                    {
                        self.state = State::Applied
                    }
                    State::AwaitingAck {
                        deadline_monotonic_ms,
                    } if now.monotonic_ms >= deadline_monotonic_ms => self.retry(now),
                    _ => {}
                }
            }
        }
        self.last_time = now;
        Ok(self.state)
    }
    /// Supply actual final-portion metadata separately from observation time. Delayed receipts cannot postpone response deadlines.
    pub fn transmitted(
        &mut self,
        transmission: &Transmission,
        completion: Completion,
        now: Time,
    ) -> Result<State, Error> {
        self.check_clock(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| !pending.matches(transmission))
        {
            return Err(Error::StaleTransmission);
        }
        let prepared_at = self
            .pending
            .as_ref()
            .ok_or(Error::StaleTransmission)?
            .prepared_at;
        if completion.last_fdtc_block < prepared_at.fdtc_block
            || completion.last_fdtc_block > now.fdtc_block
            || completion.completed_monotonic_ms < prepared_at.monotonic_ms
            || completion.completed_monotonic_ms > now.monotonic_ms
        {
            return Err(Error::TransmissionTime);
        }
        let (sent, state) = if self.configuration.countdown {
            let ack_block = completion
                .last_fdtc_block
                .checked_add(ACK_BLOCKS)
                .ok_or(Error::TimeOverflow)?;
            let apply_block = completion
                .last_fdtc_block
                .checked_add(APPLY_BLOCKS)
                .ok_or(Error::TimeOverflow)?;
            (
                Sent::Countdown {
                    ack_block,
                    apply_block,
                },
                State::Countdown {
                    ack_block,
                    apply_block,
                },
            )
        } else {
            let deadline_monotonic_ms = completion
                .completed_monotonic_ms
                .checked_add(ACK_TIMEOUT_MS)
                .ok_or(Error::TimeOverflow)?;
            (
                Sent::Immediate,
                State::AwaitingAck {
                    deadline_monotonic_ms,
                },
            )
        };
        let transmissions = self
            .transmissions
            .checked_add(1)
            .ok_or(Error::TimeOverflow)?;
        self.advance(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| !pending.matches(transmission))
        {
            return Err(Error::StaleTransmission);
        }
        self.pending = None;
        self.sent = Some(sent);
        self.state = state;
        self.transmissions = transmissions;
        self.advance(now)
    }
    /// Supply this exchange's filler handle and actual start time. Delayed receipts may tighten but never extend the abort deadline.
    pub fn filler_transmitted(
        &mut self,
        owner: &FillerHandle,
        started_monotonic_ms: u64,
        now: Time,
    ) -> Result<State, Error> {
        self.check_clock(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        let (handle, requested_ms, started_ms) = match &self.protection {
            Protection::None => return Err(Error::FillerNotRequested),
            Protection::Required {
                handle,
                requested_ms,
            } => (handle.clone(), *requested_ms, started_monotonic_ms),
            Protection::Active {
                handle,
                requested_ms,
                started_ms,
                ..
            } => (
                handle.clone(),
                *requested_ms,
                (*started_ms).min(started_monotonic_ms),
            ),
        };
        if !handle.matches(owner) {
            return Err(Error::StaleFiller);
        }
        if started_monotonic_ms < requested_ms || started_monotonic_ms > now.monotonic_ms {
            return Err(Error::FillerTime);
        }
        let abort_monotonic_ms = started_ms
            .checked_add(FILLER_ABORT_MS)
            .ok_or(Error::TimeOverflow)?;
        self.advance(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        self.protection = Protection::Active {
            handle,
            requested_ms,
            started_ms,
            abort_monotonic_ms,
        };
        if now.monotonic_ms >= abort_monotonic_ms {
            self.state = State::Aborted;
            self.pending = None;
        }
        Ok(self.state)
    }
    /// Supply a CRC-valid Mobile Ack from this bearer with no concurrent configuration exchange. Wire ACKs have no request ID.
    pub fn acknowledge(
        &mut self,
        recipient: MobileIdentity,
        message: &TrafficMessage,
        now: Time,
    ) -> Result<State, Error> {
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
        let expected =
            TrafficMessage::AmrControl(AmrControlMessage::Configuration(self.configuration))
                .message_type();
        if *ack_message_type != expected {
            return Err(Error::AckType {
                expected,
                received: *ack_message_type,
            });
        }
        let sent = self.sent.ok_or(Error::NotTransmitted)?;
        self.advance(now)?;
        if self.state.closed() {
            return Err(Error::Closed);
        }
        match sent {
            Sent::Countdown {
                ack_block,
                apply_block,
            } => {
                if now.fdtc_block > ack_block
                    || (now.fdtc_block == ack_block && now.block_offset_ns != 0)
                {
                    return Err(Error::LateCountdownAck);
                }
                self.configuration.countdown = true;
                self.state = State::CountdownAcknowledged { apply_block };
            }
            Sent::Immediate => self.state = State::Applied,
        }
        self.pending = None;
        self.protection = Protection::None;
        Ok(self.state)
    }
    /// Cancelling a transmitted configuration requires retiring the bearer. It cannot roll back the phone's state.
    pub fn cancel(&mut self, now: Time) -> Result<State, Error> {
        self.advance(now)?;
        if !self.state.closed() {
            self.state = State::Cancelled;
            self.pending = None;
            if self.sent.is_some() {
                self.require_protection(now);
            }
        }
        Ok(self.state)
    }
}
#[cfg(test)]
mod tests;
