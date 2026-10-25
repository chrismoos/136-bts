mod parties;
pub(crate) use parties::PartySlots;
mod service;
pub use service::ServiceError;
pub(crate) use service::{ServiceState, ServiceTransmission};
mod flash;
mod measurements;
pub use flash::FlashError;
pub(crate) use flash::{FlashState, FlashTransmission};
pub use measurements::{ExtendedMeasurementSnapshot, MeasurementHandle};
use std::{collections::VecDeque, error::Error, fmt, sync::Arc};
const MAX_PENDING_RESPONSES: usize = 16;
pub(crate) const MAX_ANSWER_TIMEOUT_MS: u64 = 65_000;

use super::calls::{CallHandle, CallReservation, ReservedParty};
use crate::{
    l3::legacy_traffic::{
        AcknowledgmentInformation, ControlChannel, ReleaseFields, ReleaseInformation,
        ReleaseReason, TrafficMessage,
    },
    link::traffic_order::{
        OrderError, OrderKind, OrderPolicy, OrderState, OrderTransmission, TrafficOrderExchange,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Party {
    Caller,
    Callee,
}
impl Party {
    fn index(self) -> usize {
        match self {
            Self::Caller => 0,
            Self::Callee => 1,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    SetupTimeout,
    AlertTimeout,
    AnswerTimeout,
    Cancelled,
    MobileRelease(Party),
    RadioFailure(Party),
    TimingOutOfRange(Party),
    TimingOverlap,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Establishing,
    Alerting,
    WaitingAnswer,
    Connecting,
    Connected,
    Releasing(EndReason),
    Stopped(EndReason),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerState {
    Reserved,
    Assigning,
    Established,
    Stopping,
    Stopped,
}
#[derive(Debug, Clone, Copy)]
pub struct SessionPolicy {
    pub establishment_timeout_ms: u64,
    pub answer_timeout_ms: u64,
    pub release_timeout_ms: u64,
    pub alert: OrderPolicy,
    pub release: OrderPolicy,
    pub measurements: OrderPolicy,
    pub status: OrderPolicy,
    pub capabilities: OrderPolicy,
    pub physical_control: OrderPolicy,
}
#[derive(Debug, Clone)]
pub struct BearerHandle {
    party: Party,
    token: Arc<()>,
}
impl BearerHandle {
    pub fn party(&self) -> Party {
        self.party
    }
}
#[derive(Debug, Clone)]
pub struct Assignment {
    pub handle: BearerHandle,
    pub subscriber: ReservedParty,
}
#[derive(Debug, Clone)]
pub struct ResponseTransmission {
    party: Party,
    token: Arc<()>,
    message: TrafficMessage,
}
impl ResponseTransmission {
    pub(crate) fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
    pub fn party(&self) -> Party {
        self.party
    }
    pub fn message(&self) -> &TrafficMessage {
        &self.message
    }
}
#[derive(Debug, Clone)]
pub struct StopRequest {
    party: Party,
    token: Arc<()>,
}
impl StopRequest {
    pub fn party(&self) -> Party {
        self.party
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    ResponseQueueFull,
    Flash(FlashError),
    Service(ServiceError),
    Policy,
    TimeOverflow,
    ClockRegression,
    StaleBearer,
    StaleResponse,
    StaleStop,
    State,
    ProtocolContext,
    Message,
    Order(OrderError),
    Control(crate::l3::legacy_traffic::TrafficMessageError),
}
impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "call session failed: {self:?}")
    }
}
impl Error for SessionError {}
struct Leg {
    confirmed_physical_control: crate::l3::legacy_traffic::PhysicalControlFields,
    alignment_measurements: measurements::AlignmentState,
    protocol_version: Option<crate::l3::broadcast::ProtocolVersion>,
    extended_measurements: Option<measurements::Active>,
    service: service::Endpoint,
    flash: flash::Endpoint,
    subscriber: ReservedParty,
    handle: BearerHandle,
    state: BearerState,
    release: Option<TrafficOrderExchange>,
    return_channel: Option<ControlChannel>,
    release_reason: Option<ReleaseReason>,
    measurements: Option<TrafficOrderExchange>,
    responses: VecDeque<ResponseTransmission>,
    status: Option<TrafficOrderExchange>,
    status_outcome: Option<OrderState>,
    capabilities: Option<TrafficOrderExchange>,
    capability_outcome: Option<OrderState>,
    physical_control: Option<TrafficOrderExchange>,
    physical_control_outcome: Option<OrderState>,
    physical_control_last_request: Option<crate::l3::legacy_traffic::PhysicalControlRequest>,
    stop: Option<StopRequest>,
}
impl Leg {
    fn new(party: Party, subscriber: ReservedParty) -> Self {
        Self {
            confirmed_physical_control: Default::default(),
            alignment_measurements: measurements::AlignmentState::Unrestricted,
            protocol_version: None,
            extended_measurements: None,
            service: service::Endpoint::new(subscriber.codec),
            flash: Default::default(),
            subscriber,
            handle: BearerHandle {
                party,
                token: Arc::new(()),
            },
            state: BearerState::Reserved,
            release: None,
            return_channel: None,
            release_reason: None,
            measurements: None,
            responses: VecDeque::new(),
            status: None,
            status_outcome: None,
            capabilities: None,
            capability_outcome: None,
            physical_control: None,
            physical_control_outcome: None,
            physical_control_last_request: None,
            stop: None,
        }
    }
    fn queue_ack(&mut self, message_type: u8) -> Result<(), SessionError> {
        self.queue_response(TrafficMessage::Ack {
            ack_message_type: message_type,
            information: AcknowledgmentInformation {
                control_channels: if message_type == ReleaseInformation::MESSAGE_TYPE {
                    self.return_channel.into_iter().collect()
                } else {
                    Vec::new()
                },
                ..Default::default()
            },
        })
    }
    fn queue_response(&mut self, message: TrafficMessage) -> Result<(), SessionError> {
        if self
            .responses
            .iter()
            .any(|pending| pending.message == message)
        {
            return Ok(());
        }
        if self.responses.len() >= MAX_PENDING_RESPONSES {
            return Err(SessionError::ResponseQueueFull);
        }
        self.responses.push_back(ResponseTransmission {
            party: self.handle.party,
            token: Arc::new(()),
            message,
        });
        Ok(())
    }
    fn close_status(&mut self) {
        if let Some(status) = self.status.take() {
            self.status_outcome = Some(match status.state() {
                OrderState::Prepared | OrderState::AwaitingAck { .. } => OrderState::Cancelled,
                state => state,
            });
        }
    }
    fn close_capabilities(&mut self) {
        if let Some(exchange) = self.capabilities.take() {
            self.capability_outcome = Some(match exchange.state() {
                OrderState::Prepared | OrderState::AwaitingAck { .. } => OrderState::Cancelled,
                state => state,
            });
        }
    }
    fn close_physical_control(&mut self) {
        if let Some(exchange) = self.physical_control.take() {
            self.physical_control_outcome = Some(match exchange.state() {
                OrderState::Prepared | OrderState::AwaitingAck { .. } => OrderState::Cancelled,
                state => state,
            });
        }
    }
    fn request_stop(&mut self) {
        self.release = None;
        self.measurements = None;
        self.extended_measurements = None;
        self.flash.cancel();
        self.service.cancel();
        self.responses.clear();
        self.close_status();
        self.close_capabilities();
        self.close_physical_control();
        if matches!(self.state, BearerState::Stopping | BearerState::Stopped) {
            return;
        }
        if self.state == BearerState::Reserved {
            self.state = BearerState::Stopped;
            return;
        }
        self.state = BearerState::Stopping;
        self.stop = Some(StopRequest {
            party: self.handle.party,
            token: Arc::new(()),
        });
    }
}

pub struct CallSession {
    handle: CallHandle,
    caller_number: String,
    unassigned_control: crate::l3::legacy_traffic::PhysicalControlFields,
    audio_direction: Option<super::calls::AudioDirection>,
    remote_answer_pending: bool,
    policy: SessionPolicy,
    state: CallState,
    legs: PartySlots<Leg>,
    alert: Option<TrafficOrderExchange>,
    establishment_deadline_monotonic_ms: u64,
    answer_deadline_monotonic_ms: Option<u64>,
    release_deadline_monotonic_ms: Option<u64>,
    last_monotonic_ms: u64,
}
impl CallSession {
    /// The reservation must be handed off by CallPaging before either traffic assignment is transmitted.
    pub fn try_new(
        reservation: CallReservation,
        policy: SessionPolicy,
        monotonic_ms: u64,
    ) -> Result<Self, SessionError> {
        let legs = PartySlots::pair(
            Leg::new(Party::Caller, reservation.caller.clone()),
            Leg::new(Party::Callee, reservation.callee.clone()),
        );
        Self::from_parts(
            reservation.handle,
            reservation.caller.number,
            None,
            legs,
            policy,
            monotonic_ms,
        )
    }
    pub fn try_new_audio(
        reservation: super::calls::AudioReservation,
        policy: SessionPolicy,
        monotonic_ms: u64,
    ) -> Result<Self, SessionError> {
        use super::calls::AudioDirection;
        let party = match reservation.direction {
            AudioDirection::MobileOriginated => Party::Caller,
            AudioDirection::MobileTerminated => Party::Callee,
        };
        let caller_number = if party == Party::Caller {
            reservation.mobile.number.clone()
        } else {
            reservation.remote_number.clone()
        };
        Self::from_parts(
            reservation.handle,
            caller_number,
            Some(reservation.direction),
            PartySlots::single(party, Leg::new(party, reservation.mobile)),
            policy,
            monotonic_ms,
        )
    }
    fn from_parts(
        handle: CallHandle,
        caller_number: String,
        audio_direction: Option<super::calls::AudioDirection>,
        legs: PartySlots<Leg>,
        policy: SessionPolicy,
        monotonic_ms: u64,
    ) -> Result<Self, SessionError> {
        if policy.establishment_timeout_ms == 0
            || policy.answer_timeout_ms == 0
            || policy.answer_timeout_ms > MAX_ANSWER_TIMEOUT_MS
            || policy.release_timeout_ms == 0
        {
            return Err(SessionError::Policy);
        }
        for order in [
            policy.alert,
            policy.release,
            policy.measurements,
            policy.status,
            policy.capabilities,
            policy.physical_control,
        ] {
            if order.response_timeout_ms == 0
                || order.overall_timeout_ms == 0
                || order.max_transmissions == 0
            {
                return Err(SessionError::Policy);
            }
        }
        let establishment_deadline_monotonic_ms = monotonic_ms
            .checked_add(policy.establishment_timeout_ms)
            .ok_or(SessionError::TimeOverflow)?;
        Ok(Self {
            handle,
            caller_number,
            unassigned_control: Default::default(),
            audio_direction,
            remote_answer_pending: false,
            policy,
            state: CallState::Establishing,
            legs,
            alert: None,
            establishment_deadline_monotonic_ms,
            answer_deadline_monotonic_ms: None,
            release_deadline_monotonic_ms: None,
            last_monotonic_ms: monotonic_ms,
        })
    }
    pub(crate) fn wait_for_remote_answer(&mut self) {
        self.remote_answer_pending = true;
    }
    pub(crate) fn remote_answered(&mut self, now_ms: u64) -> Result<(), SessionError> {
        self.advance(now_ms)?;
        self.remote_answer_pending = false;
        if self.state == CallState::WaitingAnswer
            && self.audio_direction == Some(super::calls::AudioDirection::MobileOriginated)
        {
            self.answer_deadline_monotonic_ms = None;
            self.state = CallState::Connected;
        }
        Ok(())
    }
    pub(crate) fn set_return_channel(
        &mut self,
        channel: ControlChannel,
    ) -> Result<(), SessionError> {
        channel.validate().map_err(SessionError::Control)?;
        if self.state != CallState::Establishing
            || self
                .legs
                .iter()
                .any(|leg| leg.state != BearerState::Reserved)
        {
            return Err(SessionError::State);
        }
        for leg in &mut self.legs {
            leg.return_channel = Some(channel);
        }
        Ok(())
    }
    pub fn release_reason(&self, party: Party) -> Option<ReleaseReason> {
        self.legs.get(party.index())?.release_reason
    }
    pub(crate) fn operation_deadlines(&self) -> Vec<crate::operations::proto::Metric> {
        use crate::operations::metric;
        let mut readings = vec![metric(
            "Establishment deadline",
            self.establishment_deadline_monotonic_ms,
            "ms since startup",
        )];
        if let Some(deadline) = self.answer_deadline_monotonic_ms {
            readings.push(metric("Answer deadline", deadline, "ms since startup"));
        }
        if let Some(deadline) = self.release_deadline_monotonic_ms {
            readings.push(metric("Release deadline", deadline, "ms since startup"));
        }
        readings
    }
    pub fn state(&self) -> CallState {
        self.state
    }
    pub(crate) fn handle(&self) -> &CallHandle {
        &self.handle
    }
    pub fn bearer_state(&self, party: Party) -> BearerState {
        self.legs
            .get(party.index())
            .map_or(BearerState::Stopped, |leg| leg.state)
    }
    pub(crate) fn parties(&self) -> [Option<Party>; 2] {
        self.legs.present()
    }

    pub fn assignment(&self, party: Party) -> Option<Assignment> {
        let leg = self.legs.get(party.index())?;
        (self.state == CallState::Establishing && leg.state == BearerState::Reserved).then(|| {
            Assignment {
                handle: leg.handle.clone(),
                subscriber: leg.subscriber.clone(),
            }
        })
    }
    pub fn order(&self, party: Party) -> Option<&OrderTransmission> {
        if matches!(self.state, CallState::Releasing(_)) {
            return self
                .legs
                .get(party.index())?
                .release
                .as_ref()?
                .transmission();
        }
        if !matches!(self.state, CallState::Connected | CallState::Stopped(_))
            && let Some(exchange) = &self.legs.get(party.index())?.physical_control
            && matches!(
                exchange.state(),
                OrderState::Prepared | OrderState::AwaitingAck { .. }
            )
        {
            return exchange.transmission();
        }
        if self.state == CallState::Alerting && party == Party::Callee {
            return self.alert.as_ref()?.transmission();
        }
        if self.state == CallState::Connected {
            let leg = &self.legs.get(party.index())?;
            for exchange in [
                &leg.measurements,
                &leg.status,
                &leg.capabilities,
                &leg.physical_control,
            ]
            .into_iter()
            .flatten()
            {
                if exchange.transmissions() > 0
                    && matches!(
                        exchange.state(),
                        OrderState::Prepared | OrderState::AwaitingAck { .. }
                    )
                {
                    return exchange.transmission();
                }
            }
            if leg.alignment_measurements == measurements::AlignmentState::AwaitingMeasurementTransmission
                && leg.physical_control.as_ref().is_some_and(|control| {
                    matches!(control.kind(), OrderKind::PhysicalControl(request) if request.time_alignment == Some(crate::l3::designation::TimeAlignment::OffsetUnits(crate::l3::legacy_traffic::MAX_ALIGNMENT_UNITS)))
                    && matches!(control.state(), OrderState::Prepared | OrderState::AwaitingAck { .. })
                })
            {
                return leg.measurements.as_ref()?.transmission();
            }
            for exchange in [
                &leg.physical_control,
                &leg.status,
                &leg.capabilities,
                &leg.measurements,
            ]
            .into_iter()
            .flatten()
            {
                if matches!(
                    exchange.state(),
                    OrderState::Prepared | OrderState::AwaitingAck { .. }
                ) {
                    return exchange.transmission();
                }
            }
            return None;
        }
        None
    }
    pub(crate) fn measurement_state(&self, party: Party) -> Option<OrderState> {
        self.legs
            .get(party.index())?
            .measurements
            .as_ref()
            .map(TrafficOrderExchange::state)
    }
    pub(crate) fn status_state(&self, party: Party) -> Option<OrderState> {
        self.legs
            .get(party.index())?
            .status
            .as_ref()
            .map(TrafficOrderExchange::state)
            .or(self.legs.get(party.index())?.status_outcome)
    }
    pub(crate) fn request_status(
        &mut self,
        party: Party,
        monotonic_ms: u64,
    ) -> Result<(), SessionError> {
        self.check_party(party)?;
        self.advance(monotonic_ms)?;
        if self.state != CallState::Connected {
            return Err(SessionError::State);
        }
        let leg = &mut self.legs[party.index()];
        if leg.status.as_ref().is_some_and(|status| {
            matches!(
                status.state(),
                OrderState::Prepared | OrderState::AwaitingAck { .. }
            )
        }) {
            return Ok(());
        }
        leg.status = Some(
            TrafficOrderExchange::try_new(
                leg.subscriber.identity,
                OrderKind::QueryStatus,
                self.policy.status,
                monotonic_ms,
            )
            .map_err(SessionError::Order)?,
        );
        Ok(())
    }
    pub(crate) fn capability_state(&self, party: Party) -> Option<OrderState> {
        self.legs
            .get(party.index())?
            .capabilities
            .as_ref()
            .map(TrafficOrderExchange::state)
            .or(self.legs.get(party.index())?.capability_outcome)
    }
    pub(crate) fn request_capabilities(
        &mut self,
        party: Party,
        monotonic_ms: u64,
    ) -> Result<(), SessionError> {
        self.check_party(party)?;
        self.advance(monotonic_ms)?;
        if self.state != CallState::Connected {
            return Err(SessionError::State);
        }
        let leg = &mut self.legs[party.index()];
        if leg.capabilities.as_ref().is_some_and(|status| {
            matches!(
                status.state(),
                OrderState::Prepared | OrderState::AwaitingAck { .. }
            )
        }) {
            return Ok(());
        }
        leg.capabilities = Some(
            TrafficOrderExchange::try_new(
                leg.subscriber.identity,
                OrderKind::QueryCapabilities(
                    leg.protocol_version
                        .unwrap_or(crate::l3::broadcast::ProtocolVersion::Is136RevA),
                ),
                self.policy.capabilities,
                monotonic_ms,
            )
            .map_err(SessionError::Order)?,
        );
        Ok(())
    }
    pub(crate) fn physical_control_request(
        &self,
        party: Party,
    ) -> Option<crate::l3::legacy_traffic::PhysicalControlRequest> {
        self.legs.get(party.index())?.physical_control_last_request
    }
    pub(crate) fn physical_control_transmitted(&self, party: Party) -> bool {
        self.legs
            .get(party.index())
            .and_then(|leg| leg.physical_control.as_ref())
            .is_some_and(|exchange| exchange.transmissions() > 0)
    }
    pub(crate) fn physical_control_state(&self, party: Party) -> Option<OrderState> {
        let leg = &self.legs.get(party.index())?;
        leg.physical_control
            .as_ref()
            .map(TrafficOrderExchange::state)
            .or(leg.physical_control_outcome)
    }
    pub(crate) fn request_physical_control(
        &mut self,
        party: Party,
        request: crate::l3::legacy_traffic::PhysicalControlRequest,
        monotonic_ms: u64,
    ) -> Result<(), SessionError> {
        self.check_party(party)?;
        request.validate().map_err(SessionError::Control)?;
        if request.is_empty() {
            return Err(SessionError::Message);
        }
        self.advance(monotonic_ms)?;
        if self.state != CallState::Connected {
            return Err(SessionError::State);
        }
        let leg = &mut self.legs[party.index()];
        if let Some(current) = &leg.physical_control
            && matches!(
                current.state(),
                OrderState::Prepared | OrderState::AwaitingAck { .. }
            )
        {
            if current.kind() == OrderKind::PhysicalControl(request) {
                return Ok(());
            }
            return Err(SessionError::State);
        }
        let exchange = TrafficOrderExchange::try_new(
            leg.subscriber.identity,
            OrderKind::PhysicalControl(request),
            self.policy.physical_control,
            monotonic_ms,
        )
        .map_err(SessionError::Order)?;
        if request.time_alignment
            == Some(crate::l3::designation::TimeAlignment::OffsetUnits(
                crate::l3::legacy_traffic::MAX_ALIGNMENT_UNITS,
            ))
        {
            leg.restrict_neighbor_measurements(
                self.policy.measurements,
                monotonic_ms,
                measurements::AlignmentRestriction::Requested,
            )?;
            leg.alignment_measurements.restrict();
        }
        leg.physical_control = Some(exchange);
        leg.physical_control_last_request = Some(request);
        Ok(())
    }
    pub(crate) fn align_initial_bearer(
        &mut self,
        handle: &BearerHandle,
        units: u8,
        monotonic_ms: u64,
    ) -> Result<(), SessionError> {
        self.check_bearer(handle)?;
        self.advance(monotonic_ms)?;
        if self.legs[handle.party.index()].state != BearerState::Assigning
            || self.state != CallState::Establishing
        {
            return Err(SessionError::State);
        }
        let leg = &mut self.legs[handle.party.index()];
        if leg.physical_control.is_some() {
            return Ok(());
        }
        let request = crate::l3::legacy_traffic::PhysicalControlRequest {
            time_alignment: Some(crate::l3::designation::TimeAlignment::OffsetUnits(units)),
            ..Default::default()
        };
        request.validate().map_err(SessionError::Control)?;
        leg.physical_control = Some(
            TrafficOrderExchange::try_new(
                leg.subscriber.identity,
                OrderKind::PhysicalControl(request),
                self.policy.physical_control,
                monotonic_ms,
            )
            .map_err(SessionError::Order)?,
        );
        if units == crate::l3::legacy_traffic::MAX_ALIGNMENT_UNITS {
            leg.alignment_measurements.restrict();
        }
        leg.physical_control_last_request = Some(request);
        Ok(())
    }
    pub fn set_measurements(
        &mut self,
        party: Party,
        enabled: bool,
        monotonic_ms: u64,
    ) -> Result<(), SessionError> {
        self.check_party(party)?;
        self.check_clock(monotonic_ms)?;
        self.advance(monotonic_ms)?;
        if self.state != CallState::Connected {
            return Err(SessionError::State);
        }
        let kind = if enabled {
            OrderKind::MeasureServingChannel
        } else {
            OrderKind::StopMeasurements
        };
        let leg = &mut self.legs[party.index()];
        if leg.measurements.as_ref().is_some_and(|current| {
            current.kind() == kind
                && !matches!(
                    current.state(),
                    OrderState::TimedOut | OrderState::Cancelled
                )
        }) {
            return Ok(());
        }
        leg.measurements = Some(
            TrafficOrderExchange::try_new(
                leg.subscriber.identity,
                kind,
                self.policy.measurements,
                monotonic_ms,
            )
            .map_err(SessionError::Order)?,
        );
        leg.extended_measurements = None;
        Ok(())
    }
    pub(crate) fn measurement_kind(&self, party: Party) -> Option<OrderKind> {
        self.legs
            .get(party.index())?
            .measurements
            .as_ref()
            .map(TrafficOrderExchange::kind)
    }
    pub(crate) fn service_state(&self, party: Party) -> Option<ServiceState> {
        self.legs.get(party.index())?.service.state()
    }
    pub(crate) fn service_request(
        &self,
        party: Party,
    ) -> Option<&crate::l3::legacy_traffic::ServiceMessage> {
        self.legs.get(party.index())?.service.request()
    }
    pub(crate) fn service_count(&self, party: Party) -> u64 {
        self.legs
            .get(party.index())
            .map_or(0, |leg| leg.service.count())
    }
    pub(crate) fn service_response(&self, party: Party) -> Option<&TrafficMessage> {
        self.legs.get(party.index())?.service.response()
    }
    pub(crate) fn service_transmission(&self, party: Party) -> Option<&ServiceTransmission> {
        self.legs.get(party.index())?.service.transmission()
    }
    pub(crate) fn service_transmitted(
        &mut self,
        party: Party,
        tx: &ServiceTransmission,
        now: u64,
    ) -> Result<(), SessionError> {
        self.check_party(party)?;
        self.advance(now)?;
        self.legs[party.index()]
            .service
            .transmitted(tx, now)
            .map_err(SessionError::Service)
    }
    pub(crate) fn flash_state(&self, party: Party) -> Option<FlashState> {
        self.legs.get(party.index())?.flash.state()
    }
    pub(crate) fn flash_number(&self, party: Party) -> Option<u8> {
        self.legs.get(party.index())?.flash.number()
    }
    pub(crate) fn flash_received(
        &self,
        party: Party,
    ) -> Option<&crate::l3::legacy_traffic::ReverseFlashFields> {
        self.legs.get(party.index())?.flash.received()
    }
    pub(crate) fn flash_received_count(&self, party: Party) -> u64 {
        self.legs
            .get(party.index())
            .map_or(0, |leg| leg.flash.received_count())
    }
    pub(crate) fn flash_transmission(&self, party: Party) -> Option<&FlashTransmission> {
        self.legs.get(party.index())?.flash.transmission()
    }
    pub(crate) fn request_call_information(
        &mut self,
        party: Party,
        fields: crate::l3::legacy_traffic::CallInformation,
        now: u64,
    ) -> Result<(), SessionError> {
        self.check_party(party)?;
        self.advance(now)?;
        if self.state != CallState::Connected {
            return Err(SessionError::State);
        }
        self.legs[party.index()]
            .flash
            .request(fields, now)
            .map_err(SessionError::Flash)
    }
    pub(crate) fn flash_transmitted(
        &mut self,
        party: Party,
        tx: &FlashTransmission,
        now: u64,
    ) -> Result<(), SessionError> {
        self.check_party(party)?;
        self.advance(now)?;
        self.legs[party.index()]
            .flash
            .transmitted(tx, now)
            .map_err(SessionError::Flash)
    }
    pub fn response(&self, party: Party) -> Option<&ResponseTransmission> {
        self.legs.get(party.index())?.responses.front()
    }
    // TIA-136-133-E §§8.4–8.5 require a single response without changing the call task.
    pub(crate) fn respond_capabilities(
        &mut self,
        handle: &BearerHandle,
        capabilities: crate::l3::legacy_traffic::BaseStationCapabilities,
        monotonic_ms: u64,
    ) -> Result<(), SessionError> {
        self.check_bearer(handle)?;
        self.advance(monotonic_ms)?;
        if self.legs[handle.party.index()].state != BearerState::Established
            || !matches!(
                self.state,
                CallState::Alerting
                    | CallState::WaitingAnswer
                    | CallState::Connecting
                    | CallState::Connected
            )
        {
            return Err(SessionError::State);
        }
        self.legs[handle.party.index()].queue_response(TrafficMessage::CapabilityUpdateResponse(
            crate::l3::legacy_traffic::CapabilityResponse::BaseStation(capabilities),
        ))
    }
    pub fn stop_request(&self, party: Party) -> Option<&StopRequest> {
        self.legs.get(party.index())?.stop.as_ref()
    }
    pub fn media_ready(&self) -> bool {
        self.state == CallState::Connected
    }
    /// Only this terminal handle may be passed to reservation release after assignments have started.
    pub fn stopped_handle(&self) -> Option<&CallHandle> {
        matches!(self.state, CallState::Stopped(_)).then_some(&self.handle)
    }
    /// Mark before an assignment can reach the air, including before submitting it to a radio queue.
    pub fn assignment_started(
        &mut self,
        handle: &BearerHandle,
        monotonic_ms: u64,
    ) -> Result<CallState, SessionError> {
        self.check_bearer(handle)?;
        self.advance(monotonic_ms)?;
        let leg = &mut self.legs[handle.party.index()];
        if self.state != CallState::Establishing || leg.state != BearerState::Reserved {
            return Err(SessionError::State);
        }
        leg.state = BearerState::Assigning;
        Ok(self.state)
    }
    /// Confirmation requires the radio procedure's valid-DVCC and initialization checks, not assignment transmission alone.
    pub fn bearer_confirmed(
        &mut self,
        handle: &BearerHandle,
        monotonic_ms: u64,
    ) -> Result<CallState, SessionError> {
        self.check_bearer(handle)?;
        self.advance(monotonic_ms)?;
        if self.state != CallState::Establishing
            || self.legs[handle.party.index()].state != BearerState::Assigning
        {
            return Err(SessionError::State);
        }
        let other = 1 - handle.party.index();
        let alert = if self.audio_direction == Some(super::calls::AudioDirection::MobileTerminated)
            || self
                .legs
                .get(other)
                .is_some_and(|leg| leg.state == BearerState::Established)
        {
            Some(
                TrafficOrderExchange::try_new_alert(
                    self.legs[Party::Callee.index()].subscriber.identity,
                    crate::l3::legacy_traffic::AlertInformation {
                        calling_numbers: vec![
                            crate::l3::legacy_traffic::CallingNumber::network(&self.caller_number)
                                .unwrap_or_else(|_| {
                                    crate::l3::legacy_traffic::CallingNumber::unavailable()
                                }),
                        ],
                        ..Default::default()
                    },
                    self.policy.alert,
                    monotonic_ms,
                )
                .map_err(SessionError::Order)?,
            )
        } else {
            None
        };
        self.legs[handle.party.index()].state = BearerState::Established;
        if let Some(alert) = alert {
            self.alert = Some(alert);
            self.state = CallState::Alerting;
        } else if self.audio_direction == Some(super::calls::AudioDirection::MobileOriginated) {
            if self.remote_answer_pending {
                self.state = CallState::WaitingAnswer;
                self.answer_deadline_monotonic_ms = Some(
                    monotonic_ms
                        .checked_add(self.policy.answer_timeout_ms)
                        .ok_or(SessionError::TimeOverflow)?,
                );
            } else {
                self.state = CallState::Connected;
            }
        }
        Ok(self.state)
    }
    pub fn order_transmitted(
        &mut self,
        handle: &BearerHandle,
        transmission: &OrderTransmission,
        monotonic_ms: u64,
    ) -> Result<CallState, SessionError> {
        self.check_bearer(handle)?;
        self.advance(monotonic_ms)?;
        if !matches!(self.state, CallState::Releasing(_) | CallState::Stopped(_))
            && let Some(exchange) = &mut self.legs[handle.party.index()].physical_control
            && exchange
                .transmission()
                .is_some_and(|current| current.matches(transmission))
        {
            exchange
                .transmitted(transmission, monotonic_ms)
                .map_err(SessionError::Order)?;
            return Ok(self.state);
        }
        if matches!(self.state, CallState::Releasing(_)) {
            let order = self.legs[handle.party.index()]
                .release
                .as_mut()
                .ok_or(SessionError::State)?;
            order
                .transmitted(transmission, monotonic_ms)
                .map_err(SessionError::Order)?;
        } else if self.state == CallState::Connected {
            let leg = &mut self.legs[handle.party.index()];
            let exchange = [
                &mut leg.status,
                &mut leg.capabilities,
                &mut leg.physical_control,
                &mut leg.measurements,
            ]
            .into_iter()
            .find_map(|exchange| {
                exchange.as_mut().filter(|current| {
                    current
                        .transmission()
                        .is_some_and(|pending| pending.matches(transmission))
                })
            })
            .ok_or(SessionError::Order(OrderError::StaleTransmission))?;
            let state = exchange
                .transmitted(transmission, monotonic_ms)
                .map_err(SessionError::Order)?;
            if matches!(state, OrderState::AwaitingAck { .. })
                && let Some(targets) = exchange.extended_targets().cloned()
                && let Some(active) = &mut leg.extended_measurements
            {
                active.transmitted(targets);
            }
            leg.measurement_alignment_transmitted();
        } else {
            if self.state != CallState::Alerting || handle.party != Party::Callee {
                return Err(SessionError::State);
            }
            let deadline = match self.answer_deadline_monotonic_ms {
                Some(deadline) => deadline,
                None => monotonic_ms
                    .checked_add(self.policy.answer_timeout_ms)
                    .ok_or(SessionError::TimeOverflow)?,
            };
            self.alert
                .as_mut()
                .ok_or(SessionError::State)?
                .transmitted(transmission, monotonic_ms)
                .map_err(SessionError::Order)?;
            self.answer_deadline_monotonic_ms = Some(deadline);
        }
        self.advance(monotonic_ms)
    }
    // TIA-136-133-E §§8.4–8.5 require matching Base Station Ack for Connect, Release and spontaneous Status.
    pub fn receive(
        &mut self,
        handle: &BearerHandle,
        message: &TrafficMessage,
        monotonic_ms: u64,
    ) -> Result<CallState, SessionError> {
        self.check_bearer(handle)?;
        match message {
            TrafficMessage::Release { information } => information
                .validate_in(crate::l3::relay::Direction::Reverse)
                .map_err(SessionError::Control)?,
            TrafficMessage::Ack { information, .. } => information
                .validate_in(crate::l3::relay::Direction::Reverse)
                .map_err(SessionError::Control)?,
            _ => {}
        }
        self.advance(monotonic_ms)?;
        match message {
            TrafficMessage::Service(request) => {
                if self.state != CallState::Connected {
                    return Err(SessionError::State);
                }
                let leg = &mut self.legs[handle.party.index()];
                let mut service = leg.service.clone();
                service
                    .receive(request.clone(), monotonic_ms)
                    .map_err(SessionError::Service)?;
                leg.queue_ack(message.message_type())?;
                leg.service = service;
            }
            TrafficMessage::Flash(message) => {
                if self.state != CallState::Connected {
                    return Err(SessionError::State);
                }
                let leg = &mut self.legs[handle.party.index()];
                match message {
                    crate::l3::legacy_traffic::FlashMessage::Reverse {
                        request_number,
                        request,
                    } => {
                        let is_new = leg
                            .flash
                            .reverse_is_new(*request_number, request)
                            .map_err(SessionError::Flash)?;
                        leg.queue_response(TrafficMessage::Flash(
                            message.acknowledgment().ok_or(SessionError::Message)?,
                        ))?;
                        if is_new {
                            leg.flash.record_reverse(*request_number, request.clone());
                        }
                    }
                    crate::l3::legacy_traffic::FlashMessage::Ack(ack) => leg
                        .flash
                        .acknowledge(ack, monotonic_ms)
                        .map_err(SessionError::Flash)?,
                    _ => return Err(SessionError::Message),
                }
            }
            TrafficMessage::Dtmf(request) => {
                if self.state != CallState::Connected {
                    return Err(SessionError::State);
                }
                message.encode().map_err(|_| SessionError::Message)?;
                let ack = request.acknowledgment().ok_or(SessionError::Message)?;
                self.legs[handle.party.index()].queue_response(TrafficMessage::Dtmf(ack))?;
            }
            TrafficMessage::Status(_) => {
                if self.legs[handle.party.index()].state != BearerState::Established
                    || matches!(self.state, CallState::Releasing(_) | CallState::Stopped(_))
                {
                    return Err(SessionError::State);
                }
                let leg = &mut self.legs[handle.party.index()];
                if let Some(status) = &mut leg.status {
                    match status.acknowledge(leg.subscriber.identity, message, monotonic_ms) {
                        Ok(_) => {}
                        Err(
                            OrderError::NotTransmitted
                            | OrderError::StatusFields
                            | OrderError::Closed,
                        ) => {}
                        Err(error) => return Err(SessionError::Order(error)),
                    }
                }
                leg.queue_ack(message.message_type())?;
            }
            TrafficMessage::MeasurementOrderAck { .. }
            | TrafficMessage::ExtendedMeasurementAck(_) => {
                if self.state != CallState::Connected {
                    return Err(SessionError::State);
                }
                let leg = &mut self.legs[handle.party.index()];
                leg.measurements
                    .as_mut()
                    .ok_or(SessionError::State)?
                    .acknowledge(leg.subscriber.identity, message, monotonic_ms)
                    .map_err(SessionError::Order)?;
            }
            TrafficMessage::PhysicalControlAck(fields) => {
                if matches!(self.state, CallState::Releasing(_) | CallState::Stopped(_)) {
                    return Err(SessionError::State);
                }
                message.encode().map_err(SessionError::Control)?;
                let leg = &mut self.legs[handle.party.index()];
                leg.physical_control
                    .as_mut()
                    .ok_or(SessionError::State)?
                    .acknowledge(leg.subscriber.identity, message, monotonic_ms)
                    .map_err(SessionError::Order)?;
                leg.confirmed_physical_control.update(fields);
                if let Some(crate::l3::designation::TimeAlignment::OffsetUnits(units)) =
                    fields.control.time_alignment
                {
                    if units >= crate::l3::legacy_traffic::MAX_ALIGNMENT_UNITS {
                        leg.alignment_measurements.restrict();
                    } else {
                        leg.alignment_measurements = measurements::AlignmentState::Unrestricted;
                    }
                }
                if leg.alignment_measurements != measurements::AlignmentState::Unrestricted {
                    leg.restrict_neighbor_measurements(
                        self.policy.measurements,
                        monotonic_ms,
                        measurements::AlignmentRestriction::Confirmed,
                    )?;
                }
            }
            TrafficMessage::CapabilityUpdateResponse(
                crate::l3::legacy_traffic::CapabilityResponse::Mobile(_),
            ) => {
                if self.legs[handle.party.index()].state != BearerState::Established
                    || matches!(self.state, CallState::Releasing(_) | CallState::Stopped(_))
                {
                    return Err(SessionError::State);
                }
                let leg = &mut self.legs[handle.party.index()];
                if let Some(exchange) = &mut leg.capabilities {
                    match exchange.acknowledge(leg.subscriber.identity, message, monotonic_ms) {
                        Ok(_) | Err(OrderError::NotTransmitted | OrderError::Closed) => {}
                        Err(error) => return Err(SessionError::Order(error)),
                    }
                }
            }
            TrafficMessage::ChannelQuality(_) => {
                if self.legs[handle.party.index()].state != BearerState::Established {
                    return Err(SessionError::State);
                }
            }
            TrafficMessage::Connect => {
                if handle.party != Party::Callee
                    || !matches!(
                        self.state,
                        CallState::Alerting
                            | CallState::WaitingAnswer
                            | CallState::Connecting
                            | CallState::Connected
                    )
                    || self
                        .alert
                        .as_ref()
                        .is_none_or(|alert| alert.transmissions() == 0)
                {
                    return Err(SessionError::State);
                }
                self.legs[handle.party.index()].queue_ack(message.message_type())?;
                if self.state != CallState::Connected {
                    self.state = CallState::Connecting;
                }
            }
            TrafficMessage::Release { information } => {
                if self.legs[handle.party.index()].state != BearerState::Established {
                    return Err(SessionError::State);
                }
                self.start_release(
                    EndReason::MobileRelease(handle.party),
                    Some(handle.party),
                    monotonic_ms,
                )?;
                let leg = &mut self.legs[handle.party.index()];
                leg.release = None;
                if let ReleaseFields::Reason(reason) = information.fields {
                    leg.release_reason.get_or_insert(reason);
                }
                leg.queue_ack(message.message_type())?;
            }
            TrafficMessage::Ack {
                ack_message_type, ..
            } if *ack_message_type == crate::l3::legacy_traffic::ServiceMessage::RESPONSE_TYPE => {
                if self.state != CallState::Connected {
                    return Err(SessionError::State);
                }
                self.legs[handle.party.index()]
                    .service
                    .acknowledge(monotonic_ms)
                    .map_err(SessionError::Service)?;
            }
            TrafficMessage::Ack { .. } => {
                if self.state == CallState::Connected {
                    let leg = &mut self.legs[handle.party.index()];
                    leg.measurements
                        .as_mut()
                        .ok_or(SessionError::State)?
                        .acknowledge(leg.subscriber.identity, message, monotonic_ms)
                        .map_err(SessionError::Order)?;
                } else if matches!(self.state, CallState::Releasing(_)) {
                    let leg = &mut self.legs[handle.party.index()];
                    let order = leg.release.as_mut().ok_or(SessionError::State)?;
                    order
                        .acknowledge(leg.subscriber.identity, message, monotonic_ms)
                        .map_err(SessionError::Order)?;
                    leg.request_stop();
                } else {
                    if handle.party != Party::Callee {
                        return Err(SessionError::State);
                    }
                    let alert = self.alert.as_mut().ok_or(SessionError::State)?;
                    alert
                        .acknowledge(
                            self.legs[Party::Callee.index()].subscriber.identity,
                            message,
                            monotonic_ms,
                        )
                        .map_err(SessionError::Order)?;
                    if self.state == CallState::Alerting {
                        self.state = CallState::WaitingAnswer;
                    }
                }
            }
            _ => return Err(SessionError::Message),
        }
        Ok(self.state)
    }
    pub fn response_transmitted(
        &mut self,
        transmission: &ResponseTransmission,
        monotonic_ms: u64,
    ) -> Result<CallState, SessionError> {
        self.check_clock(monotonic_ms)?;
        let leg = self
            .legs
            .get(transmission.party.index())
            .ok_or(SessionError::StaleResponse)?;
        if leg
            .responses
            .front()
            .is_none_or(|pending| !Arc::ptr_eq(&pending.token, &transmission.token))
        {
            return Err(SessionError::StaleResponse);
        }
        self.advance(monotonic_ms)?;
        let leg = &mut self.legs[transmission.party.index()];
        if leg
            .responses
            .front()
            .is_none_or(|pending| !Arc::ptr_eq(&pending.token, &transmission.token))
        {
            return Err(SessionError::StaleResponse);
        }
        leg.responses.pop_front();
        let TrafficMessage::Ack {
            ack_message_type, ..
        } = transmission.message
        else {
            return Ok(self.state);
        };
        let release_type = TrafficMessage::Release {
            information: Default::default(),
        }
        .message_type();
        if ack_message_type == crate::l3::legacy_traffic::ServiceMessage::REQUEST_TYPE
            && self.state == CallState::Connected
        {
            leg.service.request_ack_transmitted();
        }
        if ack_message_type == release_type && matches!(self.state, CallState::Releasing(_)) {
            leg.request_stop();
        } else if ack_message_type == TrafficMessage::Connect.message_type()
            && self.state == CallState::Connecting
        {
            self.state = CallState::Connected;
        }
        Ok(self.state)
    }
    /// Both radio streams and all queued work must be stopped before this forced teardown.
    pub fn radio_shutdown(&mut self, monotonic_ms: u64) -> Result<(), SessionError> {
        self.check_clock(monotonic_ms)?;
        for leg in &mut self.legs {
            leg.state = BearerState::Stopped;
            leg.release = None;
            leg.measurements = None;
            leg.extended_measurements = None;
            leg.flash.cancel();
            leg.service.cancel();
            leg.responses.clear();
            leg.close_status();
            leg.close_capabilities();
            leg.close_physical_control();
            leg.stop = None;
        }
        self.alert = None;
        self.state = CallState::Stopped(EndReason::Cancelled);
        self.last_monotonic_ms = monotonic_ms;
        Ok(())
    }
    pub fn end(&mut self, reason: EndReason, monotonic_ms: u64) -> Result<CallState, SessionError> {
        self.check_clock(monotonic_ms)?;
        self.start_release(reason, None, monotonic_ms)?;
        self.last_monotonic_ms = monotonic_ms;
        Ok(self.state)
    }
    pub fn advance(&mut self, monotonic_ms: u64) -> Result<CallState, SessionError> {
        self.check_clock(monotonic_ms)?;
        match self.state {
            CallState::Establishing if monotonic_ms >= self.establishment_deadline_monotonic_ms => {
                self.start_release(EndReason::SetupTimeout, None, monotonic_ms)?
            }
            CallState::Alerting | CallState::WaitingAnswer | CallState::Connecting
                if self
                    .answer_deadline_monotonic_ms
                    .is_some_and(|deadline| monotonic_ms >= deadline) =>
            {
                self.start_release(EndReason::AnswerTimeout, None, monotonic_ms)?
            }
            CallState::Alerting
                if self
                    .alert
                    .as_mut()
                    .ok_or(SessionError::State)?
                    .advance(monotonic_ms)
                    .map_err(SessionError::Order)?
                    == OrderState::TimedOut =>
            {
                self.start_release(EndReason::AlertTimeout, None, monotonic_ms)?;
            }
            _ => {}
        }
        for leg in &mut self.legs {
            leg.service
                .advance(monotonic_ms)
                .map_err(SessionError::Service)?;
            leg.flash
                .advance(monotonic_ms)
                .map_err(SessionError::Flash)?;
        }
        if self.state == CallState::Connected {
            for leg in &mut self.legs {
                if leg.measurements.is_none() {
                    leg.measurements = Some(
                        TrafficOrderExchange::try_new(
                            leg.subscriber.identity,
                            OrderKind::MeasureServingChannel,
                            self.policy.measurements,
                            monotonic_ms,
                        )
                        .map_err(SessionError::Order)?,
                    );
                }
                leg.measurements
                    .as_mut()
                    .expect("connected measurement exchange exists")
                    .advance(monotonic_ms)
                    .map_err(SessionError::Order)?;
                if leg.measurements.as_ref().is_some_and(|order| {
                    matches!(order.state(), OrderState::TimedOut | OrderState::Cancelled)
                }) {
                    leg.extended_measurements = None;
                }
            }
        }
        for leg in &mut self.legs {
            for exchange in [
                &mut leg.status,
                &mut leg.capabilities,
                &mut leg.physical_control,
            ]
            .into_iter()
            .flatten()
            {
                exchange
                    .advance(monotonic_ms)
                    .map_err(SessionError::Order)?;
            }
        }
        if matches!(self.state, CallState::Releasing(_)) {
            let deadline = self
                .release_deadline_monotonic_ms
                .ok_or(SessionError::State)?;
            for leg in &mut self.legs {
                let timed_out = if let Some(order) = &mut leg.release {
                    order.advance(monotonic_ms).map_err(SessionError::Order)?
                        == OrderState::TimedOut
                } else {
                    false
                };
                if monotonic_ms >= deadline || timed_out {
                    leg.request_stop();
                }
            }
            self.finish_stopped();
        }
        self.last_monotonic_ms = monotonic_ms;
        Ok(self.state)
    }
    /// Report the identified radio bearer's actual stopped state, not Mobile Ack or a queued stop command.
    pub fn bearer_stopped(
        &mut self,
        request: &StopRequest,
        monotonic_ms: u64,
    ) -> Result<CallState, SessionError> {
        self.check_clock(monotonic_ms)?;
        self.check_party(request.party)?;
        let leg = &mut self.legs[request.party.index()];
        if leg
            .stop
            .as_ref()
            .is_none_or(|pending| !Arc::ptr_eq(&pending.token, &request.token))
        {
            return Err(SessionError::StaleStop);
        }
        leg.state = BearerState::Stopped;
        leg.stop = None;
        self.finish_stopped();
        self.last_monotonic_ms = monotonic_ms;
        Ok(self.state)
    }
    fn start_release(
        &mut self,
        reason: EndReason,
        origin: Option<Party>,
        monotonic_ms: u64,
    ) -> Result<(), SessionError> {
        if matches!(self.state, CallState::Stopped(_)) {
            return Ok(());
        }
        if matches!(reason, EndReason::RadioFailure(_)) {
            self.state = CallState::Releasing(reason);
            self.release_deadline_monotonic_ms = Some(monotonic_ms);
            self.alert = None;
            for leg in &mut self.legs {
                leg.request_stop();
            }
            self.finish_stopped();
            return Ok(());
        }
        if matches!(self.state, CallState::Releasing(_)) {
            return Ok(());
        }
        let deadline = monotonic_ms
            .checked_add(self.policy.release_timeout_ms)
            .ok_or(SessionError::TimeOverflow)?;
        let policy = OrderPolicy {
            overall_timeout_ms: self
                .policy
                .release
                .overall_timeout_ms
                .min(self.policy.release_timeout_ms),
            ..self.policy.release
        };
        let mut orders = [None, None];
        for leg in self.legs.iter() {
            let index = leg.handle.party.index();
            if leg.state == BearerState::Established && origin != Some(leg.handle.party) {
                orders[index] = Some(
                    TrafficOrderExchange::try_new_release(
                        leg.subscriber.identity,
                        ReleaseInformation {
                            fields: leg.return_channel.map_or(ReleaseFields::None, |channel| {
                                ReleaseFields::ControlChannels(vec![channel])
                            }),
                            ..Default::default()
                        },
                        policy,
                        monotonic_ms,
                    )
                    .map_err(SessionError::Order)?,
                );
            }
        }
        self.state = CallState::Releasing(reason);
        self.release_deadline_monotonic_ms = Some(deadline);
        self.alert = None;
        for leg in self.legs.iter_mut() {
            let order = orders[leg.handle.party.index()].take();
            leg.measurements = None;
            leg.extended_measurements = None;
            leg.flash.cancel();
            leg.service.cancel();
            leg.responses.clear();
            leg.close_status();
            leg.close_capabilities();
            leg.close_physical_control();
            leg.release = order;
            if leg.state != BearerState::Established {
                leg.request_stop();
            } else if origin == Some(leg.handle.party) {
                leg.queue_ack(
                    TrafficMessage::Release {
                        information: Default::default(),
                    }
                    .message_type(),
                )?;
            }
        }
        self.finish_stopped();
        Ok(())
    }
    fn finish_stopped(&mut self) {
        if let CallState::Releasing(reason) = self.state
            && self
                .legs
                .iter()
                .all(|leg| leg.state == BearerState::Stopped)
        {
            self.state = CallState::Stopped(reason);
        }
    }
    pub(crate) fn check_bearer(&self, handle: &BearerHandle) -> Result<(), SessionError> {
        if self
            .legs
            .get(handle.party.index())
            .is_none_or(|leg| !Arc::ptr_eq(&leg.handle.token, &handle.token))
        {
            return Err(SessionError::StaleBearer);
        }
        Ok(())
    }
    fn check_party(&self, party: Party) -> Result<(), SessionError> {
        if self.legs.get(party.index()).is_some() {
            Ok(())
        } else {
            Err(SessionError::State)
        }
    }
    fn check_clock(&self, monotonic_ms: u64) -> Result<(), SessionError> {
        if monotonic_ms < self.last_monotonic_ms {
            return Err(SessionError::ClockRegression);
        }
        Ok(())
    }
}

#[cfg(test)]
mod audio_tests;
#[cfg(test)]
mod tests;
