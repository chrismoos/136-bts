use super::{
    AcknowledgedControlExchange, BLOCK_DURATION_NS, Error, Exchange, State, ThresholdExchange,
    ThresholdState, Time, control::ControlUpdate,
};
use crate::{
    l3::{
        broadcast::ProtocolVersion,
        legacy_traffic::{
            ActiveAmrMode, AmrActiveSet, AmrCodecMode, AmrConfiguration, AmrLink, AmrPhase,
            AmrThresholds,
        },
    },
    link::rach::MobileIdentity,
};
use std::sync::Arc;

const DEFAULT_ACTIVE_SET: u8 = 0x10;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownlinkPhase {
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkConfiguration {
    active_set: AmrActiveSet,
    initial_mode: ActiveAmrMode,
    thresholds: Option<AmrThresholds>,
}
impl LinkConfiguration {
    pub fn active_set(&self) -> AmrActiveSet {
        self.active_set
    }
    pub fn initial_mode(&self) -> ActiveAmrMode {
        self.initial_mode
    }
    pub fn initial_codec(&self) -> AmrCodecMode {
        self.active_set
            .codec_mode(self.initial_mode)
            .expect("validated initial mode belongs to the active set")
    }
    /// TIA-136-440-A §6.1.2 leaves thresholds undefined in the default one-mode configuration.
    pub fn thresholds(&self) -> Option<&AmrThresholds> {
        self.thresholds.as_ref()
    }
}

/// One context belongs to one negotiated AMR bearer. Retire it after an uncertain or aborted configuration.
pub struct ConfigurationContext {
    recipient: MobileIdentity,
    version: ProtocolVersion,
    uplink: LinkConfiguration,
    downlink: LinkConfiguration,
    downlink_phase: DownlinkPhase,
    pending: Option<Arc<()>>,
    last_time: Option<Time>,
}
impl ConfigurationContext {
    pub fn try_new(recipient: MobileIdentity, version: ProtocolVersion) -> Result<Self, Error> {
        let (_, width, value) = recipient.parts();
        if value == 0 || value >= 1 << width {
            return Err(Error::Identity);
        }
        let configuration = AmrConfiguration {
            countdown: false,
            link: AmrLink::Both,
            initial_mode: ActiveAmrMode::Mode1,
            active_set: AmrActiveSet::try_new(DEFAULT_ACTIVE_SET).map_err(Error::Codec)?,
            thresholds: [0; 3],
            common_hysteresis: 0,
        };
        crate::l3::legacy_traffic::AmrControlMessage::Configuration(configuration)
            .encode_for_version(version)
            .map_err(Error::Codec)?;
        // TIA-136-440-A §6.1.2 requires MR74 in both directions until configuration arrives.
        let default = LinkConfiguration {
            active_set: configuration.active_set,
            initial_mode: configuration.initial_mode,
            thresholds: None,
        };
        Ok(Self {
            recipient,
            version,
            uplink: default,
            downlink: default,
            downlink_phase: DownlinkPhase::Odd,
            pending: None,
            last_time: None,
        })
    }
    pub fn uplink(&self) -> &LinkConfiguration {
        &self.uplink
    }
    pub fn downlink(&self) -> &LinkConfiguration {
        &self.downlink
    }
    fn check_clock(&self, now: Time) -> Result<(), Error> {
        if now.block_offset_ns >= BLOCK_DURATION_NS {
            return Err(Error::BlockOffset);
        }
        if self.last_time.is_some_and(|previous| {
            (now.fdtc_block, now.block_offset_ns) < (previous.fdtc_block, previous.block_offset_ns)
                || now.monotonic_ms < previous.monotonic_ms
        }) {
            return Err(Error::ClockRegression);
        }
        Ok(())
    }
    fn owns(&self, token: &Arc<()>) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, token))
    }
    /// Fence queued transmissions before cancellation. A transmitted configuration keeps the context reserved until bearer retirement.
    pub fn cancel(&mut self, exchange: &mut Exchange, now: Time) -> Result<State, Error> {
        let token = exchange.context_token.as_ref().ok_or(Error::Context)?;
        if !self.owns(token) {
            return Err(Error::Context);
        }
        self.check_clock(now)?;
        let state = exchange.cancel(now)?;
        if state == State::Cancelled && exchange.transmissions() == 0 {
            self.pending = None;
            self.last_time = Some(now);
        }
        Ok(state)
    }
    pub fn prepare(
        &mut self,
        configuration: AmrConfiguration,
        now: Time,
    ) -> Result<Exchange, Error> {
        self.check_clock(now)?;
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        let mut exchange = Exchange::try_new(self.recipient, configuration, self.version, now)?;
        let token = Arc::new(());
        exchange.context_token = Some(token.clone());
        self.pending = Some(token);
        Ok(exchange)
    }
    pub fn prepare_thresholds(
        &mut self,
        thresholds: AmrThresholds,
        now: Time,
    ) -> Result<ThresholdExchange, Error> {
        self.check_clock(now)?;
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        thresholds
            .validate_for_active_set(self.downlink.active_set)
            .map_err(Error::Codec)?;
        let token = Arc::new(());
        let exchange = ThresholdExchange::try_new(
            self.recipient,
            ControlUpdate::Thresholds(thresholds),
            self.version,
            token.clone(),
            now,
        )?;
        self.pending = Some(token);
        Ok(exchange)
    }
    pub fn downlink_phase(&self) -> DownlinkPhase {
        self.downlink_phase
    }
    pub fn prepare_phase(
        &mut self,
        phase: AmrPhase,
        now: Time,
    ) -> Result<AcknowledgedControlExchange, Error> {
        self.check_clock(now)?;
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        let token = Arc::new(());
        let exchange = AcknowledgedControlExchange::try_new(
            self.recipient,
            ControlUpdate::Phase(phase),
            self.version,
            token.clone(),
            now,
        )?;
        self.pending = Some(token);
        Ok(exchange)
    }
    /// Fence queued transmissions first. Cancellation after transmission leaves remote state uncertain until bearer retirement.
    pub fn cancel_control(
        &mut self,
        exchange: &mut AcknowledgedControlExchange,
        now: Time,
    ) -> Result<super::ControlState, Error> {
        if !self.owns(exchange.context_token()) {
            return Err(Error::Context);
        }
        self.check_clock(now)?;
        let state = exchange.cancel(now)?;
        if state == super::ControlState::Cancelled && !exchange.was_transmitted() {
            self.pending = None;
            self.last_time = Some(now);
        }
        Ok(state)
    }
    pub(super) fn apply_phase(
        &mut self,
        token: &Arc<()>,
        phase: AmrPhase,
        now: Time,
    ) -> Result<(), Error> {
        self.check_clock(now)?;
        if !self.owns(token) {
            return Err(Error::Context);
        }
        // TIA-136-133-E §5.1.2.3.22 selects odd/even or preserves the current FDTC phase.
        match phase {
            AmrPhase::DefaultOdd => self.downlink_phase = DownlinkPhase::Odd,
            AmrPhase::OppositeEven => self.downlink_phase = DownlinkPhase::Even,
            AmrPhase::Maintain => {}
        }
        self.pending = None;
        self.last_time = Some(now);
        Ok(())
    }
    /// Fence queued transmissions first. A sent threshold command requires retiring an uncertain bearer after cancellation.
    pub fn cancel_thresholds(
        &mut self,
        exchange: &mut ThresholdExchange,
        now: Time,
    ) -> Result<ThresholdState, Error> {
        if !exchange.is_thresholds() {
            return Err(Error::ControlKind);
        }
        self.cancel_control(exchange, now)
    }
    pub(super) fn apply_thresholds(
        &mut self,
        token: &Arc<()>,
        thresholds: AmrThresholds,
        now: Time,
    ) -> Result<(), Error> {
        self.check_clock(now)?;
        if !self.owns(token) {
            return Err(Error::Context);
        }
        thresholds
            .validate_for_active_set(self.downlink.active_set)
            .map_err(Error::Codec)?;
        // TIA-136-133-E §9.1.2.2.38 modifies only the mobile's FDTC mode-request thresholds.
        self.downlink.thresholds = Some(thresholds);
        self.pending = None;
        self.last_time = Some(now);
        Ok(())
    }
    pub(super) fn recipient(&self) -> MobileIdentity {
        self.recipient
    }
    pub(super) fn apply(
        &mut self,
        token: &Arc<()>,
        configuration: AmrConfiguration,
        now: Time,
    ) -> Result<(), Error> {
        self.check_clock(now)?;
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| !Arc::ptr_eq(pending, token))
        {
            return Err(Error::Context);
        }
        let configured = LinkConfiguration {
            active_set: configuration.active_set,
            initial_mode: configuration.initial_mode,
            thresholds: Some(AmrThresholds {
                lower: configuration.thresholds,
                hysteresis: [configuration.common_hysteresis; 3],
            }),
        };
        // TIA-136-440-A §6.1.3 applies each link bit independently.
        if matches!(configuration.link, AmrLink::Uplink | AmrLink::Both) {
            self.uplink = configured;
        }
        if matches!(configuration.link, AmrLink::Downlink | AmrLink::Both) {
            self.downlink = configured;
        }
        self.pending = None;
        self.last_time = Some(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
