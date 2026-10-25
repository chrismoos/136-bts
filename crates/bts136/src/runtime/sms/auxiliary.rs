use super::*;
use crate::{
    core::{delivery::AuxiliaryReply, registry::RegistrationHandle},
    l3::{legacy_traffic::TrafficRelay, relay::addressing::AddressedRelay},
    radio::carrier::{Carrier, QueueFence},
};

#[derive(Debug, Clone)]
pub struct AuxiliaryOutcome {
    pub reply: AuxiliaryReply,
    pub observed_monotonic_ms: u64,
}
struct Job {
    packet: AuxiliaryPacket,
    registration: RegistrationHandle,
    route: Route,
    traffic_token: Option<TrafficToken>,
    deadline_ms: u64,
    cancel_requested: bool,
    terminal_observed_ms: Option<u64>,
    fence: Option<QueueFence>,
}
#[derive(Default)]
pub(super) struct AuxiliaryQueues {
    jobs: Vec<Job>,
    outcomes: VecDeque<AuxiliaryOutcome>,
}
impl AuxiliaryQueues {
    pub(super) fn outcome(&self, packet: &AuxiliaryPacket) -> Option<AuxiliaryState> {
        self.outcomes
            .iter()
            .find(|outcome| outcome.reply.packet == *packet)
            .map(|outcome| outcome.reply.state)
    }
    pub(super) fn take_outcome(&mut self, packet: &AuxiliaryPacket) -> Option<AuxiliaryOutcome> {
        let position = self
            .outcomes
            .iter()
            .position(|outcome| outcome.reply.packet == *packet)?;
        self.outcomes.remove(position)
    }
}
impl SmsRuntime {
    pub fn send_auxiliary(
        &mut self,
        recipient: MobileIdentity,
        relay: AddressedRelay,
        now_ms: u64,
    ) -> Result<AuxiliaryPacket, Failure> {
        self.observe(now_ms)?;
        if self.auxiliary.jobs.len() + self.auxiliary.outcomes.len() >= MAX_PENDING
            || self.pending.len() + self.control_arq.len() + self.traffic.pending.len()
                >= MAX_PENDING
        {
            return Err(Failure::QueueFull);
        }
        let registration = self
            .core()
            .registry()
            .registration_handle(recipient)
            .ok_or(Failure::Delivery(DeliveryError::RecipientOffline))?;
        if matches!(
            relay.message,
            RelayMessage::Data {
                protocol: relay::HigherLayerProtocol::Segmented(_),
                ..
            }
        ) {
            let peer_version = self
                .core()
                .registry()
                .subscribers()
                .iter()
                .find(|subscriber| subscriber.provisioning.identity == recipient)
                .and_then(|subscriber| subscriber.registration)
                .ok_or(Failure::Delivery(DeliveryError::RecipientOffline))?
                .protocol_version;
            if crate::runtime::capabilities::SERVING_PROTOCOL
                .common(peer_version)
                .code()
                < crate::l3::broadcast::ProtocolVersion::Pv1.code()
            {
                return Err(Failure::Wire);
            }
        }
        let route = match self
            .traffic
            .peers
            .iter()
            .find(|peer| peer.identity == recipient)
        {
            Some(peer) if !peer.ready => return Err(Failure::Delivery(DeliveryError::Busy)),
            Some(peer) => Route::Traffic(peer.call_id),
            None => Route::Control,
        };
        self.delivery
            .sync_subscribers(now_ms)
            .map_err(Failure::Delivery)?;
        let transport = match route {
            Route::Control => AuxiliaryTransport::Control,
            Route::Traffic(_) => AuxiliaryTransport::Traffic,
        };
        let packet = self
            .delivery
            .prepare_auxiliary(recipient, relay, transport, now_ms)
            .map_err(Failure::Delivery)?;
        let queued = match route {
            Route::Control => packet
                .relay()
                .encode(Direction::Forward)
                .map_err(|_| Failure::Wire)
                .and_then(|bytes| {
                    self.enqueue_owned(
                        recipient,
                        bytes,
                        ForwardOwner::Auxiliary(packet.clone()),
                        now_ms,
                    )
                }),
            Route::Traffic(call_id) => TrafficRelay::from_addressed(packet.relay())
                .map_err(|_| Failure::Wire)
                .and_then(|relay| {
                    self.enqueue_traffic_owned(
                        recipient,
                        call_id,
                        relay,
                        ForwardOwner::Auxiliary(packet.clone()),
                        now_ms,
                    )
                }),
        };
        if let Err(error) = queued {
            self.delivery
                .cancel_auxiliary_fenced(&packet, now_ms)
                .map_err(Failure::Delivery)?;
            return Err(error);
        }
        let (deadline_ms, traffic_token) = match route {
            Route::Control => {
                let pending = self
                    .pending
                    .back()
                    .expect("successful control admission appends its owner");
                (pending.deadline_ms, None)
            }
            Route::Traffic(_) => {
                let pending = self
                    .traffic
                    .pending
                    .back()
                    .expect("successful traffic admission appends its owner");
                (pending.deadline_ms, Some(pending.offer.token))
            }
        };
        self.auxiliary.jobs.push(Job {
            packet: packet.clone(),
            registration,
            route,
            traffic_token,
            deadline_ms,
            cancel_requested: false,
            terminal_observed_ms: None,
            fence: None,
        });
        Ok(packet)
    }
    pub fn auxiliary_state(&self, packet: &AuxiliaryPacket) -> Result<AuxiliaryState, Failure> {
        if let Some(outcome) = self
            .auxiliary
            .outcomes
            .iter()
            .find(|outcome| outcome.reply.packet == *packet)
        {
            return Ok(outcome.reply.state);
        }
        self.delivery
            .auxiliary_state(packet)
            .map_err(Failure::Delivery)
    }
    pub fn cancel_auxiliary(
        &mut self,
        packet: &AuxiliaryPacket,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        let job = self
            .auxiliary
            .jobs
            .iter_mut()
            .find(|job| job.packet == *packet)
            .ok_or(Failure::Delivery(DeliveryError::Auxiliary(
                crate::core::delivery::AuxiliaryError::StalePacket,
            )))?;
        job.cancel_requested = true;
        self.advance_auxiliary_jobs(now_ms)
    }
    pub fn take_auxiliary_outcome(&mut self) -> Option<AuxiliaryOutcome> {
        let position = self.auxiliary.outcomes.iter().position(|outcome| {
            !self.owns_incoming_response(&outcome.reply.packet)
                && !self.owns_outgoing_fragment(&outcome.reply.packet)
        })?;
        self.auxiliary.outcomes.remove(position)
    }
    pub(super) fn auxiliary_observed(&mut self, packet: &AuxiliaryPacket, now_ms: u64) {
        if let Some(job) = self
            .auxiliary
            .jobs
            .iter_mut()
            .find(|job| job.packet == *packet)
        {
            job.terminal_observed_ms.get_or_insert(now_ms);
        }
    }
    pub(super) fn request_auxiliary_cancel(&mut self, packet: &AuxiliaryPacket) {
        if let Some(job) = self
            .auxiliary
            .jobs
            .iter_mut()
            .find(|job| job.packet == *packet)
        {
            job.cancel_requested = true;
        }
    }
    fn route_current(&self, job: &Job) -> bool {
        if !self
            .core()
            .registry()
            .registration_current(&job.registration)
        {
            return false;
        }
        match job.route {
            Route::Control => !self
                .traffic
                .peers
                .iter()
                .any(|peer| peer.identity == job.packet.recipient()),
            Route::Traffic(call_id) => self
                .traffic
                .peers
                .iter()
                .any(|peer| peer.identity == job.packet.recipient() && peer.call_id == call_id),
        }
    }
    pub(super) fn auxiliary_active(&self, packet: &AuxiliaryPacket) -> bool {
        self.auxiliary
            .jobs
            .iter()
            .find(|job| job.packet == *packet)
            .is_some_and(|job| {
                !job.cancel_requested
                    && self.route_current(job)
                    && self
                        .delivery
                        .auxiliary_state(packet)
                        .is_ok_and(|state| !state.closed())
            })
    }
    pub(super) fn auxiliary_handed_off(
        &mut self,
        packet: &AuxiliaryPacket,
        handed_off_ms: u64,
        now_ms: u64,
    ) -> Result<(), Failure> {
        if !self.auxiliary_active(packet) {
            return Ok(());
        }
        if self
            .delivery
            .auxiliary_state(packet)
            .map_err(Failure::Delivery)?
            == AuxiliaryState::Prepared
        {
            self.delivery
                .auxiliary_submitted(packet, handed_off_ms, now_ms)
                .map_err(Failure::Delivery)?;
        }
        Ok(())
    }
    pub(crate) fn auxiliary_transmitted(
        &mut self,
        packet: &AuxiliaryPacket,
        completed_ms: u64,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        if !self.auxiliary.jobs.iter().any(|job| job.packet == *packet) {
            return Ok(());
        }
        self.auxiliary_completed(packet, completed_ms, now_ms)?;
        if let Some(log) = &self.protocol {
            let bytes = packet
                .relay()
                .encode(Direction::Forward)
                .map_err(|_| Failure::Wire)?;
            log.message(
                "Forward",
                "SMSCH",
                packet.recipient(),
                &bytes,
                "TX completion observed",
            );
        }
        Ok(())
    }
    pub(super) fn cancel_all_auxiliary(&mut self) {
        for job in &mut self.auxiliary.jobs {
            job.cancel_requested = true;
        }
    }
    pub(super) fn auxiliary_completed(
        &mut self,
        packet: &AuxiliaryPacket,
        completed_ms: u64,
        now_ms: u64,
    ) -> Result<(), Failure> {
        if !self.auxiliary_active(packet) {
            return Ok(());
        }
        if matches!(
            self.delivery
                .auxiliary_state(packet)
                .map_err(Failure::Delivery)?,
            AuxiliaryState::AwaitingResponse { .. }
        ) {
            return Ok(());
        }
        if completed_ms > now_ms {
            return Err(Failure::Clock);
        }
        if self
            .auxiliary
            .jobs
            .iter()
            .any(|job| job.packet == *packet && completed_ms >= job.deadline_ms)
        {
            self.request_auxiliary_cancel(packet);
            return Ok(());
        }
        let timeout_ms = match packet.transport() {
            AuxiliaryTransport::Control => super::super::timers::SPACH_MS,
            AuxiliaryTransport::Traffic => traffic::SACCH_RESPONSE_MS,
        };
        self.delivery
            .auxiliary_transmitted(packet, completed_ms, now_ms, timeout_ms)
            .map_err(Failure::Delivery)?;
        Ok(())
    }
    pub(crate) fn traffic_handed_off(
        &mut self,
        token: TrafficToken,
        handed_off_ms: u64,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        let owner = self
            .traffic
            .pending
            .iter()
            .find(|pending| pending.offer.token == token && pending.queued)
            .map(|pending| pending.owner.clone());
        match owner {
            Some(ForwardOwner::Auxiliary(packet)) => {
                self.auxiliary_handed_off(&packet, handed_off_ms, now_ms)?
            }
            Some(ForwardOwner::Delivery(prepared)) => {
                self.delivery.submitted(&prepared);
            }
            Some(ForwardOwner::TrackedReply(packet)) => {
                self.reply_handed_off(&packet, handed_off_ms, now_ms)?
            }
            Some(ForwardOwner::Reply) | None => {}
        }
        Ok(())
    }
    pub(crate) fn advance_auxiliary_jobs(&mut self, now_ms: u64) -> Result<(), Failure> {
        self.observe(now_ms)?;
        for index in 0..self.auxiliary.jobs.len() {
            let packet = self.auxiliary.jobs[index].packet.clone();
            let state = self
                .delivery
                .advance_auxiliary(&packet, now_ms)
                .map_err(Failure::Delivery)?;
            if state.closed() {
                self.auxiliary.jobs[index]
                    .terminal_observed_ms
                    .get_or_insert(now_ms);
            }
            if !self.route_current(&self.auxiliary.jobs[index])
                || now_ms >= self.auxiliary.jobs[index].deadline_ms
                    && matches!(state, AuxiliaryState::Prepared | AuxiliaryState::Submitted)
            {
                self.auxiliary.jobs[index].cancel_requested = true;
            }
        }
        let inactive: Vec<_> = self
            .auxiliary
            .jobs
            .iter()
            .filter(|job| !self.auxiliary_active(&job.packet))
            .map(|job| job.packet.clone())
            .collect();
        let before = self.pending.len();
        self.pending.retain(|pending| {
            pending
                .owner
                .auxiliary()
                .is_none_or(|packet| !inactive.contains(packet))
        });
        if self.pending.len() != before {
            self.selected = None;
        }
        self.traffic.pending.retain(|pending| {
            pending
                .owner
                .auxiliary()
                .is_none_or(|packet| !inactive.contains(packet))
        });
        Ok(())
    }
    /// Cancel external ARQ sources and process traffic withdrawal before capturing a radio fence.
    pub(crate) fn settle_auxiliary_fences<T>(
        &mut self,
        carrier: &Carrier<T>,
        voice: &super::super::voice::VoiceRuntime,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.advance_auxiliary_jobs(now_ms)?;
        let mut index = 0;
        while index < self.auxiliary.jobs.len() {
            let job = &self.auxiliary.jobs[index];
            if self.auxiliary_active(&job.packet)
                || self
                    .pending
                    .iter()
                    .any(|pending| pending.owner.auxiliary() == Some(&job.packet))
                || self
                    .control_arq
                    .iter()
                    .any(|transfer| transfer.message.owner.auxiliary() == Some(&job.packet))
                || self
                    .traffic
                    .pending
                    .iter()
                    .any(|pending| pending.owner.auxiliary() == Some(&job.packet))
                || job
                    .traffic_token
                    .is_some_and(|token| voice.retains_sms(token))
            {
                index += 1;
                continue;
            }
            let job = &mut self.auxiliary.jobs[index];
            let fence = job.fence.get_or_insert_with(|| carrier.queue_fence());
            if !carrier
                .fence_complete(fence)
                .map_err(|error| Failure::Fence(Box::new(error)))?
            {
                index += 1;
                continue;
            }
            let state = self
                .delivery
                .auxiliary_state(&job.packet)
                .map_err(Failure::Delivery)?;
            if state.closed() {
                self.delivery
                    .retire_auxiliary_fenced(&job.packet, now_ms)
                    .map_err(Failure::Delivery)?;
            } else {
                self.delivery
                    .cancel_auxiliary_fenced(&job.packet, now_ms)
                    .map_err(Failure::Delivery)?;
            }
            let state = self
                .delivery
                .auxiliary_state(&job.packet)
                .map_err(Failure::Delivery)?;
            let job = self.auxiliary.jobs.remove(index);
            self.auxiliary.outcomes.push_back(AuxiliaryOutcome {
                reply: AuxiliaryReply {
                    packet: job.packet,
                    state,
                },
                observed_monotonic_ms: job.terminal_observed_ms.unwrap_or(now_ms),
            });
        }
        Ok(())
    }
    pub(super) fn auxiliary_response_route(
        &self,
        identity: MobileIdentity,
        transaction_id: u8,
        route: Route,
    ) -> Result<(), Failure> {
        if let Some(job) = self.auxiliary.jobs.iter().find(|job| {
            job.packet.recipient() == identity && job.packet.transaction_id() == transaction_id
        }) && (job.cancel_requested || !self.route_current(job) || job.route != route)
        {
            return Err(Failure::Wire);
        }
        Ok(())
    }
}
