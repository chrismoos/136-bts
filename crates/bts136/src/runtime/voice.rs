#[cfg(test)]
use crate::l3::broadcast::ProtocolVersion;
mod alignment;
mod audio;
mod dtmf;
mod measurement_readings;
mod measurement_request;
mod measurements;
mod media;
mod overlap;
mod paging;
mod sip;
pub use paging::PageReceipt;
mod queue;
mod ringback;
mod sample;
mod sms;
mod status;
use crate::{
    codec::vselp::VselpParameters,
    core::{
        call_paging::{CallPaging, PagingOutcome, PagingRequest},
        call_session::{
            Assignment, BearerState, CallSession, EndReason, Party, ResponseTransmission,
            SessionPolicy,
        },
        calls::CallHandle,
        registry::LocalRegistry,
    },
    l3::designation::{AssignedSlots, Designation, TimeAlignment},
    link::{
        bearer_rx::BearerRx,
        bearer_tx::{BearerTx, PreparedBurst, QueueLimits, ScheduledCompletion},
        rach::MessageSet,
        spach::{Channel, HeaderFlags, SingleMessage},
        traffic_order::{OrderPolicy, OrderTransmission},
        traffic_tx::CommandHandle,
        voice::{FrameKind, TransmitFrame},
    },
    phy::{control::INFORMATION_BITS, speech::CrcStatus, sync::SyncWord, traffic::TrafficFields},
    resources::TrafficResources,
    scheduler::paging::PagingAllocation,
};
use std::{collections::VecDeque, error::Error};
// TIA/EIA-136-131-C Figure 11 places reverse slots 207 symbols ahead of forward slots.
const STANDARD_REVERSE_LEAD_SYMBOLS: f64 = 207.0;
const MAX_RX_TIMING_ERROR_SYMBOLS: f64 = 8.0;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimingMode {
    Automatic,
    Manual,
}

const FULL_RATE_PERIOD: u64 = 3;
const ORIGINATION: u8 = 7;
const PAGE_RESPONSE: u8 = 8;
const ESTABLISHMENT_MS: u64 = 5000;
const ORDER_RESPONSE_MS: u64 = 500;
const ORDER_TRANSMISSIONS: u16 = 3;
const CALL_HISTORY_CAPACITY: usize = 64;
const MAX_SMS_INBOX: usize = 32;
const MAX_POWER_DOWN_REPORTS: usize = 32;
const MAX_REJECTIONS: usize = 32;
const TRAFFIC_QUEUE_WORDS: usize =
    crate::l3::legacy_traffic::MAX_MESSAGE_BITS.div_ceil(crate::link::traffic::WORD_BITS) + 4;
fn queue_limits() -> QueueLimits {
    QueueLimits {
        facch_words: TRAFFIC_QUEUE_WORDS,
        sacch_words: TRAFFIC_QUEUE_WORDS,
    }
}
struct Page {
    request: PagingRequest,
    started_ms: u64,
    phase: u8,
    transmission: paging::Transmission,
}
impl Page {
    fn call_id(&self) -> u64 {
        self.request.handle.id().value()
    }
}
struct Repeat {
    superframe: u64,
    phase: u8,
    information: [bool; INFORMATION_BITS],
}
struct Designated {
    call_id: u64,
    party: Party,
    started: bool,
    frames: VecDeque<[bool; INFORMATION_BITS]>,
}
struct Rejection {
    identity: crate::link::rach::MobileIdentity,
    bytes: Vec<u8>,
    information: [bool; INFORMATION_BITS],
    deadline_ms: u64,
}
#[derive(Default)]
struct LegMeasurements {
    status: crate::l3::legacy_traffic::StatusFields,
    status_report_ms: Option<u64>,
    shortened_bursts: u64,
    channel_profile: Option<crate::phy::acquisition::channel_profile::ChannelProfile>,
    channel_profile_ms: Option<u64>,
    capabilities: Option<crate::l3::legacy_traffic::MobileCapabilities>,
    capability_report_ms: Option<u64>,
    physical_control: Option<crate::l3::legacy_traffic::PhysicalControlFields>,
    physical_control_report_ms: Option<u64>,
    downlink: Option<crate::l3::legacy_traffic::DownlinkQuality>,
    downlink_report_ms: Option<u64>,
    speech_frames: u64,
    erased_frames: u64,
    facch_frames: u64,
    signaling_errors: u64,
    uplink_bit_errors: crate::phy::bit_errors::BitErrorWindow,
}
struct Leg {
    registration: Option<crate::core::registry::RegistrationHandle>,
    power_down_reported: bool,
    designation: Vec<u8>,
    initial_alignment: alignment::InitialAlignment,
    timing: alignment::TimingTracker,
    last_shortened_interval: Option<u64>,
    controls_initialized: bool,
    measurements: LegMeasurements,
    assignment: Assignment,
    tx: BearerTx,
    rx: BearerRx,
    report_owners: measurements::ReportOwners<crate::core::call_session::MeasurementHandle>,
    next_tx: u64,
    next_rx: Option<u64>,
    in_flight: usize,
    last_rx_ms: u64,
    media: Option<VselpParameters>,
    media_pcm: Option<[f64; crate::codec::vselp::FRAME_SAMPLES]>,
    media_encoder: crate::codec::speech::Encoder,
    ringback_start: Option<u64>,
    transcoding: bool,
    source: media::Source,
    last_media: VselpParameters,
    order: Option<(CommandHandle, OrderTransmission)>,
    response: Option<(CommandHandle, ResponseTransmission)>,
    flash: Option<(CommandHandle, crate::core::call_session::FlashTransmission)>,
    service: Option<(
        CommandHandle,
        crate::core::call_session::ServiceTransmission,
    )>,
    sms_facch: Option<(CommandHandle, super::sms::TrafficOffer)>,
    sms_sacch: Option<(
        crate::link::sacch_tx::CommandHandle,
        super::sms::TrafficOffer,
    )>,
}
struct SmsHandoff {
    token: super::sms::TrafficToken,
    observed_ms: u64,
}
struct SmsCompletion {
    token: super::sms::TrafficToken,
    completed_ms: u64,
}
struct Active {
    sample_answer: sample::Answer,
    audio: bool,
    started_ms: u64,
    session: CallSession,
    legs: crate::core::call_session::PartySlots<Leg>,
    dtmf: [dtmf::Sender; 2],
    overlap: overlap::Tracker,
}
#[derive(Debug, Clone)]
pub enum Completion {
    None,
    Message {
        identity: crate::link::rach::MobileIdentity,
        bytes: Vec<u8>,
    },
    Page(PageReceipt),
    Assignment {
        party: Party,
        call_id: u64,
        last: bool,
    },
    Traffic {
        party: Party,
        call_id: u64,
        command: ScheduledCompletion,
    },
}
enum Selected {
    Page(usize, u64, u8),
    Repeat(usize),
    Assignment,
    Rejection,
}
pub struct VoiceRuntime {
    gateway: Option<crate::sip::Gateway>,
    sip_failure: Option<String>,
    sip_enabled: bool,
    sip_ringback: bool,
    ringback: crate::config::RingbackStyle,
    sip_calls: Vec<sip::Binding>,
    protocol: Option<std::sync::Arc<crate::operations::protocol::ProtocolLog>>,
    paging: CallPaging,
    sms_inbox: VecDeque<(
        super::sms::TrafficPeer,
        crate::l3::legacy_traffic::TrafficRelay,
    )>,
    sms_completed: VecDeque<SmsCompletion>,
    sms_handoffs: VecDeque<SmsHandoff>,
    power_down_reports: VecDeque<crate::core::registry::RegistrationHandle>,
    sms_interrupted: VecDeque<super::sms::TrafficToken>,
    history: VecDeque<crate::operations::proto::Call>,
    allocation: PagingAllocation,
    dvcc: u8,
    traffic_power_level: u8,
    return_channel: Option<crate::l3::legacy_traffic::ControlChannel>,
    pages: Vec<Page>,
    repeats: VecDeque<Repeat>,
    assignments: VecDeque<Designated>,
    calls: Vec<Active>,
    selected: Option<Selected>,
    answer_ms: u64,
    radio_loss_ms: u64,
    rejections: VecDeque<Rejection>,
}
pub type TrafficBurst = (u64, Party, PreparedBurst);
fn index(party: Party) -> usize {
    match party {
        Party::Caller => 0,
        Party::Callee => 1,
    }
}
fn phase(pair: AssignedSlots) -> u64 {
    match pair {
        AssignedSlots::Full14 => 0,
        AssignedSlots::Full25 => 1,
        AssignedSlots::Full36 => 2,
        _ => unreachable!("only full-rate pairs are allocated"),
    }
}
impl VoiceRuntime {
    pub fn try_new(
        resources: TrafficResources,
        allocation: PagingAllocation,
        dvcc: u8,
        traffic_power_level: u8,
        policy: &crate::config::CallConfig,
    ) -> Result<Self, Box<dyn Error>> {
        if dvcc == crate::phy::color::RESERVED_TRAFFIC_DVCC {
            return Err("traffic DVCC zero is reserved".into());
        }
        if traffic_power_level > crate::config::MAX_MOBILE_POWER_LEVEL {
            return Err("unsupported traffic power level".into());
        }
        let answer_ms = policy
            .answer_timeout_seconds
            .checked_mul(1000)
            .filter(|value| *value > 0)
            .ok_or("invalid answer timeout")?;
        let radio_loss_ms = policy
            .radio_loss_timeout_seconds
            .checked_mul(1000)
            .filter(|value| *value > 0)
            .ok_or("invalid radio loss timeout")?;
        Ok(Self {
            gateway: None,
            sip_failure: None,
            sip_enabled: false,
            sip_ringback: true,
            ringback: policy.ringback,
            sip_calls: Vec::new(),
            protocol: None,
            sms_inbox: VecDeque::new(),
            sms_completed: VecDeque::new(),
            sms_handoffs: VecDeque::new(),
            power_down_reports: VecDeque::new(),
            sms_interrupted: VecDeque::new(),
            paging: CallPaging::new(resources),
            history: VecDeque::new(),
            allocation,
            dvcc,
            traffic_power_level,
            return_channel: None,
            pages: Vec::new(),
            repeats: VecDeque::new(),
            assignments: VecDeque::new(),
            calls: Vec::new(),
            selected: None,
            answer_ms,
            radio_loss_ms,
            rejections: VecDeque::new(),
        })
    }
    pub(crate) fn has_power_down_reports(&self) -> bool {
        !self.power_down_reports.is_empty()
    }
    pub(crate) fn take_power_down_report(
        &mut self,
    ) -> Option<crate::core::registry::RegistrationHandle> {
        self.power_down_reports.pop_front()
    }
    fn dcch_locator(&self) -> u8 {
        self.return_channel.map_or(0, |channel| channel.locator())
    }
    pub fn set_return_channel(
        &mut self,
        channel: crate::l3::legacy_traffic::ControlChannel,
    ) -> Result<(), Box<dyn Error>> {
        channel.validate()?;
        if !self.calls.is_empty() || !self.pages.is_empty() {
            return Err("cannot change the return channel during a call".into());
        }
        self.return_channel = Some(channel);
        Ok(())
    }
    pub(crate) fn set_protocol_log(
        &mut self,
        log: std::sync::Arc<crate::operations::protocol::ProtocolLog>,
    ) {
        self.protocol = Some(log);
    }
    pub fn reject_access(
        &mut self,
        identity: crate::link::rach::MobileIdentity,
        cause: crate::l3::registration::RejectCause,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.enqueue_access_response(identity, crate::l3::call_control::reorder(cause), now_ms)
    }
    pub fn reject_page_response(
        &mut self,
        identity: crate::link::rach::MobileIdentity,
        cause: crate::l3::registration::RejectCause,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.enqueue_access_response(identity, crate::l3::call_control::release(cause), now_ms)
    }
    fn enqueue_access_response(
        &mut self,
        identity: crate::link::rach::MobileIdentity,
        bytes: Vec<u8>,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        if self.rejections.len() == MAX_REJECTIONS {
            return Err("call rejection queue full".into());
        }
        let frames = SingleMessage {
            channel: Channel::Arch,
            flags: HeaderFlags::default(),
            identity,
            message: bytes.clone(),
        }
        .encode_fragmented()?;
        let [information] = frames.as_slice() else {
            return Err("call response does not fit a control frame".into());
        };
        self.rejections.push_back(Rejection {
            identity,
            bytes,
            information: *information,
            deadline_ms: now_ms
                .checked_add(super::timers::SPACH_MS)
                .ok_or("call response deadline range")?,
        });
        Ok(())
    }
    pub fn receive_access(
        &mut self,
        registry: &LocalRegistry,
        set: &MessageSet,
        now_ms: u64,
        next_slot: u64,
    ) -> Result<(), Box<dyn Error>> {
        match set
            .messages
            .iter()
            .filter_map(|message| message.first())
            .copied()
            .find(|kind| matches!(*kind, ORIGINATION | PAGE_RESPONSE))
        {
            Some(ORIGINATION) => {
                let start = self.paging.begin_route(
                    registry,
                    set,
                    now_ms,
                    super::timers::ORIGINATION_PAGE_WAIT_MS,
                )?;
                let page = match start {
                    crate::core::call_paging::CallStart::Audio(reservation) => {
                        let handle = reservation.handle.clone();
                        let remote = reservation.remote_number.clone();
                        let caller = reservation.mobile.number.clone();
                        let external = self.sip_enabled && remote != sip::SAMPLE_NUMBER;
                        let mut session =
                            CallSession::try_new_audio(reservation, self.session_policy(), now_ms)?;
                        session.wait_for_remote_answer();
                        if let Err(error) =
                            self.activate(session, registry, now_ms, next_slot, true, None)
                        {
                            self.forget_assignments(handle.id().value());
                            self.paging.release_audio_stopped(&handle)?;
                            return Err(error);
                        }
                        if external {
                            self.dial_sip(handle.id().value(), &remote, &caller, now_ms)?;
                        } else {
                            if let Some(active) = self.calls.last_mut() {
                                active.sample_answer = sample::Answer::WaitingBearer;
                            }
                            tracing::info!(identity=%set.identity, "unknown destination routed to sample audio service");
                        }
                        return Ok(());
                    }
                    crate::core::call_paging::CallStart::Paging(page) => page,
                };
                self.rejections.retain(|rejection| {
                    rejection.identity != set.identity && rejection.identity != page.recipient
                });
                let phase = match self.allocation.phase(page.recipient) {
                    Ok(phase) => phase,
                    Err(error) => {
                        self.paging.cancel(&page.handle, now_ms)?;
                        return Err(error.into());
                    }
                };
                let number = self
                    .paging
                    .reservations()
                    .find(|reservation| reservation.handle.matches(&page.handle))
                    .ok_or("paging reservation missing")?
                    .caller
                    .number
                    .clone();
                let transmission = match paging::message(&number)
                    .map_err(|error| -> Box<dyn Error> { error.into() })
                    .and_then(|bytes| {
                        paging::Transmission::new(page.recipient, bytes).map_err(Into::into)
                    }) {
                    Ok(transmission) => transmission,
                    Err(error) => {
                        self.paging.cancel(&page.handle, now_ms)?;
                        return Err(error);
                    }
                };
                self.pages.push(Page {
                    request: page,
                    started_ms: now_ms,
                    phase,
                    transmission,
                });
                tracing::info!(identity=%set.identity,"mobile call enters paging");
            }
            Some(PAGE_RESPONSE) => {
                let page_index = self
                    .pages
                    .iter()
                    .position(|page| page.request.recipient == set.identity)
                    .ok_or("no matching call page")?;
                let handle = self.pages[page_index].request.handle.clone();
                let started_ms = self.pages[page_index].started_ms;
                let call_id = handle.id().value();
                let pending_history = self.call_history(call_id, now_ms);
                let outcome = match self.paging.response(&handle, set, now_ms) {
                    Err(
                        error @ (crate::core::call_paging::PagingError::Admission(_)
                        | crate::core::call_paging::PagingError::Service),
                    ) => {
                        let reason = match error {
                            crate::core::call_paging::PagingError::Admission(reason) => reason,
                            _ => crate::l3::access_call::AdmissionError::Service,
                        };
                        if self.paging.is_audio(&handle) {
                            self.paging.cancel_audio(&handle, now_ms)?;
                            self.pages.remove(page_index);
                            self.reject_page_response(set.identity, reason.reject_cause(), now_ms)?;
                            return Err(
                                crate::core::call_paging::PagingError::Admission(reason).into()
                            );
                        }
                        let reservation = self.paging.cancel(&handle, now_ms)?;
                        self.pages.remove(page_index);
                        for mut call in pending_history {
                            call.set_phase(crate::operations::proto::CallPhase::Ended);
                            call.end_reason = "Call mode rejected".into();
                            call.measurements.push(crate::operations::metric(
                                "Rejection cause",
                                format!("{:?}", reason.reject_cause()),
                                "",
                            ));
                            self.remember_call(call);
                        }
                        self.reject_access(
                            reservation.caller.identity,
                            reason.reject_cause(),
                            now_ms,
                        )?;
                        return Err(crate::core::call_paging::PagingError::Admission(reason).into());
                    }
                    result => result?,
                };
                self.pages.remove(page_index);
                if let PagingOutcome::Rejected(reservation) | PagingOutcome::Expired(reservation) =
                    &outcome
                {
                    for mut call in pending_history {
                        call.set_phase(crate::operations::proto::CallPhase::Ended);
                        call.end_reason = if matches!(outcome, PagingOutcome::Expired(_)) {
                            "Paging expired"
                        } else {
                            "Page rejected"
                        }
                        .into();
                        self.remember_call(call);
                    }
                    self.reject_access(
                        reservation.caller.identity,
                        crate::l3::registration::RejectCause::ServiceCodeNotAvailable,
                        now_ms,
                    )?;
                    self.reject_page_response(
                        reservation.callee.identity,
                        crate::l3::registration::RejectCause::ServiceCodeNotAvailable,
                        now_ms,
                    )?;
                    return Ok(());
                }
                if let PagingOutcome::AudioReady {
                    reservation,
                    response,
                } = &outcome
                {
                    let handle = reservation.handle.clone();
                    let session = CallSession::try_new_audio(
                        reservation.clone(),
                        self.session_policy(),
                        now_ms,
                    )?;
                    if let Err(error) = self.activate(
                        session,
                        registry,
                        now_ms,
                        next_slot,
                        true,
                        Some(response.protocol_version),
                    ) {
                        self.forget_assignments(handle.id().value());
                        self.paging.release_audio_stopped(&handle)?;
                        return Err(error);
                    }
                    self.keep_page_start(started_ms);
                    return Ok(());
                }
                if matches!(
                    outcome,
                    PagingOutcome::AudioRejected(_) | PagingOutcome::AudioExpired(_)
                ) {
                    for mut call in pending_history {
                        call.set_phase(crate::operations::proto::CallPhase::Ended);
                        call.end_reason = if matches!(outcome, PagingOutcome::AudioExpired(_)) {
                            "Paging expired"
                        } else {
                            "Page rejected"
                        }
                        .into();
                        self.remember_call(call);
                    }
                    return Ok(());
                }
                if let PagingOutcome::Ready { reservation, .. } = outcome {
                    let order = OrderPolicy {
                        response_timeout_ms: ORDER_RESPONSE_MS,
                        overall_timeout_ms: ESTABLISHMENT_MS,
                        max_transmissions: ORDER_TRANSMISSIONS,
                    };
                    let session = CallSession::try_new(
                        reservation,
                        SessionPolicy {
                            establishment_timeout_ms: ESTABLISHMENT_MS,
                            answer_timeout_ms: self.answer_ms,
                            release_timeout_ms: ESTABLISHMENT_MS,
                            alert: order,
                            release: order,
                            measurements: order,
                            status: order,
                            capabilities: order,
                            physical_control: order,
                        },
                        now_ms,
                    )?;
                    self.activate(session, registry, now_ms, next_slot, false, None)?;
                    self.keep_page_start(started_ms);
                }
            }
            _ => return Err("unsupported call access message".into()),
        }
        Ok(())
    }
    fn session_policy(&self) -> SessionPolicy {
        let order = OrderPolicy {
            response_timeout_ms: ORDER_RESPONSE_MS,
            overall_timeout_ms: ESTABLISHMENT_MS,
            max_transmissions: ORDER_TRANSMISSIONS,
        };
        SessionPolicy {
            establishment_timeout_ms: ESTABLISHMENT_MS,
            answer_timeout_ms: self.answer_ms,
            release_timeout_ms: ESTABLISHMENT_MS,
            alert: order,
            release: order,
            measurements: order,
            status: order,
            capabilities: order,
            physical_control: order,
        }
    }
    fn activate(
        &mut self,
        mut session: CallSession,
        registry: &LocalRegistry,
        now_ms: u64,
        next_slot: u64,
        sample_audio: bool,
        peer_version: Option<crate::l3::broadcast::ProtocolVersion>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(channel) = self.return_channel {
            session.set_return_channel(channel)?;
        }
        let call_id = session.handle().id().value();
        let mut legs = Vec::new();
        for party in session.parties().into_iter().flatten() {
            let assignment = session
                .assignment(party)
                .ok_or("missing initial assignment")?;
            let first = next_slot
                .saturating_sub(phase(assignment.subscriber.assigned_slots))
                .div_ceil(FULL_RATE_PERIOD);
            let designation = Designation {
                dvcc: self.dvcc,
                dmac: self.traffic_power_level,
                channel: assignment.subscriber.channel,
                assigned_slots: assignment.subscriber.assigned_slots,
                shortened_burst: true,
                protocol_version: super::capabilities::SERVING_PROTOCOL.common(
                    assignment
                        .subscriber
                        .protocol_version
                        .or_else(|| {
                            registry
                                .subscribers()
                                .iter()
                                .find(|subscriber| {
                                    subscriber.provisioning.identity
                                        == assignment.subscriber.identity
                                })
                                .and_then(|subscriber| subscriber.registration)
                                .map(|registration| registration.protocol_version)
                        })
                        .or(peer_version)
                        .ok_or("assigned phone has no negotiated protocol version")?,
                ),
                time_alignment: TimeAlignment::OffsetUnits(0),
                delay_interval_compensation: false,
            };
            let codec = assignment.subscriber.codec;
            tracing::info!(identity=%assignment.subscriber.identity, codec=codec.label(), "Traffic codec selected");
            let other = if party == Party::Caller {
                Party::Callee
            } else {
                Party::Caller
            };
            let other_codec = session
                .assignment(other)
                .map_or(codec, |assignment| assignment.subscriber.codec);
            let transcoding = sample_audio
                || codec == crate::codec::speech::Codec::AcelpCc1
                || codec != other_codec;
            let designation_bytes = designation.encode_with_voice(
                (codec == crate::codec::speech::Codec::AcelpCc1).then_some(
                    crate::l3::designation::VoiceMode {
                        coder: codec.coder(),
                        privacy: 0,
                    },
                ),
            )?;
            session.bind_protocol(&assignment.handle, designation.protocol_version)?;
            let frames = SingleMessage {
                channel: Channel::Arch,
                flags: HeaderFlags::default(),
                identity: assignment.subscriber.identity,
                message: designation_bytes.clone(),
            }
            .encode_fragmented()?
            .into();
            self.assignments.push_back(Designated {
                call_id,
                party,
                frames,
                started: false,
            });
            legs.push(Leg {
                registration: registry.registration_handle(assignment.subscriber.identity),
                power_down_reported: false,
                designation: designation_bytes,
                initial_alignment: alignment::InitialAlignment::Shortened,
                timing: alignment::TimingTracker::default(),
                last_shortened_interval: None,
                controls_initialized: false,
                measurements: LegMeasurements::default(),
                assignment,
                tx: BearerTx::try_new(self.dvcc, self.dcch_locator(), queue_limits(), first)?,
                report_owners: Default::default(),
                rx: BearerRx::with_codec(
                    self.dvcc,
                    codec,
                    crate::l3::relay::Direction::Reverse,
                    designation.protocol_version,
                ),
                next_tx: first,
                next_rx: None,
                in_flight: 0,
                last_rx_ms: now_ms,
                media: None,
                media_pcm: None,
                media_encoder: crate::codec::speech::Encoder::new(codec),
                ringback_start: None,
                transcoding,
                source: if sample_audio {
                    media::Source::Sample(None)
                } else {
                    media::Source::Peer
                },
                last_media: VselpParameters::default(),
                order: None,
                response: None,
                flash: None,
                service: None,
                sms_facch: None,
                sms_sacch: None,
            });
        }
        let legs = match legs.len() {
            1 => {
                let leg = legs.pop().expect("single physical leg exists");
                crate::core::call_session::PartySlots::single(leg.assignment.handle.party(), leg)
            }
            2 => {
                let callee = legs.pop().expect("callee exists");
                let caller = legs.pop().expect("caller exists");
                crate::core::call_session::PartySlots::pair(caller, callee)
            }
            _ => return Err("call has no physical bearer".into()),
        };
        self.calls.push(Active {
            sample_answer: sample::Answer::Disabled,
            audio: sample_audio,
            started_ms: now_ms,
            session,
            legs,
            dtmf: Default::default(),
            overlap: Default::default(),
        });
        tracing::info!(call_id, "call enters traffic assignment");
        Ok(())
    }
    fn keep_page_start(&mut self, started_ms: u64) {
        if let Some(active) = self.calls.last_mut() {
            active.started_ms = started_ms;
        }
    }
    fn forget_assignments(&mut self, call_id: u64) {
        self.assignments
            .retain(|pending| pending.call_id != call_id);
    }
    pub(crate) fn has_active_call(&self, call_id: u64) -> bool {
        self.call_index(call_id).is_some()
    }
    fn call_index(&self, call_id: u64) -> Option<usize> {
        self.calls
            .iter()
            .position(|active| active.session.handle().id().value() == call_id)
    }
    fn active_call(&mut self, call_id: u64) -> Result<&mut Active, Box<dyn Error>> {
        let index = self.call_index(call_id).ok_or("call is no longer active")?;
        Ok(&mut self.calls[index])
    }
    fn call_history(&self, call_id: u64, now_ms: u64) -> Vec<crate::operations::proto::Call> {
        self.operation_calls(now_ms)
            .into_iter()
            .filter(|call| call.id == call_id)
            .collect()
    }
    fn traffic_leg(&self, physical_slot: u64, sync: Option<SyncWord>) -> Option<(usize, Party)> {
        self.calls.iter().enumerate().find_map(|(call, active)| {
            active
                .legs
                .present()
                .into_iter()
                .flatten()
                .find(|party| {
                    let slot_phase = phase(
                        active.legs[index(*party)]
                            .assignment
                            .subscriber
                            .assigned_slots,
                    );
                    physical_slot % FULL_RATE_PERIOD == slot_phase
                        && sync.is_none_or(|sync| sync == SyncWord::ALL[slot_phase as usize])
                })
                .map(|party| (call, party))
        })
    }
    pub(crate) fn page_in_progress(&self) -> bool {
        self.pages
            .iter()
            .any(|page| page.transmission.in_progress())
    }
    pub(crate) fn control_in_progress(&self) -> bool {
        self.page_in_progress()
            || self
                .assignments
                .front()
                .is_some_and(|pending| pending.started)
    }
    pub(crate) fn repeat_count(&self, superframe: u64, phase: u8) -> usize {
        self.repeats
            .iter()
            .filter(|repeat| (repeat.superframe, repeat.phase) >= (superframe, phase))
            .count()
    }
    pub(crate) fn control_frame_count(&self) -> usize {
        match self.selected {
            Some(Selected::Page(page, superframe, phase)) => {
                self.pages.get(page).map_or(0, |page| {
                    page.transmission
                        .frame_count(self.allocation, superframe, phase)
                })
            }
            Some(Selected::Assignment) => self
                .assignments
                .front()
                .map_or(0, |pending| pending.frames.len()),
            Some(_) => 1,
            None => 0,
        }
    }
    pub(crate) fn control_adds_repeats(&self) -> usize {
        if let Some(Selected::Page(page, ..)) = self.selected {
            self.pages
                .get(page)
                .map_or(0, |page| page.transmission.remaining_frames())
        } else {
            0
        }
    }
    pub fn has_repeat(&self, superframe: u64, phase: u8) -> bool {
        self.repeats
            .iter()
            .any(|repeat| repeat.superframe == superframe && repeat.phase == phase)
    }
    pub fn control(&mut self, superframe: u64, phase: u8) -> Option<&[bool; INFORMATION_BITS]> {
        self.selected = None;
        self.repeats
            .retain(|repeat| (repeat.superframe, repeat.phase) >= (superframe, phase));
        if let Some(index) = self
            .repeats
            .iter()
            .position(|repeat| repeat.superframe == superframe && repeat.phase == phase)
        {
            self.selected = Some(Selected::Repeat(index));
            return Some(&self.repeats[index].information);
        }
        if let Some(index) = self
            .pages
            .iter()
            .position(|page| page.transmission.in_progress())
        {
            let page = &mut self.pages[index];
            self.selected = Some(Selected::Page(index, superframe, phase));
            return page.transmission.select(superframe, phase, page.phase);
        }
        if let Some(designated) = self.assignments.front().filter(|pending| pending.started) {
            self.selected = Some(Selected::Assignment);
            return designated.frames.front();
        }
        if let Some(rejection) = self.rejections.front() {
            self.selected = Some(Selected::Rejection);
            return Some(&rejection.information);
        }
        if let Some(designated) = self.assignments.front() {
            self.selected = Some(Selected::Assignment);
            return designated.frames.front();
        }
        if self.repeats.iter().any(|repeat| {
            repeat.superframe == superframe.saturating_add(1) && repeat.phase == phase
        }) {
            return None;
        }
        let index = self.pages.iter_mut().position(|page| {
            page.transmission
                .select(superframe, phase, page.phase)
                .is_some()
        })?;
        self.selected = Some(Selected::Page(index, superframe, phase));
        let page = &mut self.pages[index];
        page.transmission.select(superframe, phase, page.phase)
    }
    pub fn control_completion(&self) -> Completion {
        match &self.selected {
            Some(Selected::Page(index, ..)) => {
                let page = &self.pages[*index];
                Completion::Page(page.transmission.receipt(&page.request.handle))
            }
            Some(Selected::Repeat(_)) => Completion::None,
            Some(Selected::Rejection) => {
                let rejection = self.rejections.front().expect("rejection selected");
                Completion::Message {
                    identity: rejection.identity,
                    bytes: rejection.bytes.clone(),
                }
            }
            Some(Selected::Assignment) => {
                let pending = self.assignments.front().expect("designation selected");
                Completion::Assignment {
                    party: pending.party,
                    call_id: pending.call_id,
                    last: pending.frames.len() == 1,
                }
            }
            None => Completion::None,
        }
    }
    pub fn control_submitted(&mut self) -> Result<(), Box<dyn Error>> {
        match self.selected.take().expect("control slot selected") {
            Selected::Page(index, superframe, phase) => {
                let page = &mut self.pages[index];
                self.repeats.push_back(page.transmission.submitted(
                    self.allocation,
                    superframe,
                    phase,
                )?);
            }
            Selected::Rejection => {
                self.rejections.pop_front();
            }
            Selected::Repeat(index) => {
                self.repeats.remove(index);
            }
            Selected::Assignment => {
                let pending = self.assignments.front_mut().expect("designation selected");
                let call = self
                    .calls
                    .iter_mut()
                    .find(|active| active.session.handle().id().value() == pending.call_id)
                    .ok_or("designation without its call")?;
                call.legs[index(pending.party)].in_flight += 1;
                pending.started = true;
                pending.frames.pop_front();
                if pending.frames.is_empty() {
                    self.assignments.pop_front();
                }
            }
        }
        Ok(())
    }
    pub fn transmitted(
        &mut self,
        completion: Completion,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.transmitted_at(completion, now_ms, now_ms)
    }
    pub(crate) fn transmitted_at(
        &mut self,
        completion: Completion,
        completed_ms: u64,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        if completed_ms > now_ms {
            return Err("traffic completion time is after observation".into());
        }
        match completion {
            Completion::None => {}
            Completion::Message { identity, bytes } => {
                if let Some(log) = &self.protocol {
                    log.message(
                        "Forward",
                        "ARCH",
                        identity,
                        &bytes,
                        "TX completion observed",
                    );
                }
            }
            Completion::Page(receipt) => {
                let Some(page) = self
                    .pages
                    .iter_mut()
                    .find(|page| receipt.matches(&page.request.handle))
                else {
                    return Ok(());
                };
                if !page
                    .transmission
                    .transmitted(&receipt, &page.request.handle)
                {
                    return Ok(());
                }
                if let Some(log) = &self.protocol {
                    log.message(
                        "Forward",
                        "PCH",
                        page.request.recipient,
                        &page.transmission.bytes,
                        "TX completion observed",
                    );
                }
                if now_ms >= page.request.deadline_monotonic_ms {
                    self.advance(now_ms)?;
                } else {
                    self.paging.transmitted(&page.request.handle, now_ms)?;
                }
            }
            Completion::Assignment {
                party,
                call_id,
                last,
            } => {
                let call = self.call_index(call_id).ok_or("stale traffic assignment")?;
                let active = &mut self.calls[call];
                let leg = active
                    .legs
                    .get_mut(index(party))
                    .ok_or("call party has no radio bearer")?;
                leg.in_flight = leg
                    .in_flight
                    .checked_sub(1)
                    .ok_or("assignment without submission")?;
                if last {
                    if let Some(log) = &self.protocol {
                        log.message(
                            "Forward",
                            "ARCH",
                            leg.assignment.subscriber.identity,
                            &leg.designation,
                            "TX completion observed",
                        );
                    }
                    active
                        .session
                        .assignment_started(&leg.assignment.handle, now_ms)?;
                }
            }
            Completion::Traffic {
                party,
                call_id,
                command,
            } => {
                let call = self.call_index(call_id).ok_or("stale traffic completion")?;
                let active = &mut self.calls[call];
                let leg = active
                    .legs
                    .get_mut(index(party))
                    .ok_or("call party has no radio bearer")?;
                leg.in_flight = leg
                    .in_flight
                    .checked_sub(1)
                    .ok_or("traffic completion without submission")?;
                if let Some(handle) = command.facch {
                    if leg
                        .sms_facch
                        .as_ref()
                        .is_some_and(|(pending, _)| *pending == handle)
                    {
                        let (_, offer) = leg.sms_facch.take().expect("matched SMS transfer exists");
                        self.sms_completed.push_back(SmsCompletion {
                            token: offer.token,
                            completed_ms,
                        });
                        if let Some(log) = &self.protocol {
                            log.traffic(
                                "Forward",
                                "FACCH",
                                leg.assignment.subscriber.identity,
                                &offer.message,
                            );
                        }
                    } else if leg
                        .order
                        .as_ref()
                        .is_some_and(|(pending, _)| *pending == handle)
                    {
                        let (_, order) = leg.order.take().expect("matching order");
                        if let Some(log) = &self.protocol {
                            log.traffic(
                                "Forward",
                                "FACCH",
                                leg.assignment.subscriber.identity,
                                order.message(),
                            );
                        }
                        active
                            .session
                            .order_transmitted(&leg.assignment.handle, &order, now_ms)?;
                        if let Some(owner) = active.session.active_measurement_handle(party) {
                            leg.report_owners.transmitted(owner, command.interval);
                        }
                    } else if leg
                        .service
                        .as_ref()
                        .is_some_and(|(pending, _)| *pending == handle)
                    {
                        let (_, tx) = leg.service.take().expect("matching service response");
                        if let Some(log) = &self.protocol {
                            log.traffic(
                                "Forward",
                                "FACCH",
                                leg.assignment.subscriber.identity,
                                tx.message(),
                            );
                        }
                        match active.session.service_transmitted(party, &tx, now_ms) {
                            Ok(()) => {}
                            Err(crate::core::call_session::SessionError::Service(
                                crate::core::call_session::ServiceError::Closed
                                | crate::core::call_session::ServiceError::StaleTransmission,
                            )) => {}
                            Err(error) => return Err(error.into()),
                        }
                    } else if leg
                        .flash
                        .as_ref()
                        .is_some_and(|(pending, _)| *pending == handle)
                    {
                        let (_, tx) = leg.flash.take().expect("matching Flash transmission");
                        if let Some(log) = &self.protocol {
                            log.traffic(
                                "Forward",
                                "FACCH",
                                leg.assignment.subscriber.identity,
                                tx.message(),
                            );
                        }
                        match active.session.flash_transmitted(party, &tx, now_ms) {
                            Ok(()) => {}
                            Err(crate::core::call_session::SessionError::Flash(
                                crate::core::call_session::FlashError::Closed
                                | crate::core::call_session::FlashError::StaleTransmission,
                            )) => {}
                            Err(error) => return Err(error.into()),
                        }
                    } else if leg
                        .response
                        .as_ref()
                        .is_some_and(|(pending, _)| *pending == handle)
                    {
                        let (_, response) = leg.response.take().expect("matching response");
                        if let Some(log) = &self.protocol {
                            log.traffic(
                                "Forward",
                                "FACCH",
                                leg.assignment.subscriber.identity,
                                response.message(),
                            );
                        }
                        active.session.response_transmitted(&response, now_ms)?;
                    }
                }
                if let Some(handle) = command.sacch
                    && leg
                        .sms_sacch
                        .as_ref()
                        .is_some_and(|(pending, _)| *pending == handle)
                {
                    let (_, offer) = leg.sms_sacch.take().expect("matched SMS transfer exists");
                    self.sms_completed.push_back(SmsCompletion {
                        token: offer.token,
                        completed_ms,
                    });
                    if let Some(log) = &self.protocol {
                        log.traffic(
                            "Forward",
                            "SACCH",
                            leg.assignment.subscriber.identity,
                            &offer.message,
                        );
                    }
                }
            }
        }
        Ok(())
    }
    pub(crate) fn skipped_slot(&mut self, physical_slot: u64) -> Result<(), Box<dyn Error>> {
        let locator = self.dcch_locator();
        for active in &mut self.calls {
            for leg in active.legs.iter_mut() {
                let destination = index(leg.assignment.handle.party());
                let slot_phase = phase(leg.assignment.subscriber.assigned_slots);
                if physical_slot % FULL_RATE_PERIOD != slot_phase {
                    continue;
                }
                let next = physical_slot / FULL_RATE_PERIOD + 1;
                // Missing interleaved halves require retransmitting complete signaling words.
                leg.tx = BearerTx::try_new(self.dvcc, locator, queue_limits(), next)?;
                active.dtmf[1 - destination].skip(next - 1);
                leg.next_tx = next;
                leg.order = None;
                leg.response = None;
                leg.flash = None;
                leg.service = None;
                if let Some((_, offer)) = leg.sms_facch.take() {
                    self.sms_interrupted.push_back(offer.token);
                }
                if let Some((_, offer)) = leg.sms_sacch.take() {
                    self.sms_interrupted.push_back(offer.token);
                }
                leg.media = None;
                leg.media_pcm = None;
            }
        }
        Ok(())
    }
    pub fn traffic(&mut self, physical_slot: u64) -> Result<Option<TrafficBurst>, Box<dyn Error>> {
        let Some((call, party)) = self.traffic_leg(physical_slot, None) else {
            return Ok(None);
        };
        let active = &mut self.calls[call];
        if matches!(
            active.session.bearer_state(party),
            BearerState::Stopping | BearerState::Stopped
        ) {
            return Ok(None);
        }
        let call_id = active.session.handle().id().value();
        let leg = active
            .legs
            .get_mut(index(party))
            .ok_or("call party has no radio bearer")?;
        let slot_phase = phase(leg.assignment.subscriber.assigned_slots);
        let interval = (physical_slot - slot_phase) / FULL_RATE_PERIOD;
        media::offer(
            leg,
            &mut active.dtmf[1 - index(party)],
            interval,
            active.session.state(),
            self.ringback,
        )?;
        let burst = leg
            .tx
            .prepare(interval, SyncWord::ALL[slot_phase as usize])?
            .clone();
        Ok(Some((call_id, party, burst)))
    }
    pub(crate) fn subscriber_busy(&self, identity: crate::link::rach::MobileIdentity) -> bool {
        self.paging
            .reservations()
            .any(|call| call.caller.identity == identity || call.callee.identity == identity)
            || self
                .paging
                .audio_reservations()
                .any(|call| call.mobile.identity == identity)
    }
    pub(crate) fn operation_calls(&self, now_ms: u64) -> Vec<crate::operations::proto::Call> {
        use crate::operations::{
            metric,
            proto::{Call, CallLeg},
            unavailable,
        };
        self.paging
            .reservations()
            .map(|reservation| {
                let active = self
                    .calls
                    .iter()
                    .find(|active| active.session.handle().id() == reservation.handle.id());
                let page = self
                    .pages
                    .iter()
                    .find(|page| page.request.handle.id() == reservation.handle.id());
                let started_ms = active
                    .map(|active| active.started_ms)
                    .or(page.map(|page| page.started_ms))
                    .unwrap_or(now_ms);
                let legs = [
                    (&reservation.caller, Party::Caller),
                    (&reservation.callee, Party::Callee),
                ]
                .into_iter()
                .map(|(subscriber, party)| {
                    let mut measurements = vec![
                        metric("Codec", subscriber.codec.label(), ""),
                        metric(
                            "Configured mobile power level",
                            self.traffic_power_level,
                            "",
                        ),
                        unavailable(
                            "RSSI",
                            "Calibrated per-mobile RSSI is not exposed by the receiver",
                        ),
                    ];
                    if let Some(active) = active {
                        let leg = &active.legs[index(party)];
                        status::append_metrics(&mut measurements, &leg.measurements, now_ms);
                        measurement_readings::append_metrics(
                            &mut measurements,
                            active.session.extended_measurements(party),
                            now_ms,
                        );
                        super::capabilities::append_metrics(
                            &mut measurements,
                            leg.measurements.capabilities.as_ref(),
                            leg.measurements.capability_report_ms,
                            now_ms,
                        );
                        measurements.push(crate::operations::metric(
                            "Shortened bursts",
                            leg.measurements.shortened_bursts,
                            "",
                        ));
                        let initial_alignment = match leg.initial_alignment {
                            alignment::InitialAlignment::Shortened => "Acquiring",
                            alignment::InitialAlignment::Commanded => "Waiting for full bursts",
                            alignment::InitialAlignment::Complete => "Complete",
                        };
                        measurements.push(crate::operations::metric(
                            "Initial alignment",
                            initial_alignment,
                            "",
                        ));
                        measurements.push(crate::operations::metric(
                            "Timing control",
                            if leg.timing.manual {
                                "Manual"
                            } else {
                                "Automatic"
                            },
                            "",
                        ));
                        if let Some(residual) = leg.timing.residual_symbols {
                            measurements.push(crate::operations::metric(
                                "Reverse arrival error",
                                format!("{residual:.3}"),
                                "symbols",
                            ));
                        }
                        measurements.push(metric(
                            "Timing target arrival",
                            format!("{:.2}", leg.timing.target_symbols),
                            "symbols",
                        ));
                        if let Some(profile) = leg.measurements.channel_profile {
                            measurements.push(metric(
                                "Significant precursor",
                                profile.earliest_symbol,
                                "symbols",
                            ));
                            measurements.push(metric(
                                "Significant postcursor",
                                profile.latest_symbol,
                                "symbols",
                            ));
                            measurements.push(metric(
                                "Channel model residual",
                                format!("{:.3}", profile.residual_ratio),
                                "",
                            ));
                            if let Some(at) = leg.measurements.channel_profile_ms {
                                measurements.push(metric(
                                    "Channel profile age",
                                    now_ms.saturating_sub(at),
                                    "ms",
                                ));
                            }
                        }
                        if let Some(overlap) = active.overlap.overlap_symbols {
                            measurements.push(metric(
                                "Adjacent-slot overlap",
                                format!("{overlap:.2}"),
                                "symbols",
                            ));
                        }
                        measurements.push(metric(
                            "Overlap detections",
                            active.overlap.detections,
                            "",
                        ));
                        if leg.timing.saturated {
                            measurements.push(crate::operations::metric(
                                "Timing range",
                                "Limit reached",
                                "",
                            ));
                        }
                        if let Some(request) = active.session.physical_control_request(party) {
                            if let Some(level) = request.power_level {
                                measurements.push(crate::operations::metric(
                                    "Requested mobile power level",
                                    level,
                                    "",
                                ));
                            }
                            if let Some(alignment) = request.time_alignment {
                                let (value, unit) = match alignment {
                                    TimeAlignment::OffsetUnits(units) => {
                                        (units.to_string(), "half-symbols")
                                    }
                                    TimeAlignment::Maintain => {
                                        ("Maintain current timing".into(), "")
                                    }
                                };
                                measurements.push(crate::operations::metric(
                                    "Requested timing advance",
                                    value,
                                    unit,
                                ));
                            }
                        }
                        if let Some(state) = active.session.physical_control_state(party) {
                            measurements.push(crate::operations::metric(
                                "Physical control",
                                format!("{state:?}"),
                                "",
                            ));
                        }
                        if let Some(fields) = &leg.measurements.physical_control {
                            if let Some(level) = fields.control.power_level {
                                measurements.push(crate::operations::metric(
                                    "Confirmed mobile power level",
                                    level,
                                    "",
                                ));
                            }
                            if let Some(TimeAlignment::OffsetUnits(units)) =
                                fields.control.time_alignment
                            {
                                measurements.push(crate::operations::metric(
                                    "Confirmed timing advance",
                                    units,
                                    "half-symbols",
                                ));
                            }
                            if let Some(at) = leg.measurements.physical_control_report_ms {
                                measurements.push(crate::operations::metric(
                                    "Last physical control report age",
                                    now_ms.saturating_sub(at),
                                    "ms",
                                ));
                            }
                        }
                        if let Some(reason) = active.session.release_reason(party) {
                            measurements.push(metric(
                                "Mobile release reason",
                                format!("{reason:?}"),
                                "",
                            ));
                        }
                        if let Some(state) = active.session.service_state(party) {
                            measurements.push(metric("Service response", format!("{state:?}"), ""));
                            measurements.push(metric(
                                "Handset service requests",
                                active.session.service_count(party),
                                "",
                            ));
                        }
                        if let Some(request) = active.session.service_request(party) {
                            measurements.push(metric(
                                "Last service request",
                                format!("{request:?}"),
                                "",
                            ));
                        }
                        if let Some(response) = active.session.service_response(party) {
                            measurements.push(metric(
                                "Last service response",
                                format!("{response:?}"),
                                "",
                            ));
                        }
                        if let Some(state) = active.session.flash_state(party) {
                            measurements.push(metric("Call information", format!("{state:?}"), ""));
                        }
                        if let Some(number) = active.session.flash_number(party) {
                            measurements.push(metric(
                                "Call information request number",
                                number,
                                "",
                            ));
                        }
                        if let Some(fields) = active.session.flash_received(party) {
                            measurements.push(metric(
                                "Handset feature requests",
                                active.session.flash_received_count(party),
                                "",
                            ));
                            measurements.push(metric(
                                "Last handset feature request",
                                format!("{fields:?}"),
                                "",
                            ));
                            measurements.push(metric(
                                "Supplementary service execution",
                                "Not configured",
                                "",
                            ));
                        }
                        if let Some(state) = active.session.capability_state(party) {
                            measurements.push(crate::operations::metric(
                                "Capability query",
                                format!("{state:?}"),
                                "",
                            ));
                        }
                        if let Some(state) = active.session.status_state(party) {
                            measurements.push(metric("Status query", format!("{state:?}"), ""));
                        }
                        if let Some(quality) = leg.measurements.downlink {
                            if let Some(at_ms) = leg.measurements.downlink_report_ms {
                                measurements.push(metric(
                                    "Phone measurement age",
                                    now_ms.saturating_sub(at_ms),
                                    "ms",
                                ));
                            }
                            measurements.push(metric(
                                "Phone reported downlink RSSI",
                                quality.rssi_description(),
                                "dBm",
                            ));
                            measurements.push(metric(
                                "Phone reported downlink BER",
                                quality.ber_range(),
                                "%",
                            ));
                        }
                        measurements.push(metric(
                            "Measurement mode",
                            active
                                .session
                                .measurement_kind(party)
                                .map_or_else(|| "None".into(), |kind| format!("{kind:?}")),
                            "",
                        ));
                        measurements.push(metric(
                            "Measurement request",
                            active.session.measurement_state(party).map_or_else(
                                || "Not requested".into(),
                                |state| format!("{state:?}"),
                            ),
                            "",
                        ));
                        measurements.extend([
                            metric("Speech frames", leg.measurements.speech_frames, ""),
                            metric("Erased speech frames", leg.measurements.erased_frames, ""),
                            metric("FACCH frames", leg.measurements.facch_frames, ""),
                            metric(
                                "Signaling decode errors",
                                leg.measurements.signaling_errors,
                                "",
                            ),
                        ]);
                        if leg.measurements.speech_frames != 0 {
                            measurements.push(metric(
                                "Frame erasure rate",
                                format!(
                                    "{:.2}",
                                    leg.measurements.erased_frames as f64 * 100.0
                                        / leg.measurements.speech_frames as f64
                                ),
                                "%",
                            ));
                        }
                        let bit_errors = leg.measurements.uplink_bit_errors.total();
                        measurements.push(match bit_errors.percent() {
                            Some(percent) => metric("Uplink BER", format!("{percent:.2}"), "%"),
                            None => unavailable(
                                "Uplink BER",
                                "No CRC-valid uplink frame has been decoded",
                            ),
                        });
                        measurements.push(metric(
                            "Uplink coded bits checked",
                            bit_errors.checked,
                            "",
                        ));
                        measurements.push(metric(
                            "Last reverse burst age",
                            now_ms.saturating_sub(leg.last_rx_ms),
                            "ms",
                        ));
                        measurements.push(metric("In-flight TX slots", leg.in_flight, ""));
                    }
                    CallLeg {
                        party: format!("{party:?}"),
                        identity: format!("{:?}", subscriber.identity),
                        number: subscriber.number.clone(),
                        channel: u32::from(subscriber.channel),
                        slots: format!("{:?}", subscriber.assigned_slots),
                        phase: active.map_or(
                            crate::operations::proto::BearerPhase::Paging as i32,
                            |active| {
                                crate::operations::bearer_phase(active.session.bearer_state(party))
                                    as i32
                            },
                        ),
                        source: String::new(),
                        measurements,
                    }
                })
                .collect();
                Call {
                    id: reservation.handle.id().value(),
                    phase: active.map_or(
                        crate::operations::proto::CallPhase::Paging as i32,
                        |active| crate::operations::call_phase(active.session.state()).0 as i32,
                    ),
                    end_reason: active.map_or_else(String::new, |active| {
                        crate::operations::call_phase(active.session.state()).1
                    }),
                    legs,
                    measurements: {
                        let mut readings =
                            vec![metric("Duration", now_ms.saturating_sub(started_ms), "ms")];
                        if let Some(active) = active {
                            readings.extend(active.session.operation_deadlines());
                        }
                        if let Some(page) = page {
                            readings.push(metric(
                                "Paging deadline",
                                page.request.deadline_monotonic_ms,
                                "ms since startup",
                            ));
                        }
                        readings
                    },
                }
            })
            .chain(self.audio_operation_calls(now_ms))
            .collect()
    }
    pub(crate) fn recent_calls(&self) -> Vec<crate::operations::proto::Call> {
        self.history.iter().rev().cloned().collect()
    }
    fn remember_call(&mut self, call: crate::operations::proto::Call) {
        if self.history.len() == CALL_HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(call);
    }
    pub(crate) fn release_call(&mut self, call_id: u64, now_ms: u64) -> Result<(), Box<dyn Error>> {
        if let Some(index) = self.call_index(call_id) {
            self.calls[index]
                .session
                .end(EndReason::Cancelled, now_ms)?;
            tracing::info!(call_id, "operator requested call release");
            return Ok(());
        }
        if let Some(page_index) = self.pages.iter().position(|page| page.call_id() == call_id) {
            let handle = self.pages[page_index].request.handle.clone();
            let mut calls = self.call_history(call_id, now_ms);
            const CLEARING_RESPONSES: usize = 2;
            if self.rejections.len() > MAX_REJECTIONS - CLEARING_RESPONSES {
                return Err("call response queue is full".into());
            }
            now_ms
                .checked_add(super::timers::SPACH_MS)
                .ok_or("call response deadline range")?;
            if self.paging.is_audio(&handle) {
                self.paging.cancel_audio(&handle, now_ms)?;
                self.pages.remove(page_index);
                for mut call in calls {
                    call.set_phase(crate::operations::proto::CallPhase::Ended);
                    call.end_reason = "Cancelled while paging".into();
                    self.remember_call(call);
                }
                return Ok(());
            }
            let reservation = self.paging.cancel(&handle, now_ms)?;
            self.pages.remove(page_index);
            let cause = crate::l3::registration::RejectCause::ServiceCodeNotAvailable;
            self.reject_access(reservation.caller.identity, cause, now_ms)?;
            self.reject_page_response(reservation.callee.identity, cause, now_ms)?;
            for call in &mut calls {
                call.set_phase(crate::operations::proto::CallPhase::Ended);
                call.end_reason = "Cancelled while paging".into();
            }
            for call in calls {
                self.remember_call(call);
            }
            tracing::info!(call_id, "operator canceled call paging");
            return Ok(());
        }
        Err("call is no longer active".into())
    }
    pub(crate) fn send_call_information(
        &mut self,
        call_id: u64,
        party: Party,
        fields: crate::l3::legacy_traffic::CallInformation,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        let active = self.active_call(call_id)?;
        if active.legs.get(index(party)).is_none() {
            return Err("call party has no radio bearer".into());
        }
        active
            .session
            .request_call_information(party, fields, now_ms)?;
        Ok(())
    }
    pub(crate) fn request_status(
        &mut self,
        call_id: u64,
        party: Party,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        let active = self.active_call(call_id)?;
        if active.legs.get(index(party)).is_none() {
            return Err("call party has no radio bearer".into());
        }
        active.session.request_status(party, now_ms)?;
        tracing::info!(call_id, ?party, "phone status requested");
        Ok(())
    }
    pub(crate) fn set_measurements(
        &mut self,
        call_id: u64,
        party: Party,
        enabled: bool,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        let active = self.active_call(call_id)?;
        if active.legs.get(index(party)).is_none() {
            return Err("call party has no radio bearer".into());
        }
        active.session.set_measurements(party, enabled, now_ms)?;
        tracing::info!(call_id, ?party, enabled, "measurement reporting requested");
        Ok(())
    }
    pub(crate) fn request_extended_measurements(
        &mut self,
        call_id: u64,
        party: Party,
        targets: crate::l3::legacy_traffic::ExtendedMeasurementTargets,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        let active = self.active_call(call_id)?;
        if active.legs.get(index(party)).is_none() {
            return Err("call party has no radio bearer".into());
        }
        let version = active
            .session
            .protocol_version(party)
            .ok_or("bearer protocol is not assigned")?;
        let order = measurement_request::order(
            targets,
            version,
            active.session.confirmed_physical_control(party),
        )?;
        active.session.request_extended_measurements(
            &active.legs[index(party)].assignment.handle,
            order,
            now_ms,
        )?;
        tracing::info!(call_id, ?party, "extended measurements requested");
        Ok(())
    }
    pub(crate) fn set_physical_control(
        &mut self,
        call_id: u64,
        party: Party,
        request: crate::l3::legacy_traffic::PhysicalControlRequest,
        timing_mode: Option<TimingMode>,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        let active = self.active_call(call_id)?;
        if active.legs.get(index(party)).is_none() {
            return Err("call party has no radio bearer".into());
        }
        active
            .session
            .request_physical_control(party, request, now_ms)?;
        active.legs[index(party)].controls_initialized = true;
        if let Some(mode) = timing_mode {
            active.legs[index(party)]
                .timing
                .set_manual(mode == TimingMode::Manual);
        } else if request.time_alignment.is_some() {
            active.legs[index(party)].timing.set_manual(true);
        }
        tracing::info!(
            call_id,
            ?party,
            ?request,
            "phone physical controls requested"
        );
        Ok(())
    }
    pub(crate) fn traffic_submitted_at(
        &mut self,
        call_id: u64,
        party: Party,
        burst: &PreparedBurst,
        now_ms: u64,
    ) -> Result<ScheduledCompletion, Box<dyn Error>> {
        let completion = self.traffic_submitted(call_id, party, burst)?;
        let leg = &self.active_call(call_id)?.legs[index(party)];
        for token in [
            leg.sms_facch
                .as_ref()
                .filter(|(handle, _)| completion.facch.as_ref() == Some(handle))
                .map(|(_, offer)| offer.token),
            leg.sms_sacch
                .as_ref()
                .filter(|(handle, _)| completion.sacch.as_ref() == Some(handle))
                .map(|(_, offer)| offer.token),
        ]
        .into_iter()
        .flatten()
        {
            self.sms_handoffs.push_back(SmsHandoff {
                token,
                observed_ms: now_ms,
            });
        }
        Ok(completion)
    }
    pub(crate) fn retains_sms(&self, token: super::sms::TrafficToken) -> bool {
        self.calls.iter().any(|active| {
            active.legs.iter().any(|leg| {
                leg.sms_facch
                    .as_ref()
                    .is_some_and(|(_, offer)| offer.token == token)
                    || leg
                        .sms_sacch
                        .as_ref()
                        .is_some_and(|(_, offer)| offer.token == token)
            })
        })
    }
    pub fn traffic_submitted(
        &mut self,
        call_id: u64,
        party: Party,
        burst: &PreparedBurst,
    ) -> Result<ScheduledCompletion, Box<dyn Error>> {
        let leg = self
            .active_call(call_id)?
            .legs
            .get_mut(index(party))
            .ok_or("call party has no radio bearer")?;
        let completion = leg.tx.submitted(burst)?;
        leg.next_tx = burst.interval() + 1;
        leg.in_flight += 1;
        Ok(completion)
    }
    pub(crate) fn needs_traffic_reception(&self) -> bool {
        !self.calls.is_empty()
    }
    pub(crate) fn needs_initial_alignment(&self) -> bool {
        self.calls.iter().any(|active| {
            [Party::Caller, Party::Callee].into_iter().any(|party| {
                active.session.bearer_state(party) == BearerState::Assigning
                    && active.legs[index(party)].initial_alignment
                        != alignment::InitialAlignment::Complete
            })
        })
    }
    pub(crate) fn receive_shortened(
        &mut self,
        relative_symbols: f64,
        fields: &crate::phy::shortened::ShortenedBurst,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        if fields.color.dvcc != self.dvcc || !relative_symbols.is_finite() {
            return Ok(());
        }
        let slot_symbols = (crate::phy::traffic::SLOT_BITS / 2) as f64;
        let position = (relative_symbols + STANDARD_REVERSE_LEAD_SYMBOLS) / slot_symbols;
        let physical = position.round();
        if physical < 0.0
            || ((position - physical) * slot_symbols).abs()
                > MAX_RX_TIMING_ERROR_SYMBOLS
                    + f64::from(crate::l3::legacy_traffic::MAX_ALIGNMENT_UNITS) / 2.0
        {
            return Ok(());
        }
        let physical = physical as u64;
        if let Some((call, party)) = self.traffic_leg(physical, Some(fields.sync)) {
            let active = &mut self.calls[call];
            let leg = active
                .legs
                .get_mut(index(party))
                .ok_or("call party has no radio bearer")?;
            let slot_phase = phase(leg.assignment.subscriber.assigned_slots);
            if active.session.bearer_state(party) != BearerState::Assigning
                || leg.initial_alignment == alignment::InitialAlignment::Complete
            {
                return Ok(());
            }
            let interval = (physical - slot_phase) / FULL_RATE_PERIOD;
            if leg
                .last_shortened_interval
                .is_some_and(|previous| interval <= previous)
            {
                return Ok(());
            }
            leg.last_shortened_interval = Some(interval);
            leg.measurements.shortened_bursts += 1;
            leg.last_rx_ms = now_ms;
            if leg.initial_alignment == alignment::InitialAlignment::Shortened {
                let residual = (position - physical as f64) * slot_symbols;
                leg.timing.residual_symbols = Some(residual);
                let units = alignment::TimingTracker::initial_units(residual);
                active
                    .session
                    .align_initial_bearer(&leg.assignment.handle, units, now_ms)?;
                leg.initial_alignment = alignment::InitialAlignment::Commanded;
                tracing::info!(
                    ?party,
                    advance_half_symbols = units,
                    arrival_error_symbols = residual,
                    "shortened burst acquired"
                );
            }
        }
        Ok(())
    }
    pub(crate) fn reverse_identity(
        &self,
        relative_symbols: f64,
        sync: SyncWord,
        dvcc: u8,
    ) -> Option<crate::link::rach::MobileIdentity> {
        if dvcc != self.dvcc {
            return None;
        }
        let physical = reverse_position(relative_symbols)?.physical_slot;
        let (call, party) = self.traffic_leg(physical, Some(sync))?;
        Some(
            self.calls[call]
                .legs
                .get(index(party))?
                .assignment
                .subscriber
                .identity,
        )
    }
    pub fn receive_traffic(
        &mut self,
        relative_symbols: f64,
        segment: u64,
        fields: &TrafficFields,
        channel_profile: Option<crate::phy::acquisition::channel_profile::ChannelProfile>,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        if fields.color.dvcc != self.dvcc {
            return Ok(());
        }
        let Some(position) = reverse_position(relative_symbols) else {
            return Ok(());
        };
        let physical = position.physical_slot;
        let Some((call, party)) = self.traffic_leg(physical, Some(fields.sync)) else {
            return Ok(());
        };
        {
            let active = &mut self.calls[call];
            let leg = active
                .legs
                .get_mut(index(party))
                .ok_or("call party has no radio bearer")?;
            let slot_phase = phase(leg.assignment.subscriber.assigned_slots);
            if matches!(
                active.session.bearer_state(party),
                BearerState::Reserved | BearerState::Stopping | BearerState::Stopped
            ) {
                return Ok(());
            }
            let interval = (physical - slot_phase) / FULL_RATE_PERIOD;
            if leg.next_rx.is_some_and(|next| interval < next) {
                return Ok(());
            }
            if leg.initial_alignment == alignment::InitialAlignment::Shortened
                || leg
                    .last_shortened_interval
                    .is_some_and(|shortened| interval <= shortened)
                || (leg.initial_alignment == alignment::InitialAlignment::Commanded
                    && !active.session.physical_control_transmitted(party))
            {
                return Ok(());
            }
            let residual_symbols = position.residual_symbols;
            if leg.initial_alignment == alignment::InitialAlignment::Commanded
                && residual_symbols.abs() > alignment::ALIGNMENT_TOLERANCE_SYMBOLS
            {
                leg.timing.residual_symbols = Some(residual_symbols);
                return Ok(());
            }
            leg.initial_alignment = alignment::InitialAlignment::Complete;
            let mut timing_failure = false;
            if active.session.state() == crate::core::call_session::CallState::Connected
                && active.session.physical_control_state(party)
                    == Some(crate::link::traffic_order::OrderState::Acknowledged)
                && let Some(TimeAlignment::OffsetUnits(units)) = leg
                    .measurements
                    .physical_control
                    .as_ref()
                    .and_then(|fields| fields.control.time_alignment)
                && let Some(action) = leg.timing.observe(residual_symbols, units, segment)
            {
                match action {
                    alignment::TimingAction::Adjust(target) => {
                        active.session.request_physical_control(
                            party,
                            crate::l3::legacy_traffic::PhysicalControlRequest {
                                time_alignment: Some(TimeAlignment::OffsetUnits(target)),
                                ..Default::default()
                            },
                            now_ms,
                        )?;
                        tracing::info!(call_id=active.session.handle().id().value(), ?party, current_units=units, target_units=target, residual_symbols=?leg.timing.residual_symbols, "adjusting phone timing");
                    }
                    alignment::TimingAction::OutOfRange => {
                        timing_failure = true;
                    }
                }
            }
            leg.report_owners.push(
                interval,
                segment,
                active.session.active_measurement_handle(party),
            );
            let result = leg.rx.push(interval, segment, Some(fields))?;
            if let Some(frame) = &result.frame {
                match frame.kind {
                    FrameKind::Speech { crc, .. } => {
                        leg.measurements.speech_frames += 1;
                        if crc != CrcStatus::Valid {
                            leg.measurements.erased_frames += 1;
                        }
                    }
                    FrameKind::Facch(_) => leg.measurements.facch_frames += 1,
                    FrameKind::Missing
                    | FrameKind::UnavailableSpeech(_)
                    | FrameKind::UnavailableAcelp(_) => {
                        leg.measurements.speech_frames += 1;
                        leg.measurements.erased_frames += 1;
                    }
                }
            }
            let sacch_bit_errors =
                result
                    .sacch_word
                    .as_ref()
                    .map(|word| crate::phy::bit_errors::BitErrors {
                        corrected: u32::from(word.corrected_coded_bits),
                        checked: crate::phy::sacch::CODED_WORD_BITS as u32,
                    });
            for block in result
                .frame
                .as_ref()
                .and_then(|frame| frame.kind.bit_errors())
                .into_iter()
                .chain(sacch_bit_errors)
            {
                leg.measurements.uplink_bit_errors.push(block);
                if let Some(log) = &self.protocol {
                    log.bit_errors(leg.assignment.subscriber.identity, block);
                }
            }
            if result.facch_message.is_err() || result.sacch_message.is_err() {
                leg.measurements.signaling_errors += 1;
            }
            leg.next_rx = Some(interval + 1);
            leg.last_rx_ms = now_ms;
            if active.session.bearer_state(party) == BearerState::Assigning {
                // IS-54-B §3.6.5.2 confirms channel acquisition through received DVCC status, not assignment TX alone.
                active
                    .session
                    .bearer_confirmed(&leg.assignment.handle, now_ms)?;
                active.sample_answer.advance(&mut active.session, now_ms)?;
                tracing::info!(?party, "traffic channel confirmed");
            }
            for (channel, message) in [
                ("FACCH", result.facch_message),
                ("SACCH", result.sacch_message),
            ] {
                match message {
                    Ok(Some(message)) => {
                        if let Some(log) = &self.protocol {
                            log.traffic(
                                "Reverse",
                                channel,
                                leg.assignment.subscriber.identity,
                                &message,
                            );
                        }
                        if let crate::l3::legacy_traffic::TrafficMessage::ExtendedChannelQuality(
                            report,
                        ) = &message
                        {
                            let owner = if channel == "FACCH" {
                                leg.report_owners.facch()
                            } else {
                                leg.report_owners.sacch()
                            };
                            let Some(owner) = owner else {
                                tracing::debug!(
                                    ?party,
                                    channel,
                                    "measurement report crosses an inactive or replaced request"
                                );
                                continue;
                            };
                            match active
                                .session
                                .record_extended_measurement(owner, report, now_ms)
                            {
                                Ok(()) => {}
                                Err(
                                    crate::core::call_session::SessionError::State
                                    | crate::core::call_session::SessionError::Control(_),
                                ) => {
                                    tracing::debug!(
                                        ?party,
                                        channel,
                                        "measurement report rejected outside its active request"
                                    );
                                }
                                Err(error) => return Err(error.into()),
                            }
                            continue;
                        }
                        if matches!(
                            message,
                            crate::l3::legacy_traffic::TrafficMessage::AmrControl(_)
                                | crate::l3::legacy_traffic::TrafficMessage::AmrNoise(_)
                                | crate::l3::legacy_traffic::TrafficMessage::ComfortNoise(_)
                                | crate::l3::legacy_traffic::TrafficMessage::Flash(_)
                                | crate::l3::legacy_traffic::TrafficMessage::Service(_)
                                | crate::l3::legacy_traffic::TrafficMessage::ExtendedMeasurementAck(
                                    _
                                )
                        ) && channel != "FACCH"
                        {
                            tracing::warn!(?party, "FACCH-only message received on SACCH");
                            continue;
                        }
                        if let crate::l3::legacy_traffic::TrafficMessage::Dtmf(request) = &message {
                            if channel != "FACCH" {
                                tracing::warn!(?party, "DTMF request received outside FACCH");
                                continue;
                            }
                            let is_new = match active.dtmf[index(party)].is_new(request) {
                                Ok(is_new) => is_new,
                                Err(error) => {
                                    tracing::warn!(?party, ?error, "DTMF request not admitted");
                                    continue;
                                }
                            };
                            match active
                                .session
                                .receive(&leg.assignment.handle, &message, now_ms)
                            {
                                Ok(_) => {
                                    if is_new {
                                        active.dtmf[index(party)].accept(request);
                                        if let media::Source::Sip { audio, .. } = &leg.source
                                            && let Some(digit) = sip::digit(request)
                                        {
                                            audio.digits.force_push(digit);
                                        }
                                    }
                                }
                                Err(
                                    crate::core::call_session::SessionError::State
                                    | crate::core::call_session::SessionError::Message
                                    | crate::core::call_session::SessionError::ResponseQueueFull,
                                ) => {
                                    tracing::debug!(
                                        ?party,
                                        "DTMF request deferred for current call state or response capacity"
                                    );
                                }
                                Err(error) => return Err(error.into()),
                            }
                            continue;
                        }
                        if let crate::l3::legacy_traffic::TrafficMessage::Relay(relay) = message {
                            if self.sms_inbox.len() >= MAX_SMS_INBOX {
                                return Err("traffic SMS inbox is full".into());
                            }
                            self.sms_inbox.push_back((
                                super::sms::TrafficPeer {
                                    identity: leg.assignment.subscriber.identity,
                                    call_id: active.session.handle().id().value(),
                                    ready: false,
                                },
                                relay,
                            ));
                            continue;
                        }
                        if matches!(message, crate::l3::legacy_traffic::TrafficMessage::CapabilityUpdateRequest(_) | crate::l3::legacy_traffic::TrafficMessage::CapabilityUpdateResponse(_)) && channel != "FACCH" {
                            tracing::warn!(?party, "capability update received on an unsupported channel");
                            continue;
                        }
                        if let crate::l3::legacy_traffic::TrafficMessage::CapabilityUpdateRequest(
                            version,
                        ) = &message
                        {
                            match active.session.respond_capabilities(
                                &leg.assignment.handle,
                                super::capabilities::station_response(),
                                now_ms,
                            ) {
                                Ok(()) => {}
                                Err(crate::core::call_session::SessionError::State) => {
                                    tracing::debug!(
                                        ?party,
                                        "capability request discarded outside an active call task"
                                    );
                                    continue;
                                }
                                Err(error) => return Err(error.into()),
                            }
                            tracing::info!(?party, ?version, "phone requested cell capabilities");
                            continue;
                        }
                        if let crate::l3::legacy_traffic::TrafficMessage::ChannelQuality(report) =
                            &message
                            && let Some(current) = report.current()
                        {
                            leg.measurements.downlink = Some(current);
                            leg.measurements.downlink_report_ms = Some(now_ms);
                        }
                        if matches!(
                            message,
                            crate::l3::legacy_traffic::TrafficMessage::Status(_)
                        ) && channel != "FACCH"
                        {
                            tracing::warn!(
                                ?party,
                                "status report received on an unsupported channel"
                            );
                            continue;
                        }
                        let power_down = matches!(&message, crate::l3::legacy_traffic::TrafficMessage::Release { information } if information.fields == crate::l3::legacy_traffic::ReleaseFields::Reason(crate::l3::legacy_traffic::ReleaseReason::PowerDown));
                        let release = matches!(
                            &message,
                            crate::l3::legacy_traffic::TrafficMessage::Release { .. }
                        ) || matches!(&message, crate::l3::legacy_traffic::TrafficMessage::Ack { ack_message_type, .. } if *ack_message_type == crate::l3::legacy_traffic::ReleaseInformation::MESSAGE_TYPE);
                        if release && channel != "FACCH" {
                            tracing::warn!(?party, "Release exchange received outside FACCH");
                            continue;
                        }
                        if power_down
                            && !leg.power_down_reported
                            && self.power_down_reports.len() >= MAX_POWER_DOWN_REPORTS
                        {
                            return Err("power-down event queue is full".into());
                        }
                        if let Err(error) =
                            active
                                .session
                                .receive(&leg.assignment.handle, &message, now_ms)
                        {
                            if matches!(error,
                                crate::core::call_session::SessionError::State
                                | crate::core::call_session::SessionError::Message
                                | crate::core::call_session::SessionError::Control(_)
                                | crate::core::call_session::SessionError::ResponseQueueFull
                                | crate::core::call_session::SessionError::Service(crate::core::call_session::ServiceError::Busy | crate::core::call_session::ServiceError::ChangedRetransmission | crate::core::call_session::ServiceError::Fields | crate::core::call_session::ServiceError::NotTransmitted | crate::core::call_session::ServiceError::Closed)
                                | crate::core::call_session::SessionError::Flash(crate::core::call_session::FlashError::RequestNumber|crate::core::call_session::FlashError::Parameter|crate::core::call_session::FlashError::NotTransmitted|crate::core::call_session::FlashError::Closed|crate::core::call_session::FlashError::ChangedRetransmission|crate::core::call_session::FlashError::Fields)
                                | crate::core::call_session::SessionError::Order(
                                    crate::link::traffic_order::OrderError::NotAck
                                    | crate::link::traffic_order::OrderError::AckType { .. }
                                    | crate::link::traffic_order::OrderError::MeasurementChannels
                                    | crate::link::traffic_order::OrderError::MeasurementFields
                                    | crate::link::traffic_order::OrderError::StatusFields
                                    | crate::link::traffic_order::OrderError::ControlFields
                                    | crate::link::traffic_order::OrderError::NotTransmitted
                                    | crate::link::traffic_order::OrderError::Closed
                                )
                            ) {
                                tracing::debug!(?party, %error, ?message, state=?active.session.state(), "traffic message discarded for the current call task");
                                continue;
                            }
                            return Err(error.into());
                        }
                        if power_down && !leg.power_down_reported {
                            if let Some(registration) = &leg.registration {
                                self.power_down_reports.push_back(registration.clone());
                            }
                            leg.power_down_reported = true;
                        }
                        if let crate::l3::legacy_traffic::TrafficMessage::CapabilityUpdateResponse(
                            crate::l3::legacy_traffic::CapabilityResponse::Mobile(capabilities),
                        ) = &message
                        {
                            leg.measurements.capabilities = Some(capabilities.clone());
                            leg.measurements.capability_report_ms = Some(now_ms);
                            tracing::info!(call_id=active.session.handle().id().value(), ?party, capabilities=?capabilities, "phone capabilities received");
                        }
                        if let crate::l3::legacy_traffic::TrafficMessage::PhysicalControlAck(fields) =
                            &message
                            && active.session.physical_control_state(party)
                                == Some(crate::link::traffic_order::OrderState::Acknowledged)
                        {
                            leg.measurements
                                .physical_control
                                .get_or_insert_with(Default::default)
                                .update(fields);
                            leg.measurements.physical_control_report_ms = Some(now_ms);
                            tracing::info!(?party, ?fields, "phone physical controls acknowledged");
                        }
                        if let crate::l3::legacy_traffic::TrafficMessage::Status(fields) = &message
                        {
                            leg.measurements.status.update(fields);
                            leg.measurements.status_report_ms = Some(now_ms);
                            tracing::info!(call_id=active.session.handle().id().value(), ?party, identity=%leg.assignment.subscriber.identity, esn=?fields.serial_numbers, terminal=?fields.terminals, "phone status received");
                        }
                        if matches!(
                            message,
                            crate::l3::legacy_traffic::TrafficMessage::ChannelQuality(_)
                        ) {
                            tracing::debug!(?party, "channel quality received");
                        } else {
                            tracing::info!(?party,state=?active.session.state(),"call signaling received");
                        }
                    }
                    Err(error) => tracing::debug!(%error,"traffic message discarded"),
                    _ => {}
                }
            }
            if timing_failure
                && active.session.state() == crate::core::call_session::CallState::Connected
            {
                tracing::warn!(call_id=active.session.handle().id().value(), ?party, residual_symbols=?leg.timing.residual_symbols, "phone timing remains outside the supported range. Releasing call");
                active
                    .session
                    .end(EndReason::TimingOutOfRange(party), now_ms)?;
            }
            if active.session.media_ready()
                && let Some(frame) = result.frame.as_ref()
                && let media::Source::Sip { audio, .. } = &leg.source
            {
                audio.uplink.force_push(frame.samples.map(|sample| {
                    (sample * 32768.0)
                        .round()
                        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
                }));
            }
            if active.session.media_ready()
                && let Some(frame) = result.frame
                && let Some(destination) = active.legs.get_mut(1 - index(party))
            {
                if destination.transcoding {
                    destination.media_pcm = Some(frame.samples);
                } else if matches!(
                    frame.kind,
                    FrameKind::Speech {
                        crc: CrcStatus::Valid,
                        ..
                    }
                ) {
                    destination.media = frame.parameters;
                }
            }

            self.receive_overlap(
                (call, party),
                physical,
                residual_symbols,
                segment,
                channel_profile,
                now_ms,
            )?;
        }
        Ok(())
    }
    pub fn advance(&mut self, now_ms: u64) -> Result<(), Box<dyn Error>> {
        self.rejections
            .retain(|rejection| now_ms < rejection.deadline_ms);
        let expiring: Vec<u64> = self
            .pages
            .iter()
            .filter(|page| now_ms >= page.request.deadline_monotonic_ms)
            .map(Page::call_id)
            .collect();
        let pending_calls: Vec<_> = if expiring.is_empty() {
            Vec::new()
        } else {
            self.operation_calls(now_ms)
                .into_iter()
                .filter(|call| expiring.contains(&call.id))
                .collect()
        };
        let expired = self.paging.advance(now_ms)?;
        for outcome in &expired {
            if let PagingOutcome::Expired(reservation) = outcome {
                if let Err(error) = self.reject_access(
                    reservation.caller.identity,
                    crate::l3::registration::RejectCause::ServiceCodeNotAvailable,
                    now_ms,
                ) {
                    tracing::warn!(%error,"expired caller rejection could not be queued");
                }
                if let Err(error) = self.reject_page_response(
                    reservation.callee.identity,
                    crate::l3::registration::RejectCause::ServiceCodeNotAvailable,
                    now_ms,
                ) {
                    tracing::warn!(%error,"expired callee release could not be queued");
                }
            }
        }
        if !expired.is_empty() {
            let expired_ids: Vec<u64> = expired.iter().filter_map(outcome_call_id).collect();
            for mut call in pending_calls
                .into_iter()
                .filter(|call| expired_ids.contains(&call.id))
            {
                call.set_phase(crate::operations::proto::CallPhase::Ended);
                call.end_reason = "Paging expired".into();
                self.remember_call(call);
            }
            self.pages
                .retain(|page| !expired_ids.contains(&page.call_id()));
            tracing::info!(call_ids=?expired_ids, "call paging expired");
        }
        for call in 0..self.calls.len() {
            self.advance_call(call, now_ms)?;
        }
        let mut call = 0;
        while call < self.calls.len() {
            let active = &self.calls[call];
            let Some(handle) = active
                .session
                .stopped_handle()
                .filter(|_| active.legs.iter().all(|leg| leg.in_flight == 0))
                .cloned()
            else {
                call += 1;
                continue;
            };
            let call_id = handle.id().value();
            if let Some(record) = self.call_history(call_id, now_ms).into_iter().next() {
                self.remember_call(record);
            }
            if self.calls[call].audio {
                self.paging.release_audio_stopped(&handle)?;
            } else {
                self.paging.release_stopped(&handle)?;
            }
            self.calls.remove(call);
            self.forget_assignments(call_id);
            tracing::info!(call_id, "call ended and traffic slots released");
        }
        Ok(())
    }
    fn advance_call(&mut self, call: usize, now_ms: u64) -> Result<(), Box<dyn Error>> {
        let active = &mut self.calls[call];
        active.session.advance(now_ms)?;
        active.sample_answer.advance(&mut active.session, now_ms)?;
        for party in active.legs.present().into_iter().flatten() {
            if active.session.state() == crate::core::call_session::CallState::Connected
                && active.session.status_state(party).is_none()
                && matches!(
                    active.session.measurement_state(party),
                    Some(
                        crate::link::traffic_order::OrderState::Acknowledged
                            | crate::link::traffic_order::OrderState::TimedOut
                            | crate::link::traffic_order::OrderState::Cancelled
                    )
                )
            {
                active.session.request_status(party, now_ms)?;
            }
            if active.session.state() == crate::core::call_session::CallState::Connected
                && active.session.capability_state(party).is_none()
                && matches!(
                    active.session.status_state(party),
                    Some(
                        crate::link::traffic_order::OrderState::Acknowledged
                            | crate::link::traffic_order::OrderState::TimedOut
                            | crate::link::traffic_order::OrderState::Cancelled
                    )
                )
            {
                active.session.request_capabilities(party, now_ms)?;
            }
            if active.session.state() == crate::core::call_session::CallState::Connected
                && !active.legs[index(party)].controls_initialized
                && !matches!(
                    active.session.physical_control_state(party),
                    Some(
                        crate::link::traffic_order::OrderState::Prepared
                            | crate::link::traffic_order::OrderState::AwaitingAck { .. }
                    )
                )
                && matches!(
                    active.session.capability_state(party),
                    Some(
                        crate::link::traffic_order::OrderState::Acknowledged
                            | crate::link::traffic_order::OrderState::TimedOut
                            | crate::link::traffic_order::OrderState::Cancelled
                    )
                )
            {
                active.session.request_physical_control(
                    party,
                    crate::l3::legacy_traffic::PhysicalControlRequest {
                        power_level: Some(self.traffic_power_level),
                        time_alignment: Some(TimeAlignment::Maintain),
                        dtx_allowed: Some(false),
                        delay_compensation: Some(false),
                    },
                    now_ms,
                )?;
                active.legs[index(party)].controls_initialized = true;
            }
            let leg = active
                .legs
                .get_mut(index(party))
                .ok_or("call party has no radio bearer")?;
            if active.session.bearer_state(party) == BearerState::Established
                && now_ms.saturating_sub(leg.last_rx_ms) >= self.radio_loss_ms
            {
                active.session.end(EndReason::RadioFailure(party), now_ms)?;
            }
            if leg.order.as_ref().is_some_and(|(_, queued)| {
                active
                    .session
                    .order(party)
                    .is_none_or(|current| !current.matches(queued))
            }) {
                let (handle, _) = leg.order.take().expect("obsolete order exists");
                leg.tx.cancel_facch(&handle)?;
            }
            if leg.response.as_ref().is_some_and(|(_, queued)| {
                active
                    .session
                    .response(party)
                    .is_none_or(|current| !current.matches(queued))
            }) {
                let (handle, _) = leg.response.take().expect("obsolete response exists");
                leg.tx.cancel_facch(&handle)?;
            }
            if leg.service.as_ref().is_some_and(|(_, queued)| {
                active
                    .session
                    .service_transmission(party)
                    .is_none_or(|current| !current.matches(queued))
            }) {
                let (handle, _) = leg
                    .service
                    .take()
                    .expect("obsolete service response exists");
                leg.tx.cancel_facch(&handle)?;
            }
            if leg.flash.as_ref().is_some_and(|(_, queued)| {
                active
                    .session
                    .flash_transmission(party)
                    .is_none_or(|current| !current.matches(queued))
            }) {
                let (handle, _) = leg
                    .flash
                    .take()
                    .expect("obsolete Flash transmission exists");
                leg.tx.cancel_facch(&handle)?;
            }
            if !active.session.media_ready() {
                active.dtmf[index(party)].clear();
                leg.media = None;
                leg.media_pcm = None;
            }
            if let Some(request) = active.session.stop_request(party).cloned() {
                if leg.in_flight == 0 {
                    active.session.bearer_stopped(&request, now_ms)?;
                    tracing::info!(?party, "traffic bearer stopped");
                }
                continue;
            }
            if leg.response.is_none()
                && let Some(response) = active.session.response(party).cloned()
                && let Some(handle) = queue::facch(&mut leg.tx, response.message())?
            {
                leg.response = Some((handle, response));
            }
            if leg.service.is_none()
                && let Some(tx) = active.session.service_transmission(party).cloned()
                && let Some(handle) = queue::facch(&mut leg.tx, tx.message())?
            {
                leg.service = Some((handle, tx));
            }
            if leg.flash.is_none()
                && let Some(tx) = active.session.flash_transmission(party).cloned()
                && let Some(handle) = queue::facch(&mut leg.tx, tx.message())?
            {
                leg.flash = Some((handle, tx));
            }
            if leg.order.is_none()
                && let Some(order) = active.session.order(party).cloned()
                && let Some(handle) = queue::facch(&mut leg.tx, order.message())?
            {
                leg.order = Some((handle, order));
            }
        }
        let call_id = active.session.handle().id().value();
        if matches!(
            active.session.state(),
            crate::core::call_session::CallState::Releasing(_)
                | crate::core::call_session::CallState::Stopped(_)
        ) {
            self.forget_assignments(call_id);
        }
        Ok(())
    }
    pub fn shutdown(&mut self, now_ms: u64) -> Result<(), Box<dyn Error>> {
        for active in &mut self.calls {
            active.session.radio_shutdown(now_ms)?;
        }
        let mut calls = self.operation_calls(now_ms);
        for call in &mut calls {
            call.set_phase(crate::operations::proto::CallPhase::Ended);
            call.end_reason = "Stopped (cell shutdown)".into();
        }
        for call in calls {
            self.remember_call(call);
        }
        for page in std::mem::take(&mut self.pages) {
            if self.paging.is_audio(&page.request.handle) {
                self.paging.cancel_audio(&page.request.handle, now_ms)?;
            } else {
                self.paging.cancel(&page.request.handle, now_ms)?;
            }
        }
        for active in std::mem::take(&mut self.calls) {
            if let Some(handle) = active.session.stopped_handle() {
                if active.audio {
                    self.paging.release_audio_stopped(handle)?;
                } else {
                    self.paging.release_stopped(handle)?;
                }
            }
        }
        self.assignments.clear();
        self.repeats.clear();
        Ok(())
    }
}

fn outcome_call_id(outcome: &PagingOutcome) -> Option<u64> {
    match outcome {
        PagingOutcome::Expired(reservation) | PagingOutcome::Rejected(reservation) => {
            Some(reservation.handle.id().value())
        }
        PagingOutcome::AudioExpired(reservation) | PagingOutcome::AudioRejected(reservation) => {
            Some(reservation.handle.id().value())
        }
        PagingOutcome::Ready { .. } | PagingOutcome::AudioReady { .. } => None,
    }
}
pub(crate) fn page_response_rejection_cause(
    error: &(dyn Error + 'static),
) -> Option<crate::l3::registration::RejectCause> {
    use crate::core::call_paging::PagingError;
    if matches!(
        error.downcast_ref::<PagingError>(),
        Some(
            PagingError::Wire(_)
                | PagingError::Options(_)
                | PagingError::OptionalElements
                | PagingError::DuplicateResponse
                | PagingError::MissingResponse
                | PagingError::NotTransmitted
                | PagingError::ClockRegression
                | PagingError::TimeOverflow
                | PagingError::OutputCapacity
                | PagingError::TimeoutDuration
        )
    ) {
        None
    } else {
        Some(access_rejection_cause(error))
    }
}
pub(crate) fn access_rejection_cause(
    error: &(dyn Error + 'static),
) -> crate::l3::registration::RejectCause {
    use crate::core::{call_paging::PagingError, calls::CallError};
    match error.downcast_ref::<PagingError>() {
        Some(PagingError::Admission(reason) | PagingError::Call(CallError::Admission(reason))) => {
            reason.reject_cause()
        }
        Some(PagingError::Service | PagingError::Call(CallError::Service(_))) => {
            crate::l3::registration::RejectCause::ServiceCodeNotSupported
        }
        _ => crate::l3::registration::RejectCause::ServiceCodeNotAvailable,
    }
}
struct ReversePosition {
    physical_slot: u64,
    residual_symbols: f64,
}
fn reverse_position(relative_symbols: f64) -> Option<ReversePosition> {
    let slot_symbols = (crate::phy::traffic::SLOT_BITS / 2) as f64;
    let position = (relative_symbols + STANDARD_REVERSE_LEAD_SYMBOLS) / slot_symbols;
    let physical = position.round();
    if !physical.is_finite()
        || physical < 0.0
        || ((position - physical) * slot_symbols).abs()
            > MAX_RX_TIMING_ERROR_SYMBOLS
                + f64::from(crate::l3::legacy_traffic::MAX_ALIGNMENT_UNITS) / 2.0
    {
        return None;
    }
    Some(ReversePosition {
        physical_slot: physical as u64,
        residual_symbols: (position - physical) * slot_symbols,
    })
}
#[cfg(test)]
impl VoiceRuntime {
    fn active(&self) -> Option<&Active> {
        self.calls.first()
    }
    fn active_mut(&mut self) -> Option<&mut Active> {
        self.calls.first_mut()
    }
    fn page(&self) -> Option<&Page> {
        self.pages.first()
    }
    fn call_id(&self) -> Option<u64> {
        self.calls
            .first()
            .map(|active| active.session.handle().id().value())
    }
}
#[cfg(test)]
mod tests;
