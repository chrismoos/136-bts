use crate::{
    core::registry::{LocalRegistry, RegistrationHandle, SubscriberHandle},
    link::{
        rach::MobileIdentity,
        spach::{Channel, HeaderFlags, SingleMessage},
    },
    phy::control::INFORMATION_BITS,
    scheduler::paging::PagingAllocation,
};
use std::{collections::VecDeque, sync::Arc};
const MAX_POLLS: usize = 32;
const TDMA_BLOCKS_PER_SUPERFRAME: u64 = 32;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueryKind {
    Capabilities,
    MessageWaiting(crate::l3::message_waiting::MessageWaiting),
    UserAlert(crate::l3::user_alert::UserAlert),
    Audit {
        force_registration: bool,
        debug_display_allowed: bool,
    },
}
impl QueryKind {
    fn indication_type(&self) -> Option<u8> {
        match self {
            Self::MessageWaiting(_) => Some(crate::l3::message_waiting::MESSAGE_TYPE),
            Self::UserAlert(_) => Some(crate::l3::user_alert::MESSAGE_TYPE),
            _ => None,
        }
    }

    fn same_procedure(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Capabilities, Self::Capabilities)
                | (Self::MessageWaiting(_), Self::MessageWaiting(_))
                | (Self::UserAlert(_), Self::UserAlert(_))
                | (Self::Audit { .. }, Self::Audit { .. })
        )
    }
    fn bytes(&self) -> Result<Vec<u8>, PollError> {
        match self {
            Self::UserAlert(message) => message.encode().map_err(|_| PollError::Framing),
            Self::MessageWaiting(message) => message.encode().map_err(|_| PollError::Framing),
            Self::Capabilities => Ok(crate::l3::capability_report::request().to_vec()),
            Self::Audit {
                force_registration,
                debug_display_allowed,
            } => crate::l3::audit::AuditOrder {
                force_registration: *force_registration,
                debug_display_allowed: *debug_display_allowed,
                ..Default::default()
            }
            .encode()
            .map_err(|_| PollError::Framing),
        }
    }
}

const RESPONSE_TIMEOUT_MS: u64 = 30_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollState {
    Queued,
    InFlight,
    AwaitingReport { deadline_ms: u64 },
    Received,
    TimedOut,
    Cancelled,
}
impl PollState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::InFlight => "Sending",
            Self::AwaitingReport { .. } => "Waiting for the phone",
            Self::Received => "Answered",
            Self::TimedOut => "No answer",
            Self::Cancelled => "Cancelled",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollError {
    UnknownSubscriber,
    Offline,
    Busy,
    Full,
    Identity,
    Framing,
    ResponseFields,
    ClockRegression,
    Overflow,
}
impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "idle query failed: {self:?}")
    }
}
impl std::error::Error for PollError {}
#[derive(Clone, Debug)]
pub(crate) struct Completion {
    pub identity: MobileIdentity,
    pub bytes: Vec<u8>,
    token: Arc<()>,
    frame: usize,
    final_frame: bool,
}
impl Completion {
    pub fn final_frame(&self) -> bool {
        self.final_frame
    }
}
struct Poll {
    kind: QueryKind,
    confirmation: Option<crate::l3::audit::AuditConfirmation>,
    forced_registration_received: bool,
    completion: Completion,
    provisioning: Option<SubscriberHandle>,
    registration: Option<RegistrationHandle>,
    phase: u8,
    information: Vec<[bool; INFORMATION_BITS]>,
    next_frame: usize,
    due: Option<(u64, u8)>,
    receipts: Vec<bool>,
    reuse_after_ms: u64,
    state: PollState,
    queue_deadline_ms: u64,
}
impl Poll {
    fn provisioning_current(&self, registry: &LocalRegistry) -> bool {
        self.provisioning.as_ref().map_or_else(
            || {
                registry
                    .subscriber_handle(self.completion.identity)
                    .is_none()
            },
            |handle| registry.subscriber_current(handle),
        )
    }
    fn pending(&self) -> bool {
        matches!(
            self.state,
            PollState::Queued | PollState::InFlight | PollState::AwaitingReport { .. }
        )
    }
}
struct Repeat {
    superframe: u64,
    phase: u8,
    information: [bool; INFORMATION_BITS],
}
enum Selected {
    Primary(usize, u64),
    Repeat(usize),
}
pub(crate) struct IdleQueries {
    allocation: PagingAllocation,
    polls: VecDeque<Poll>,
    registration_requests: VecDeque<MobileIdentity>,
    repeats: VecDeque<Repeat>,
    selected: Option<Selected>,
    last_ms: u64,
    startup_until_block: u64,
}
impl IdleQueries {
    pub fn new(allocation: PagingAllocation) -> Self {
        Self {
            allocation,
            polls: VecDeque::new(),
            registration_requests: VecDeque::new(),
            repeats: VecDeque::new(),
            selected: None,
            last_ms: 0,
            startup_until_block: 0,
        }
    }
    pub fn after_restart(
        allocation: PagingAllocation,
        policy: super::spach_arq::reuse::ReusePolicy,
    ) -> Self {
        let mut queries = Self::new(allocation);
        queries.startup_until_block = policy.quiet_blocks();
        queries
    }
    pub fn state(&self, identity: MobileIdentity) -> Option<PollState> {
        self.polls
            .iter()
            .find(|poll| {
                poll.completion.identity == identity && poll.kind == QueryKind::Capabilities
            })
            .map(|poll| poll.state)
    }
    pub fn audit_state(&self, identity: MobileIdentity) -> Option<PollState> {
        self.polls
            .iter()
            .find(|poll| {
                poll.completion.identity == identity && matches!(poll.kind, QueryKind::Audit { .. })
            })
            .map(|poll| poll.state)
    }
    pub fn audit_confirmation(
        &self,
        identity: MobileIdentity,
    ) -> Option<&crate::l3::audit::AuditConfirmation> {
        self.polls
            .iter()
            .find(|poll| {
                poll.completion.identity == identity && matches!(poll.kind, QueryKind::Audit { .. })
            })
            .and_then(|poll| poll.confirmation.as_ref())
    }
    pub fn forced_registration_received(&self, identity: MobileIdentity) -> Option<bool> {
        self.polls
            .iter()
            .find(|poll| {
                poll.completion.identity == identity
                    && matches!(
                        poll.kind,
                        QueryKind::Audit {
                            force_registration: true,
                            ..
                        }
                    )
            })
            .map(|poll| poll.forced_registration_received)
    }
    pub fn request(
        &mut self,
        registry: &LocalRegistry,
        identity: MobileIdentity,
        busy: bool,
        now: u64,
    ) -> Result<(), PollError> {
        self.request_kind(registry, identity, QueryKind::Capabilities, busy, now)
    }
    pub fn request_audit(
        &mut self,
        registry: &LocalRegistry,
        identity: MobileIdentity,
        order: crate::l3::audit::AuditOrder,
        busy: bool,
        now: u64,
    ) -> Result<(), PollError> {
        if order.subaddress.is_some() || !order.unparsed_bits.is_empty() {
            return Err(PollError::Framing);
        }
        self.request_kind(
            registry,
            identity,
            QueryKind::Audit {
                force_registration: order.force_registration,
                debug_display_allowed: order.debug_display_allowed,
            },
            busy,
            now,
        )
    }
    pub fn request_message_waiting(
        &mut self,
        registry: &LocalRegistry,
        identity: MobileIdentity,
        message: crate::l3::message_waiting::MessageWaiting,
        busy: bool,
        now: u64,
    ) -> Result<(), PollError> {
        message
            .validate_version(crate::l3::broadcast::ProtocolVersion::Is136RevA)
            .map_err(|_| PollError::Framing)?;
        if message.subaddress.is_some()
            || !message.unparsed_bits.is_empty()
            || !message.ignored_bits.is_empty()
        {
            return Err(PollError::Framing);
        }
        self.request_kind(
            registry,
            identity,
            QueryKind::MessageWaiting(message),
            busy,
            now,
        )
    }
    pub fn request_user_alert(
        &mut self,
        registry: &LocalRegistry,
        identity: MobileIdentity,
        message: crate::l3::user_alert::UserAlert,
        busy: bool,
        now: u64,
    ) -> Result<(), PollError> {
        message
            .validate_version(crate::l3::broadcast::ProtocolVersion::Is136RevA)
            .map_err(|_| PollError::Framing)?;
        if message.subaddress.is_some()
            || !message.unparsed_bits.is_empty()
            || !message.ignored_bits.is_empty()
        {
            return Err(PollError::Framing);
        }
        self.request_kind(registry, identity, QueryKind::UserAlert(message), busy, now)
    }
    pub fn user_alert_state(&self, identity: MobileIdentity) -> Option<PollState> {
        self.polls
            .iter()
            .find(|poll| {
                poll.completion.identity == identity && matches!(poll.kind, QueryKind::UserAlert(_))
            })
            .map(|poll| poll.state)
    }
    pub fn message_waiting_state(&self, identity: MobileIdentity) -> Option<PollState> {
        self.polls
            .iter()
            .find(|poll| {
                poll.completion.identity == identity
                    && matches!(poll.kind, QueryKind::MessageWaiting(_))
            })
            .map(|poll| poll.state)
    }
    fn request_kind(
        &mut self,
        registry: &LocalRegistry,
        identity: MobileIdentity,
        kind: QueryKind,
        busy: bool,
        now: u64,
    ) -> Result<(), PollError> {
        self.advance(registry, |candidate| candidate == identity && busy, now)?;
        let provisioning = registry.subscriber_handle(identity);
        if provisioning.is_none()
            && !matches!(
                kind,
                QueryKind::Audit {
                    force_registration: true,
                    ..
                }
            )
        {
            return Err(PollError::UnknownSubscriber);
        }
        let registration = registry.registration_handle(identity);
        if !matches!(kind, QueryKind::Audit { .. }) && registration.is_none() {
            return Err(PollError::Offline);
        }
        if busy {
            return Err(PollError::Busy);
        }
        if let Some(pending) = self
            .polls
            .iter()
            .find(|poll| poll.completion.identity == identity && poll.pending())
        {
            return if pending.kind == kind {
                Ok(())
            } else {
                Err(PollError::Busy)
            };
        }
        if kind.indication_type().is_some()
            && self.polls.iter().any(|poll| {
                poll.completion.identity == identity
                    && poll.kind.same_procedure(&kind)
                    && now < poll.reuse_after_ms
            })
        {
            return Err(PollError::Busy);
        }
        let phase = self
            .allocation
            .phase(identity)
            .map_err(|_| PollError::Identity)?;
        let queue_deadline_ms = now
            .checked_add(super::timers::SPACH_MS)
            .ok_or(PollError::Overflow)?;
        let bytes = kind.bytes()?;
        let frames = SingleMessage {
            channel: Channel::Pch,
            flags: HeaderFlags::default(),
            identity,
            message: bytes.clone(),
        }
        .encode_fragmented()
        .map_err(|_| PollError::Framing)?;

        let remove = self
            .polls
            .iter()
            .position(|poll| {
                poll.completion.identity == identity && poll.kind.same_procedure(&kind)
            })
            .or_else(|| {
                (self.polls.len() >= MAX_POLLS)
                    .then(|| {
                        self.polls
                            .iter()
                            .position(|poll| !poll.pending() && now >= poll.reuse_after_ms)
                    })
                    .flatten()
            });
        if remove.is_none() && self.polls.len() >= MAX_POLLS {
            return Err(PollError::Full);
        }
        if let Some(index) = remove {
            self.polls.remove(index);
        }
        self.polls.push_back(Poll {
            kind,
            confirmation: None,
            forced_registration_received: false,
            completion: Completion {
                identity,
                bytes,
                token: Arc::new(()),
                frame: 0,
                final_frame: frames.len() == 1,
            },
            provisioning,
            registration,
            phase,
            receipts: vec![false; frames.len()],
            information: frames,
            next_frame: 0,
            due: None,
            reuse_after_ms: 0,
            state: PollState::Queued,
            queue_deadline_ms,
        });
        Ok(())
    }
    pub(crate) fn registration_needed(
        &mut self,
        registry: &LocalRegistry,
        identity: MobileIdentity,
    ) {
        if registry.registration_handle(identity).is_some()
            || self.registration_requests.contains(&identity)
        {
            return;
        }
        if self.registration_requests.len() >= MAX_POLLS {
            tracing::warn!(%identity, "Registration solicitation queue is full");
            return;
        }
        self.registration_requests.push_back(identity);
        tracing::debug!(%identity, "Registration solicitation deferred until phone is idle");
    }
    pub(crate) fn solicit_registrations(
        &mut self,
        registry: &LocalRegistry,
        busy: impl Fn(MobileIdentity) -> bool,
        now: u64,
    ) -> Result<(), PollError> {
        self.clock(now)?;
        let mut index = 0;
        while index < self.registration_requests.len() {
            let identity = self.registration_requests[index];
            if registry.registration_handle(identity).is_some() {
                self.registration_requests.remove(index);
                continue;
            }
            if busy(identity) {
                index += 1;
                continue;
            }
            match self.request_audit(
                registry,
                identity,
                crate::l3::audit::AuditOrder {
                    force_registration: true,
                    ..Default::default()
                },
                false,
                now,
            ) {
                Ok(()) => {
                    self.registration_requests.remove(index);
                    tracing::info!(%identity, "Requesting registration after unregistered call access");
                }
                Err(PollError::Busy | PollError::Full) => index += 1,
                Err(error) => {
                    self.registration_requests.remove(index);
                    tracing::warn!(%identity, %error, "Registration solicitation could not be queued");
                }
            }
        }
        Ok(())
    }
    pub fn advance(
        &mut self,
        registry: &LocalRegistry,
        busy: impl Fn(MobileIdentity) -> bool,
        now: u64,
    ) -> Result<(), PollError> {
        self.clock(now)?;
        for poll in &mut self.polls {
            if !poll.pending() {
                continue;
            }
            let registration_current = match &poll.registration {
                Some(handle) => registry.registration_current(handle),
                None => registry
                    .registration_handle(poll.completion.identity)
                    .is_none(),
            };
            if busy(poll.completion.identity)
                || !poll.provisioning_current(registry)
                || !registration_current
            {
                poll.state = PollState::Cancelled;
                continue;
            }
            match poll.state {
                PollState::Queued | PollState::InFlight if now >= poll.queue_deadline_ms => {
                    poll.state = PollState::TimedOut
                }
                PollState::AwaitingReport { deadline_ms } if now >= deadline_ms => {
                    poll.state = PollState::TimedOut
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub(crate) fn repeat_count(&self, superframe: u64, phase: u8) -> usize {
        self.repeats
            .iter()
            .filter(|repeat| {
                repeat.superframe > superframe
                    || repeat.superframe == superframe && repeat.phase >= phase
            })
            .count()
    }
    pub fn has_repeat(&self, superframe: u64, phase: u8) -> bool {
        self.repeats
            .iter()
            .any(|repeat| repeat.superframe == superframe && repeat.phase == phase)
    }
    pub fn frame(&mut self, superframe: u64, phase: u8) -> Option<&[bool; INFORMATION_BITS]> {
        self.selected = None;
        self.repeats
            .retain(|repeat| superframe <= repeat.superframe);
        if let Some(index) = self
            .repeats
            .iter()
            .position(|repeat| repeat.superframe == superframe && repeat.phase == phase)
        {
            self.selected = Some(Selected::Repeat(index));
            return Some(&self.repeats[index].information);
        }
        if let Some(index) = self
            .polls
            .iter()
            .position(|poll| poll.state == PollState::InFlight && poll.due.is_some())
        {
            let due = self.polls[index]
                .due
                .expect("active continuation has a due opportunity");
            if (superframe, phase) > due {
                self.polls[index].state = PollState::Cancelled;
                self.polls[index].due = None;
                tracing::warn!(identity=%self.polls[index].completion.identity, "Idle PCH continuation missed. Cancelling the incomplete message");
            } else {
                if (superframe, phase) != due {
                    return None;
                }
                self.selected = Some(Selected::Primary(index, superframe));
                return Some(&self.polls[index].information[self.polls[index].next_frame]);
            }
        }
        if !superframe.is_multiple_of(2)
            || self
                .repeats
                .iter()
                .any(|repeat| repeat.phase == phase && repeat.superframe == superframe + 1)
        {
            return None;
        }
        let index = self.polls.iter().position(|poll| {
            poll.phase == phase
                && poll.state == PollState::Queued
                && (poll.kind.indication_type().is_none()
                    || superframe
                        .saturating_mul(TDMA_BLOCKS_PER_SUPERFRAME)
                        .saturating_add(u64::from(phase))
                        >= self.startup_until_block)
        })?;
        self.selected = Some(Selected::Primary(index, superframe));
        Some(&self.polls[index].information[self.polls[index].next_frame])
    }
    pub fn completion(&self) -> Option<Completion> {
        if let Some(Selected::Primary(index, _)) = self.selected {
            let mut completion = self.polls[index].completion.clone();
            completion.frame = self.polls[index].next_frame;
            completion.final_frame = completion.frame + 1 == self.polls[index].information.len();
            Some(completion)
        } else {
            None
        }
    }
    pub fn submitted(&mut self) -> Result<(), PollError> {
        match self.selected.take().ok_or(PollError::Framing)? {
            Selected::Primary(index, superframe) => {
                let poll = &mut self.polls[index];
                let phase = poll.due.map_or(poll.phase, |due| due.1);
                self.repeats.push_back(Repeat {
                    superframe: superframe.checked_add(1).ok_or(PollError::Overflow)?,
                    phase,
                    information: poll.information[poll.next_frame],
                });
                poll.next_frame += 1;
                if poll.kind.indication_type().is_some() {
                    poll.reuse_after_ms = self
                        .last_ms
                        .checked_add(RESPONSE_TIMEOUT_MS)
                        .ok_or(PollError::Overflow)?;
                }
                poll.state = PollState::InFlight;
                poll.due = if poll.next_frame < poll.information.len() {
                    Some(
                        self.allocation
                            .continuation(superframe, phase)
                            .ok_or(PollError::Overflow)?,
                    )
                } else {
                    None
                };
            }
            Selected::Repeat(index) => {
                self.repeats.remove(index);
            }
        }
        Ok(())
    }
    pub fn in_progress(&self) -> bool {
        self.polls
            .iter()
            .any(|poll| poll.state == PollState::InFlight && poll.due.is_some())
    }
    pub fn frame_count(&self) -> usize {
        match self.selected {
            Some(Selected::Primary(index, superframe)) => {
                let poll = &self.polls[index];
                let phase = poll.due.map_or(poll.phase, |due| due.1);
                self.allocation
                    .continuation_span(superframe, phase, poll.information.len() - poll.next_frame)
                    .unwrap_or(usize::MAX)
            }
            _ => 1,
        }
    }
    pub fn adds_repeats(&self) -> usize {
        match self.selected {
            Some(Selected::Primary(index, _)) => {
                self.polls[index].information.len() - self.polls[index].next_frame
            }
            _ => 0,
        }
    }
    pub fn received_indication(
        &mut self,
        identity: MobileIdentity,
        confirmation: &crate::l3::notification::SpachConfirmation,
        now: u64,
    ) -> Result<bool, PollError> {
        self.clock(now)?;
        if !matches!(
            confirmation.confirmed_type,
            crate::l3::message_waiting::MESSAGE_TYPE | crate::l3::user_alert::MESSAGE_TYPE
        ) || confirmation.subaddress.is_some()
            || !confirmation.unparsed_bits.is_empty()
        {
            return Err(PollError::ResponseFields);
        }
        if let Some(poll) = self.polls.iter_mut().find(|poll| {
            poll.completion.identity == identity
                && poll.kind.indication_type() == Some(confirmation.confirmed_type)
        }) {
            match poll.state {
                PollState::InFlight if poll.next_frame == poll.information.len() => {
                    poll.state = PollState::Received
                }
                PollState::AwaitingReport { deadline_ms } => {
                    poll.state = if now < deadline_ms {
                        PollState::Received
                    } else {
                        PollState::TimedOut
                    }
                }
                _ => return Ok(false),
            }
            return Ok(poll.state == PollState::Received);
        }
        Ok(false)
    }
    pub fn transmitted(&mut self, completion: &Completion, now: u64) -> Result<(), PollError> {
        self.clock(now)?;
        if let Some(poll) = self
            .polls
            .iter_mut()
            .find(|poll| Arc::ptr_eq(&poll.completion.token, &completion.token))
            && poll.state == PollState::InFlight
        {
            let receipt = poll
                .receipts
                .get_mut(completion.frame)
                .ok_or(PollError::Framing)?;
            *receipt = true;
            if poll.receipts.iter().all(|receipt| *receipt) {
                let deadline_ms = now
                    .checked_add(RESPONSE_TIMEOUT_MS)
                    .ok_or(PollError::Overflow)?;
                poll.reuse_after_ms = poll.reuse_after_ms.max(deadline_ms);
                poll.state = PollState::AwaitingReport { deadline_ms };
                if let QueryKind::Audit {
                    force_registration, ..
                } = poll.kind
                {
                    tracing::info!(identity=%poll.completion.identity, force_registration, raw_hex=%format!("{:02x?}", poll.completion.bytes), "Audit Order transmitted. Waiting for handset response");
                }
            }
        }
        Ok(())
    }
    pub fn received(&mut self, identity: MobileIdentity, now: u64) -> Result<(), PollError> {
        self.clock(now)?;
        if let Some(poll) = self.polls.iter_mut().find(|poll| {
            poll.completion.identity == identity && poll.kind == QueryKind::Capabilities
        }) && let PollState::AwaitingReport { deadline_ms } = poll.state
        {
            poll.state = if now < deadline_ms {
                PollState::Received
            } else {
                PollState::TimedOut
            };
        }
        Ok(())
    }
    pub fn received_audit(
        &mut self,
        identity: MobileIdentity,
        confirmation: crate::l3::audit::AuditConfirmation,
        now: u64,
    ) -> Result<(), PollError> {
        self.clock(now)?;
        if confirmation.encode().is_err() {
            return Err(PollError::ResponseFields);
        }
        let Some(poll) = self.polls.iter_mut().find(|poll| {
            poll.completion.identity == identity && matches!(poll.kind, QueryKind::Audit { .. })
        }) else {
            return Ok(());
        };
        let PollState::AwaitingReport { deadline_ms } = poll.state else {
            return Ok(());
        };
        if now >= deadline_ms {
            poll.state = PollState::TimedOut;
            return Ok(());
        }
        let QueryKind::Audit {
            force_registration,
            debug_display_allowed,
        } = poll.kind
        else {
            return Err(PollError::ResponseFields);
        };
        if (!debug_display_allowed
            && (confirmation.display.is_some() || confirmation.multilingual_display.is_some()))
            || confirmation.multilingual_display.is_some()
            || confirmation.group.is_some()
            || confirmation.selected_system.is_some()
        {
            return Err(PollError::ResponseFields);
        }
        if poll.confirmation.is_none() {
            poll.confirmation = Some(confirmation);
        }
        tracing::info!(
            %identity,
            force_registration,
            forced_registration_received = poll.forced_registration_received,
            "Audit Confirmation received"
        );
        if !force_registration || poll.forced_registration_received {
            poll.state = PollState::Received;
        }
        Ok(())
    }
    pub fn registration_received(
        &mut self,
        registry: &LocalRegistry,
        identity: MobileIdentity,
        kind: crate::l3::registration::RegistrationType,
        busy: bool,
        now: u64,
    ) -> Result<(), PollError> {
        self.clock(now)?;
        if kind != crate::l3::registration::RegistrationType::Forced {
            return self.advance(registry, |candidate| candidate == identity && busy, now);
        }
        let Some(subscriber) = registry
            .subscribers()
            .iter()
            .find(|subscriber| subscriber.provisioning.identity == identity)
        else {
            return self.advance(registry, |candidate| candidate == identity && busy, now);
        };
        let Some(registration) = registry.registration_handle(subscriber.provisioning.identity)
        else {
            return self.advance(registry, |candidate| candidate == identity && busy, now);
        };
        for poll in &mut self.polls {
            if poll.completion.identity == identity && poll.pending() && busy {
                poll.state = PollState::Cancelled;
                continue;
            }
            if poll.completion.identity != identity
                || poll
                    .provisioning
                    .as_ref()
                    .is_some_and(|handle| !registry.subscriber_current(handle))
                || !matches!(
                    poll.kind,
                    QueryKind::Audit {
                        force_registration: true,
                        ..
                    }
                )
            {
                continue;
            }
            if let PollState::AwaitingReport { deadline_ms } = poll.state
                && now < deadline_ms
            {
                poll.provisioning = registry.subscriber_handle(identity);
                poll.registration = Some(registration.clone());
                poll.forced_registration_received = true;
                tracing::info!(%identity, "Forced Registration received for Audit Order");
                if poll.confirmation.is_some() {
                    poll.state = PollState::Received;
                }
            }
        }
        self.advance(registry, |candidate| candidate == identity && busy, now)
    }
    pub fn shutdown(&mut self) {
        for poll in &mut self.polls {
            if poll.pending() {
                poll.state = PollState::Cancelled;
            }
        }
        self.selected = None;
        self.repeats.clear();
    }
    fn clock(&mut self, now: u64) -> Result<(), PollError> {
        if now < self.last_ms {
            return Err(PollError::ClockRegression);
        }
        self.last_ms = now;
        Ok(())
    }
}
#[cfg(test)]
mod tests;
