mod access;
mod dtmf;
mod extended;
mod flash;
mod paging;
mod service;
use super::*;
use crate::{
    codec::vselp::{SUBFRAMES, VselpParameters, VselpSubframe},
    core::store::StateStore,
    l3::{
        address::{AddressInfo, AddressValue},
        designation::{self, AssignedSlots},
        legacy_traffic::{
            ChannelQualityReport, ControlChannel, DownlinkQuality, Hyperband, ReleaseFields,
            ReleaseInformation, ReleaseReason, StatusFields, TerminalInformation, TrafficMessage,
            TrafficRelay,
        },
        origination::Origination,
        page_response::{PageResponse, ServiceCode},
        relay::{
            Direction, HigherLayerProtocol, RelayMessage,
            addressing::{AddressedRelay, RelayAddresses},
        },
        sms::{Metadata, SmsMessage, UserData},
    },
    link::{
        bearer_rx::BearerRx,
        sacch_tx::SacchTx,
        traffic::encode as encode_signaling,
        traffic_tx::TrafficTx,
        voice::{FrameKind, TransmitFrame},
    },
    phy::{acquisition::decode_forward_traffic, color::encode_dvcc, speech::CrcStatus},
};
const DVCC: u8 = 1;
const DESIGNATION_TYPE: u8 = 21;
const COLOR_BITS: usize = 12;
const CALLER: usize = 0;
const FIRST_CALL_ID: u64 = 1;
const CALLEE: usize = 1;
const OBSERVE_SLOTS: usize = 12;
const WINDOW_SLOTS: usize = 24;
const TX_LEAD_SLOTS: u64 = 12;
const TX_HORIZON_SLOTS: u64 = 24;
const RESPONSE_LEAD_BLOCKS: u64 = 4;
const TDMA_BLOCK_MS: u64 = 20;
const ANSWER_INTERVALS: u64 = 10;
const STOP_SLOTS_AFTER_ACK: u64 = 3;
const FACCH_QUEUE_WORDS: usize = 16;
const FIRST_ORIGINATION_BLOCK: u64 = 20;
const SECOND_REGISTRATION_BLOCK: u64 = 12;
const TEST_STOP_SLOTS: usize = 1200;
const TEST_DURATION_SECONDS: u64 = 12;
const SMS_TEST_STOP_SLOTS: usize = 3000;
const SMS_TEST_DURATION_SECONDS: u64 = 24;
const MAXIMUM_SMS_TEST_STOP_SLOTS: usize = 5100;
const MAXIMUM_SMS_TEST_DURATION_SECONDS: u64 = 38;
const SMS_TRANSACTION: u8 = 4;
const SMS_TEXT: [&[u8]; 2] = [
    b"in-call message from caller",
    b"in-call message from callee",
];
const UNANSWERED_TIMEOUT_SECONDS: u64 = 1;
const PROPAGATION_HALF_SYMBOLS: [u8; 2] = [4, 3];
const DRIFT_HALF_SYMBOLS: u8 = 4;
const OUT_OF_RANGE_HALF_SYMBOLS: u8 = 34;
const ADVANCED_SPEECH_INTERVALS: u64 = 10;
const MIN_SUPPORTED_POWER_LEVEL: u8 = 6;
const OPERATOR_POWER_LEVEL: u32 = 3;
const OPERATOR_ADVANCE_UNITS: u32 = 30;
const REQUIRED_SPEECH_FRAMES: usize = 5;
const SACCH_WORD_SPAN_SLOTS: u64 =
    (crate::phy::sacch::WORD_SLOTS + crate::phy::traffic::SACCH_BITS - 1) as u64;
const MEASUREMENT_REPORT_INTERVALS: u64 = 50;
const FACCH_RESPONSE_TIMEOUT_MS: u64 = 200;
const REPORT_OBSERVATION_INTERVALS: u64 = 40;
const ENERGY_BASE: [u8; 2] = [14, 20];
const ENERGY_VARIANTS: u64 = 3;
const STATION_CLASS_MARK: u8 = 15;
const NUMBER_TYPE: u8 = 0;
const NUMBERING_PLAN: u8 = 1;
const LPC: [u8; 10] = [1; 10];
const LAG: u8 = 20;
const CODE_A: u8 = 31;
const CODE_B: u8 = 42;
const GAIN: u8 = 105;
const DATA_LEAD_BITS: usize = 16;
const DATA_MIDDLE_BITS: usize = 122;
const DATA_BOUNDARY: usize = DATA_LEAD_BITS + DATA_MIDDLE_BITS;
const LEAD_DATA_START: usize = 12;
const SYNC_START: usize = 28;
const MIDDLE_DATA_START: usize = 56;
const SACCH_START: usize = 178;
const COLOR_START: usize = 190;
const FINAL_DATA_START: usize = 202;

fn hal(error: impl std::fmt::Display) -> HalError {
    error.to_string().into()
}
fn parameters(index: usize, interval: u64) -> VselpParameters {
    VselpParameters::try_new(
        ENERGY_BASE[index] + (interval % ENERGY_VARIANTS) as u8,
        LPC,
        [VselpSubframe::try_new(LAG, CODE_A, CODE_B, GAIN).unwrap(); SUBFRAMES],
    )
    .unwrap()
}
struct Leg {
    flash: flash::Probe,
    service: service::Probe,
    dtmf: Option<dtmf::Probe>,
    phase: u64,
    tx: TrafficTx,
    sacch_tx: SacchTx,
    rx: BearerRx,
    next_tx: u64,
    answer_at: Option<u64>,
    report_at: Option<u64>,
    report_sent: bool,
    alerted: bool,
    alert_information: Option<crate::l3::legacy_traffic::AlertInformation>,
    caller_seen_before_answer: bool,
    alert_messages: usize,
    connect_sent: bool,
    connected: bool,
    status_queries: usize,
    shortened: bool,
    shortened_sent: usize,
    propagation_half_symbols: u8,
    drift_applied: bool,
    physical_controls: usize,
    last_physical_control_interval: Option<u64>,
    mobile_power_level: u8,
    timing_advance_units: u8,
    capability_queries: usize,
    capability_responses: usize,
    capability_request_command: Option<crate::link::traffic_tx::CommandHandle>,
    capability_request_interval: Option<u64>,
    status_acknowledged: bool,
    spontaneous_status_sent: bool,
    spontaneous_status_command: Option<crate::link::traffic_tx::CommandHandle>,
    spontaneous_status_interval: Option<u64>,
    sms_sent: bool,
    sms_accepted: bool,
    sms_received: Vec<Vec<u8>>,
    stop_at: Option<u64>,
    release_received: bool,
    powered_down: bool,
    mobile_release_command: Option<crate::link::traffic_tx::CommandHandle>,
    return_channel_seen: bool,
    released: bool,
    speech: Vec<VselpParameters>,
    last_valid_speech_interval: Option<u64>,
    unused_after_release: usize,
}
#[derive(Clone, Copy)]
enum SmsSize {
    Short,
    Maximum,
    International,
}
#[derive(Clone, Copy)]
enum Scenario {
    Connected {
        hangup: usize,
        sms: Option<SmsSize>,
        tones: bool,
        information: bool,
        services: bool,
    },
    Unanswered,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelModel {
    SinglePath,
    TimingOutOfRange,
    DelayedEcho,
    WideSpread,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum AlertFeedback {
    Normal,
    LoseFirstAck,
}
pub(super) struct Handsets {
    extended_gate: Option<Arc<AtomicBool>>,
    scenario: Scenario,
    return_channel: ControlChannel,
    release_reason: ReleaseReason,
    alert_feedback: AlertFeedback,
    channel_model: ChannelModel,
    connect_ack_received: bool,
    control: SpachReceiver,
    page_control: paging::Observer,
    page_reply: paging::ReplyOn,
    replied_page_superframe: Option<u64>,
    page_access: access::PageAccess,
    third_arq: Option<crate::link::spach::arq::ArqReassembler>,
    third_replies: Vec<crate::link::spach::SingleMessage>,
    third_begin_block: Option<u64>,
    assignment_blocks: [Option<u64>; 2],
    last_control: Option<f64>,
    last_traffic: Option<f64>,
    legs: [Option<Leg>; 2],
    pages: usize,
    release_sent: bool,
    second_origin: bool,
}
impl Handsets {
    fn new(
        scenario: Scenario,
        channel_model: ChannelModel,
        mixed_arq: bool,
        allocation: crate::scheduler::paging::PagingAllocation,
        return_channel: ControlChannel,
    ) -> Self {
        Self {
            extended_gate: None,
            scenario,
            return_channel,
            release_reason: ReleaseReason::Normal,
            alert_feedback: AlertFeedback::Normal,
            channel_model,
            connect_ack_received: false,
            control: SpachReceiver::default(),
            page_control: paging::Observer::new(allocation),
            page_reply: paging::ReplyOn::FirstComplete,
            replied_page_superframe: None,
            page_access: access::PageAccess::default(),
            third_arq: mixed_arq.then(|| {
                crate::link::spach::arq::ArqReassembler::try_new(MobileIdentity::Min34(3)).unwrap()
            }),
            third_replies: Vec::new(),
            third_begin_block: None,
            assignment_blocks: [None; 2],
            last_control: None,
            last_traffic: None,
            legs: [None, None],
            pages: 0,
            release_sent: false,
            second_origin: false,
        }
    }
    pub(super) fn observe(&mut self, state: &mut Backend) -> Result<(), HalError> {
        if !state.slot_count.is_multiple_of(OBSERVE_SLOTS) {
            return Ok(());
        }
        let offset = state
            .tx
            .len()
            .saturating_sub(WINDOW_SLOTS * SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL);
        let samples: Vec<_> = state.tx[offset..]
            .iter()
            .map(|sample| Symbol {
                i: f64::from(sample.re),
                q: f64::from(sample.im),
            })
            .collect();
        let channels: &[LogicalChannel] = if self.page_access.awaiting_feedback() {
            &[
                LogicalChannel::Spach,
                LogicalChannel::Fbcch,
                LogicalChannel::Ebcch,
            ]
        } else {
            &[LogicalChannel::Spach]
        };
        let mut bursts = Vec::new();
        for &channel in channels {
            bursts.extend(
                decode_forward_control(&samples, RATE as f64, channel, DVCC)
                    .map_err(hal)?
                    .into_iter()
                    .map(|burst| (channel, burst)),
            );
        }
        bursts.sort_by(|first, second| {
            first
                .1
                .slot_start_sample
                .total_cmp(&second.1.slot_start_sample)
        });
        for (channel, burst) in bursts {
            let first = offset as f64 + burst.slot_start_sample;
            if duplicate(first, self.last_control) {
                continue;
            }
            self.last_control = Some(first);
            let physical = ((first - (FILTER_SPAN * SAMPLES_PER_SYMBOL) as f64)
                / (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL) as f64)
                .round()
                .max(0.0) as u64;
            let block = physical / CONTROL_SLOT_PERIOD;
            self.page_access
                .feedback(
                    block,
                    &crate::link::feedback::ObservedFeedback::from(&burst.fields.feedback),
                    MobileIdentity::Min34(2).partial_echo(),
                )
                .map_err(hal)?;
            if channel != LogicalChannel::Spach {
                continue;
            }
            if let Ok(frame) = crate::link::spach::arq::decode(&burst.fields.block.information) {
                if let crate::link::spach::arq::ArqFrame::Begin {
                    identity: MobileIdentity::Min34(3),
                    ..
                } = &frame
                {
                    self.third_begin_block.get_or_insert(block);
                }
                if let Some(receiver) = &mut self.third_arq
                    && let crate::link::spach::arq::Reception::Accepted {
                        message: Some(message),
                        ..
                    } = receiver.push_frame(frame).map_err(hal)?
                {
                    self.third_replies.push(message);
                }
                continue;
            }
            let information = &burst.fields.block.information;
            let message = if information[..3].iter().all(|bit| *bit) {
                self.page_control.observe(physical, information)?
            } else {
                self.control.push(information).unwrap_or(None)
            };
            let Some(message) = message else {
                continue;
            };
            let index = match message.identity {
                MobileIdentity::Min34(1) => 0,
                MobileIdentity::Min34(2) => 1,
                _ => continue,
            };
            if message.channel == crate::link::spach::Channel::Pch
                && message.message.first() == Some(&crate::l3::page::MESSAGE_TYPE)
            {
                let page = crate::l3::page::Page::decode(&message.message).map_err(hal)?;
                if page.service != ServiceCode::DigitalSpeech
                    || page.presentation
                        != Some(crate::l3::access_call::Presentation {
                            presentation: 0,
                            screening: 3,
                        })
                    || page.calling_number
                        != Some(AddressInfo {
                            number_type: 0,
                            numbering_plan: 1,
                            address: AddressValue::Tbcd("1001".into()),
                        })
                {
                    return Err("Page does not carry the configured network caller identity".into());
                }
                let page_superframe = physical / PHYSICAL_SLOTS;
                if self.page_reply == paging::ReplyOn::Secondary
                    && page_superframe.is_multiple_of(2)
                {
                    continue;
                }
                if self.pages == 0 || self.second_origin {
                    if self.pages == 0 {
                        self.replied_page_superframe = Some(page_superframe);
                    }
                    self.pages += 1;
                    if !self.second_origin {
                        let final_reverse_block = respond(
                            state,
                            MobileIdentity::Min34(2),
                            &crate::l3::access_call::PageResponseRequest {
                                message: PageResponse {
                                    protocol_version: ProtocolVersion::Pv3,
                                    last_try: false,
                                    scm: STATION_CLASS_MARK,
                                    service_code: ServiceCode::DigitalSpeech,
                                },
                                options: crate::l3::access_call::CallOptions {
                                    bandwidth: Some(
                                        crate::l3::access_call::Bandwidth::HalfPreferred,
                                    ),
                                    extended_voices: vec![
                                        crate::l3::capability_report::ExtendedVoice {
                                            privacy: 0,
                                            modes: vec![
                                                crate::l3::capability_report::VoiceMode {
                                                    coder: 3,
                                                    modulation: 0,
                                                    interleaving: 0,
                                                },
                                                crate::l3::capability_report::VoiceMode {
                                                    coder: 1,
                                                    modulation: 0,
                                                    interleaving: 1,
                                                },
                                            ],
                                        },
                                    ],
                                    ..Default::default()
                                },
                            }
                            .encode()
                            .map_err(hal)?,
                        );
                        self.page_access.scheduled(final_reverse_block);
                    }
                }
            } else if message.channel == crate::link::spach::Channel::Arch
                && message.message.first() == Some(&DESIGNATION_TYPE)
            {
                if index == CALLEE {
                    self.page_access.designate(block).map_err(hal)?;
                }
                self.assignment_blocks[index] = Some(block);
                let assignment = designation::decode_prefix(&message.message)
                    .map_err(hal)?
                    .message;
                let phase = match assignment.assigned_slots {
                    AssignedSlots::Full25 => 1,
                    AssignedSlots::Full36 => 2,
                    _ => return Err("unexpected traffic assignment".into()),
                };
                if self.legs[index].is_some() {
                    return Err("duplicate traffic assignment".into());
                }
                let first_interval =
                    (state.slot_count as u64 + TX_LEAD_SLOTS - phase).div_ceil(CONTROL_SLOT_PERIOD);
                self.legs[index] = Some(Leg {
                    service: service::Probe::new(matches!(
                        self.scenario,
                        Scenario::Connected { services: true, .. }
                    )),
                    flash: flash::Probe::new(matches!(
                        self.scenario,
                        Scenario::Connected {
                            information: true,
                            ..
                        }
                    )),
                    dtmf: matches!(self.scenario, Scenario::Connected { tones: true, .. })
                        .then(dtmf::Probe::default),
                    phase,
                    tx: TrafficTx::try_new(
                        DVCC,
                        handset_queue_words(self.scenario),
                        first_interval,
                    )
                    .map_err(hal)?,
                    sacch_tx: SacchTx::try_new(
                        DVCC,
                        handset_queue_words(self.scenario),
                        first_interval,
                    )
                    .map_err(hal)?,
                    rx: BearerRx::with_version(
                        DVCC,
                        VselpParameters::default(),
                        crate::l3::relay::Direction::Forward,
                        assignment.protocol_version,
                    ),
                    next_tx: first_interval,
                    answer_at: None,
                    report_at: None,
                    report_sent: false,
                    alerted: false,
                    alert_information: None,
                    caller_seen_before_answer: false,
                    alert_messages: 0,
                    connect_sent: false,
                    connected: false,
                    status_queries: 0,
                    shortened: assignment.shortened_burst,
                    shortened_sent: 0,
                    propagation_half_symbols: PROPAGATION_HALF_SYMBOLS[index],
                    drift_applied: false,
                    physical_controls: 0,
                    last_physical_control_interval: None,
                    mobile_power_level: assignment.dmac,
                    timing_advance_units: 0,
                    capability_queries: 0,
                    capability_responses: 0,
                    capability_request_command: None,
                    capability_request_interval: None,
                    status_acknowledged: false,
                    spontaneous_status_sent: false,
                    spontaneous_status_command: None,
                    spontaneous_status_interval: None,
                    sms_sent: false,
                    sms_accepted: false,
                    sms_received: Vec::new(),
                    stop_at: None,
                    release_received: false,
                    powered_down: false,
                    mobile_release_command: None,
                    return_channel_seen: false,
                    released: false,
                    speech: Vec::new(),
                    last_valid_speech_interval: None,
                    unused_after_release: 0,
                });
            }
        }
        for burst in decode_forward_traffic(&samples, RATE as f64).map_err(hal)? {
            let first = offset as f64 + burst.slot_start_sample;
            if duplicate(first, self.last_traffic) {
                continue;
            }
            self.last_traffic = Some(first);
            let physical = ((first - (FILTER_SPAN * SAMPLES_PER_SYMBOL) as f64)
                / (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL) as f64)
                .round() as u64;
            for (index, leg) in self.legs.iter_mut().enumerate() {
                let Some(leg) = leg.as_mut() else { continue };
                if physical % CONTROL_SLOT_PERIOD != leg.phase
                    || burst.fields.fields.sync != SyncWord::ALL[leg.phase as usize]
                {
                    continue;
                }
                if burst.fields.fields.color.dvcc != DVCC {
                    if leg.released {
                        leg.unused_after_release += 1;
                    }
                    continue;
                }
                if leg.powered_down {
                    continue;
                }
                let interval = physical / CONTROL_SLOT_PERIOD;
                let received = leg
                    .rx
                    .push(interval, 0, Some(&burst.fields.fields))
                    .map_err(hal)?;
                if matches!(
                    received.frame.as_ref().map(|frame| &frame.kind),
                    Some(FrameKind::Speech {
                        crc: CrcStatus::Valid,
                        ..
                    })
                ) {
                    leg.last_valid_speech_interval = Some(interval);
                }
                for (channel_index, result) in [received.facch_message, received.sacch_message]
                    .into_iter()
                    .enumerate()
                {
                    let Ok(Some(message)) = result else {
                        continue;
                    };
                    match message {
                        TrafficMessage::Service(message) => {
                            if channel_index != 0 {
                                return Err("service response outside FACCH".into());
                            }
                            leg.service.receive(interval, message, &mut leg.tx)?;
                        }
                        TrafficMessage::Ack {
                            ack_message_type, ..
                        } if ack_message_type
                            == crate::l3::legacy_traffic::ServiceMessage::REQUEST_TYPE =>
                        {
                            if channel_index != 0 {
                                return Err("service request ACK outside FACCH".into());
                            }
                            leg.service.acknowledged(interval)?;
                        }
                        TrafficMessage::Flash(message) => {
                            if channel_index != 0 {
                                return Err("Flash received outside FACCH".into());
                            }
                            leg.flash.receive(index, interval, message, &mut leg.tx)?;
                        }
                        TrafficMessage::Dtmf(ack) => {
                            if channel_index != 0 {
                                return Err("DTMF acknowledgment outside FACCH".into());
                            }
                            leg.dtmf
                                .as_mut()
                                .ok_or("unexpected DTMF acknowledgment")?
                                .acknowledgment(index, interval, &ack)?;
                        }
                        TrafficMessage::PhysicalControl(fields) => {
                            if channel_index != 0 {
                                return Err(
                                    "physical control requested on unexpected channel".into()
                                );
                            }
                            leg.physical_controls += 1;
                            leg.last_physical_control_interval = Some(leg.next_tx);
                            if let Some(level) = fields.control.power_level {
                                leg.mobile_power_level = level.max(MIN_SUPPORTED_POWER_LEVEL);
                            }
                            if let Some(designation::TimeAlignment::OffsetUnits(units)) =
                                fields.control.time_alignment
                            {
                                leg.timing_advance_units = units;
                                leg.shortened = false;
                            }
                            let mut actual = fields;
                            if actual.control.power_level.is_some() {
                                actual.control.power_level = Some(leg.mobile_power_level);
                            }
                            if actual.control.time_alignment.is_some() {
                                actual.control.time_alignment =
                                    Some(designation::TimeAlignment::OffsetUnits(
                                        leg.timing_advance_units,
                                    ));
                            }
                            leg.tx
                                .enqueue(&TrafficMessage::PhysicalControlAck(actual))
                                .map_err(hal)?;
                        }
                        TrafficMessage::CapabilityUpdateRequest(version) => {
                            if channel_index != 0 || version != ProtocolVersion::Pv2 {
                                return Err(
                                    "unexpected capability request channel or version".into()
                                );
                            }
                            leg.capability_queries += 1;
                            leg.tx
                                .enqueue(&TrafficMessage::Ack {
                                    ack_message_type: TrafficMessage::CapabilityUpdateRequest(
                                        version,
                                    )
                                    .message_type(),
                                    information: Default::default(),
                                })
                                .map_err(hal)?;
                            let response = capability_report(index);
                            leg.tx
                                .enqueue(&TrafficMessage::CapabilityUpdateResponse(
                                    crate::l3::legacy_traffic::CapabilityResponse::Mobile(response),
                                ))
                                .map_err(hal)?;
                            if leg.capability_request_command.is_none()
                                && leg.capability_request_interval.is_none()
                                && leg.capability_responses == 0
                            {
                                leg.capability_request_command = Some(
                                    leg.tx
                                        .enqueue(&TrafficMessage::CapabilityUpdateRequest(
                                            ProtocolVersion::Pv7,
                                        ))
                                        .map_err(hal)?,
                                );
                            }
                        }
                        TrafficMessage::CapabilityUpdateResponse(
                            crate::l3::legacy_traffic::CapabilityResponse::BaseStation(
                                capabilities,
                            ),
                        ) => {
                            if channel_index != 0
                                || capabilities
                                    != super::super::super::capabilities::station_response()
                            {
                                return Err(
                                    "cell advertised unexpected capability services or version"
                                        .into(),
                                );
                            }
                            let sent = leg
                                .capability_request_interval
                                .take()
                                .ok_or("capability response arrived without a completed request")?;
                            let elapsed_symbols = interval.checked_sub(sent).ok_or(
                                "capability response arrived before final request transmission",
                            )? * CONTROL_SLOT_PERIOD
                                * SYMBOLS_PER_SLOT as u64
                                + REVERSE_LEAD_SYMBOLS as u64;
                            if elapsed_symbols as f64 * 1000.0
                                > SYMBOL_RATE_HZ * FACCH_RESPONSE_TIMEOUT_MS as f64
                            {
                                return Err(
                                    "capability response missed its FACCH response deadline".into(),
                                );
                            }
                            leg.capability_responses += 1;
                        }

                        TrafficMessage::StatusRequest(fields) => {
                            if fields != StatusFields::identity_query() {
                                return Err("unexpected status query".into());
                            }
                            leg.status_queries += 1;
                            leg.tx
                                .enqueue(&TrafficMessage::Status(status_report(index)))
                                .map_err(hal)?;
                        }
                        TrafficMessage::Ack {
                            ack_message_type, ..
                        } if ack_message_type
                            == TrafficMessage::Status(StatusFields::default()).message_type() =>
                        {
                            if leg.spontaneous_status_sent
                                && !leg.status_acknowledged
                                && leg.spontaneous_status_interval.is_none()
                            {
                                return Err("Status acknowledgment arrived without a completed mobile transmission".into());
                            }
                            if let Some(sent) = leg.spontaneous_status_interval.take() {
                                let elapsed_symbols = interval.checked_sub(sent).ok_or("Status acknowledgment arrived before final mobile transmission")?
                                    * CONTROL_SLOT_PERIOD
                                    * SYMBOLS_PER_SLOT as u64
                                    + REVERSE_LEAD_SYMBOLS as u64;
                                if elapsed_symbols as f64 * 1000.0
                                    > SYMBOL_RATE_HZ * FACCH_RESPONSE_TIMEOUT_MS as f64
                                {
                                    return Err("spontaneous Status acknowledgment missed its FACCH response deadline".into());
                                }
                            }
                            leg.status_acknowledged = true;
                        }
                        TrafficMessage::Relay(relay) => match relay.message {
                            RelayMessage::Accept {
                                transaction_id: SMS_TRANSACTION,
                            } => leg.sms_accepted = true,
                            RelayMessage::Data {
                                transaction_id,
                                protocol: HigherLayerProtocol::Sms,
                                payload,
                            } => {
                                let SmsMessage::Deliver { user_data, .. } =
                                    crate::l3::sms::decode_prefix(&payload, Direction::Forward)
                                        .map_err(hal)?
                                        .message
                                else {
                                    return Err("unexpected SMS payload on traffic channel".into());
                                };
                                let text = user_data
                                    .text()
                                    .ok_or_else(|| hal("expected text traffic SMS"))?
                                    .into_bytes();
                                if text != sms_text(self.scenario, 1 - index) {
                                    return Err("incorrect traffic SMS text".into());
                                }
                                // TIA-136-132 §§1.2/1.3 keeps SACCH active when valid FACCH replaces speech.
                                let valid_frame = matches!(
                                    received.frame.as_ref().map(|frame| &frame.kind),
                                    Some(
                                        FrameKind::Speech {
                                            crc: CrcStatus::Valid,
                                            ..
                                        } | FrameKind::Facch(_)
                                    )
                                );
                                let speech_during_word =
                                    leg.last_valid_speech_interval.is_some_and(|last| {
                                        interval
                                            .checked_sub(last)
                                            .is_some_and(|elapsed| elapsed < SACCH_WORD_SPAN_SLOTS)
                                    });
                                if channel_index != 1 || !valid_frame || !speech_during_word {
                                    return Err(format!("traffic SMS did not retain valid speech during its SACCH word: leg={index} interval={interval} channel={channel_index} last_speech={:?} frame={:?}",leg.last_valid_speech_interval,received.frame.as_ref().map(|frame|&frame.kind)).into());
                                }
                                if matches!(
                                    received.frame.as_ref().map(|frame| &frame.kind),
                                    Some(FrameKind::Facch(_))
                                ) {
                                    eprintln!(
                                        "SACCH SMS completion shares valid FACCH: leg={index} interval={interval} last_speech={:?}",
                                        leg.last_valid_speech_interval
                                    );
                                }
                                leg.sms_received.push(text);
                                leg.tx
                                    .enqueue(&TrafficMessage::Relay(TrafficRelay {
                                        message: RelayMessage::Accept { transaction_id },
                                        optional_octets: vec![],
                                    }))
                                    .map_err(hal)?;
                            }
                            _ => return Err("unexpected traffic SMS relay response".into()),
                        },
                        TrafficMessage::ExtendedMeasurementOrder(order) => {
                            if channel_index != 0 {
                                return Err("extended order outside FACCH".into());
                            }
                            extended::respond(leg, order)?;
                        }
                        TrafficMessage::MeasurementOrder { rf_channels } => {
                            if !rf_channels.is_empty() {
                                return Err("unexpected neighbor measurements".into());
                            }
                            leg.tx
                                .enqueue(&TrafficMessage::MeasurementOrderAck { rf_channels })
                                .map_err(hal)?;
                            leg.report_at
                                .get_or_insert(leg.next_tx + MEASUREMENT_REPORT_INTERVALS);
                        }
                        TrafficMessage::StopMeasurements => {
                            leg.report_at = None;
                            leg.tx
                                .enqueue(&TrafficMessage::Ack {
                                    ack_message_type: TrafficMessage::StopMeasurements
                                        .message_type(),
                                    information: Default::default(),
                                })
                                .map_err(hal)?;
                        }
                        TrafficMessage::AlertWithInfo { ref information } => {
                            if index != CALLEE || channel_index != 0 {
                                return Err(
                                    "Alert With Info reached the wrong leg or signaling channel"
                                        .into(),
                                );
                            }
                            let expected = crate::l3::legacy_traffic::CallingNumber {
                                number_type: 0,
                                numbering_plan: 1,
                                presentation: 0,
                                screening: 3,
                                characters: "1001".into(),
                            };
                            if information.calling_numbers != vec![expected]
                                || information.signals.is_some()
                                || information.calling_name.is_some()
                                || !information.unparsed_bits.is_empty()
                            {
                                return Err("Alert With Info does not contain the configured caller identity".into());
                            }
                            if let Some(previous) = &leg.alert_information {
                                if previous != information {
                                    return Err("Alert retry changed caller fields".into());
                                }
                            } else {
                                if leg.connect_sent || leg.connected {
                                    return Err(
                                        "Caller identity arrived only after the phone answered"
                                            .into(),
                                    );
                                }
                                leg.alert_information = Some(information.clone());
                                leg.caller_seen_before_answer = true;
                            }
                            leg.alert_messages += 1;
                            if self.alert_feedback == AlertFeedback::Normal
                                || leg.alert_messages > 1
                            {
                                leg.tx
                                    .enqueue(&TrafficMessage::Ack {
                                        ack_message_type: message.message_type(),
                                        information: Default::default(),
                                    })
                                    .map_err(hal)?;
                            }
                            if !leg.spontaneous_status_sent {
                                let command = leg
                                    .tx
                                    .enqueue(&TrafficMessage::Status(StatusFields {
                                        call_modes: Some(vec![3]),
                                        ..Default::default()
                                    }))
                                    .map_err(hal)?;
                                leg.spontaneous_status_command = Some(command);
                                leg.spontaneous_status_sent = true;
                            }
                            if !leg.alerted {
                                leg.alerted = true;
                                if self.alert_feedback == AlertFeedback::Normal {
                                    leg.answer_at = Some(leg.next_tx + ANSWER_INTERVALS);
                                }
                            }
                            if self.alert_feedback == AlertFeedback::LoseFirstAck
                                && leg.alert_messages > 1
                                && leg.answer_at.is_none()
                            {
                                leg.answer_at = Some(leg.next_tx + ANSWER_INTERVALS);
                            }
                        }
                        TrafficMessage::Ack {
                            ack_message_type, ..
                        } if ack_message_type == TrafficMessage::Connect.message_type() => {
                            if !leg.connect_sent {
                                return Err("Connect acknowledged before mobile answer".into());
                            }
                            leg.connected = true;
                            self.connect_ack_received = true;
                        }
                        TrafficMessage::Ack {
                            ack_message_type,
                            information,
                        } if ack_message_type == ReleaseInformation::MESSAGE_TYPE => {
                            if information.control_channels != vec![self.return_channel]
                                || information.last_decoded_parameter.is_some()
                                || !information.unparsed_bits.is_empty()
                            {
                                return Err(
                                    "Release ACK lacks the configured return-to-control target"
                                        .into(),
                                );
                            }
                            leg.return_channel_seen = true;
                            if !self.release_sent {
                                return Err("Release acknowledged before mobile release".into());
                            }
                            leg.released = true;
                            leg.stop_at = Some(leg.next_tx);
                        }
                        TrafficMessage::Release { ref information } => {
                            if information.fields
                                != ReleaseFields::ControlChannels(vec![self.return_channel])
                                || !information.unparsed_bits.is_empty()
                            {
                                return Err(
                                    "Forward Release lacks the configured return-to-control target"
                                        .into(),
                                );
                            }
                            leg.return_channel_seen = true;
                            leg.tx
                                .enqueue(&TrafficMessage::Ack {
                                    ack_message_type: message.message_type(),
                                    information: Default::default(),
                                })
                                .map_err(hal)?;
                            leg.release_received = true;
                            leg.released = true;
                            leg.stop_at = Some(leg.next_tx + STOP_SLOTS_AFTER_ACK);
                        }
                        _ => return Err("unexpected forward traffic message".into()),
                    }
                }
                if let Some(frame) = &received.frame
                    && matches!(
                        frame.kind,
                        FrameKind::Speech {
                            crc: CrcStatus::Valid,
                            ..
                        }
                    )
                    && let Some(probe) = &mut leg.dtmf
                {
                    probe.audio(&frame.samples);
                }
                if let Some(frame) = received.frame
                    && matches!(
                        frame.kind,
                        FrameKind::Speech {
                            crc: CrcStatus::Valid,
                            ..
                        }
                    )
                    && let Some(parameters) = frame.parameters
                    && (0..ENERGY_VARIANTS)
                        .any(|variant| parameters == self::parameters(1 - index, variant))
                {
                    if !self.connect_ack_received {
                        return Err("speech arrived before Connect acknowledgment".into());
                    }
                    if let Some(probe) = &mut leg.dtmf {
                        probe.restored_speech(interval);
                    }
                    leg.service.speech();
                    leg.speech.push(parameters);
                }
            }
        }
        if let Scenario::Connected { hangup, sms, .. } = self.scenario
            && !self.release_sent
            && !matches!(
                self.channel_model,
                ChannelModel::TimingOutOfRange | ChannelModel::WideSpread
            )
            && self
                .extended_gate
                .as_ref()
                .is_none_or(|gate| gate.load(Ordering::Relaxed))
            && (!operator_controls(self.scenario)
                || self.legs[CALLER].as_ref().is_some_and(|leg| {
                    leg.physical_controls >= 5
                        && leg
                            .last_physical_control_interval
                            .is_some_and(|at| leg.next_tx > at + ADVANCED_SPEECH_INTERVALS)
                }))
            && (self.channel_model != ChannelModel::DelayedEcho
                || self.legs[CALLER]
                    .as_ref()
                    .is_some_and(|leg| leg.timing_advance_units == 9))
            && self.legs[CALLEE].as_ref().is_some_and(|leg| {
                leg.timing_advance_units == PROPAGATION_HALF_SYMBOLS[CALLEE] + DRIFT_HALF_SYMBOLS
            })
            && self.legs.iter().all(|leg| {
                leg.as_ref().is_some_and(|leg| {
                    leg.speech.len() >= REQUIRED_SPEECH_FRAMES
                        && leg.flash.done(leg.next_tx)
                        && leg.service.done(leg.next_tx)
                        && leg
                            .dtmf
                            .as_ref()
                            .is_none_or(|probe| probe.done(leg.next_tx))
                        && (sms.is_none() || (leg.sms_accepted && !leg.sms_received.is_empty()))
                        && leg
                            .report_at
                            .is_some_and(|at| leg.next_tx > at + REPORT_OBSERVATION_INTERVALS)
                })
            })
        {
            let leg = self.legs[hangup].as_mut().ok_or("missing callee")?;
            leg.mobile_release_command =
                Some(leg.tx.enqueue(&release(self.release_reason)).map_err(hal)?);
            self.release_sent = true;
        }
        for (index, leg) in self.legs.iter_mut().enumerate() {
            let Some(leg) = leg.as_mut() else { continue };
            while leg.next_tx * CONTROL_SLOT_PERIOD + leg.phase
                < state.slot_count as u64 + TX_HORIZON_SLOTS
            {
                if leg.stop_at.is_some_and(|stop| leg.next_tx >= stop) {
                    break;
                }
                if matches!(self.scenario, Scenario::Connected { .. })
                    && leg.answer_at.is_some_and(|answer| leg.next_tx >= answer)
                    && !leg.connect_sent
                {
                    leg.tx.enqueue(&TrafficMessage::Connect).map_err(hal)?;
                    leg.connect_sent = true;
                }
                if matches!(self.scenario, Scenario::Connected { sms: Some(_), .. })
                    && self.connect_ack_received
                    && leg.report_sent
                    && !leg.sms_sent
                {
                    let message = mobile_sms(self.scenario, index);
                    if index == CALLEE
                        && matches!(
                            self.scenario,
                            Scenario::Connected {
                                sms: Some(SmsSize::Maximum),
                                ..
                            }
                        )
                    {
                        leg.sacch_tx
                            .enqueue(&encode_signaling(&message).map_err(hal)?)
                            .map_err(hal)?;
                    } else {
                        leg.tx.enqueue(&message).map_err(hal)?;
                    }
                    leg.sms_sent = true;
                }
                if leg.report_at.is_some_and(|at| leg.next_tx >= at) && !leg.report_sent {
                    let message = TrafficMessage::ChannelQuality(ChannelQualityReport::First {
                        current: DownlinkQuality {
                            ber_code: index as u8 + 1,
                            rssi_code: 17 + index as u8,
                        },
                        neighbor_rssi_codes: [0; 6],
                    });
                    leg.sacch_tx
                        .enqueue(&encode_signaling(&message).map_err(hal)?)
                        .map_err(hal)?;
                    leg.report_sent = true;
                }
                if leg.physical_controls >= 2
                    && leg.status_acknowledged
                    && leg.capability_responses > 0
                {
                    leg.flash.drive(&mut leg.tx)?;
                    leg.service.drive(leg.next_tx, &mut leg.tx)?;
                }
                if leg.physical_controls >= 2
                    && leg.status_acknowledged
                    && leg.capability_responses > 0
                    && let Some(probe) = &mut leg.dtmf
                {
                    probe.drive(index, leg.next_tx, &mut leg.tx)?;
                }
                leg.tx
                    .offer_media(
                        leg.next_tx,
                        TransmitFrame::speech(&parameters(index, leg.next_tx)),
                    )
                    .map_err(hal)?;
                let prepared = leg.tx.prepare(leg.next_tx).map_err(hal)?.clone();
                let sacch = leg.sacch_tx.prepare(leg.next_tx).map_err(hal)?.clone();
                let bits = reverse_bits(
                    SyncWord::ALL[leg.phase as usize],
                    prepared.data(),
                    sacch.field(),
                );
                if index == CALLEE && leg.connected && leg.report_sent && !leg.drift_applied {
                    leg.propagation_half_symbols =
                        if self.channel_model == ChannelModel::TimingOutOfRange {
                            OUT_OF_RANGE_HALF_SYMBOLS
                        } else {
                            leg.propagation_half_symbols + DRIFT_HALF_SYMBOLS
                        };
                    leg.drift_applied = true;
                }
                let packet = if leg.shortened {
                    leg.shortened_sent += 1;
                    let mut short =
                        crate::phy::shortened::encode(SyncWord::ALL[leg.phase as usize], DVCC)
                            .ok_or("invalid shortened DVCC")?;
                    if index == CALLER {
                        short[12..40].fill(false);
                    }
                    reverse_waveform_active(&short, crate::phy::shortened::ACTIVE_END_SYMBOL)
                } else {
                    reverse_waveform(&bits)
                };
                let advance_samples = (isize::from(leg.timing_advance_units)
                    - isize::from(leg.propagation_half_symbols))
                    * SAMPLES_PER_SYMBOL as isize
                    / 2;
                if self.connect_ack_received
                    && leg.physical_controls >= 2
                    && (self.channel_model == ChannelModel::WideSpread
                        || (self.channel_model == ChannelModel::DelayedEcho && index == CALLER))
                {
                    let echo = packet.iter().map(|sample| *sample * 0.3).collect();
                    state.traffic_packets.push((
                        leg.next_tx * CONTROL_SLOT_PERIOD + leg.phase,
                        advance_samples - 5 * SAMPLES_PER_SYMBOL as isize,
                        echo,
                    ));
                }
                state.traffic_packets.push((
                    leg.next_tx * CONTROL_SLOT_PERIOD + leg.phase,
                    advance_samples,
                    packet,
                ));
                let completed = leg.tx.submitted(&prepared).map_err(hal)?;
                if self.release_reason == ReleaseReason::PowerDown
                    && completed.is_some()
                    && completed == leg.mobile_release_command
                {
                    leg.powered_down = true;
                    leg.released = true;
                    leg.stop_at = Some(leg.next_tx + 1);
                }
                leg.flash.submitted(&completed, leg.next_tx);
                leg.service.submitted(&completed, leg.next_tx);
                if let Some(probe) = &mut leg.dtmf {
                    probe.submitted(&completed, leg.next_tx);
                }
                if completed.is_some() && completed == leg.spontaneous_status_command {
                    leg.spontaneous_status_interval = Some(leg.next_tx);
                    leg.spontaneous_status_command = None;
                }
                if completed.is_some() && completed == leg.capability_request_command {
                    leg.capability_request_interval = Some(leg.next_tx);
                    leg.capability_request_command = None;
                }
                leg.sacch_tx.submitted(&sacch).map_err(hal)?;
                leg.next_tx += 1;
            }
        }
        if !self.second_origin
            && self.legs.iter().all(|leg| {
                leg.as_ref()
                    .is_some_and(|leg| leg.unused_after_release >= 2)
            })
        {
            respond(state, MobileIdentity::Min34(1), &origination());
            self.second_origin = true;
        }
        Ok(())
    }
}
fn duplicate(first: f64, previous: Option<f64>) -> bool {
    previous.is_some_and(|last| first < last + (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL / 2) as f64)
}
fn release(reason: ReleaseReason) -> TrafficMessage {
    TrafficMessage::Release {
        information: ReleaseInformation {
            fields: ReleaseFields::Reason(reason),
            ..Default::default()
        },
    }
}
fn respond(state: &mut Backend, identity: MobileIdentity, message: &[u8]) -> u64 {
    let first = (state.slot_count as u64).div_ceil(CONTROL_SLOT_PERIOD) + RESPONSE_LEAD_BLOCKS;
    let packets = access_packets(identity, message);
    let final_block = first + packets.iter().map(|(offset, _)| *offset).max().unwrap();
    state.packets.extend(
        packets
            .into_iter()
            .map(|(offset, packet)| (first + offset, packet)),
    );
    final_block
}
// IS-136.2-A Figure 1.2-2 places reverse data around sync, SACCH and DVCC.
fn reverse_bits(
    sync: SyncWord,
    data: &[bool; DATA_BITS],
    sacch: &[bool; crate::phy::traffic::SACCH_BITS],
) -> [bool; SLOT_BITS] {
    let mut bits = [false; SLOT_BITS];
    bits[LEAD_DATA_START..SYNC_START].copy_from_slice(&data[..DATA_LEAD_BITS]);
    bits[SYNC_START..MIDDLE_DATA_START].copy_from_slice(&sync.bits());
    bits[MIDDLE_DATA_START..SACCH_START].copy_from_slice(&data[DATA_LEAD_BITS..DATA_BOUNDARY]);
    bits[SACCH_START..COLOR_START].copy_from_slice(sacch);
    let color = encode_dvcc(DVCC);
    for (index, bit) in bits[COLOR_START..FINAL_DATA_START].iter_mut().enumerate() {
        *bit = color & (1 << (COLOR_BITS - 1 - index)) != 0;
    }
    bits[FINAL_DATA_START..].copy_from_slice(&data[DATA_BOUNDARY..]);
    bits
}
fn origination() -> Vec<u8> {
    crate::l3::access_call::OriginationRequest {
        message: Origination {
            protocol_version: ProtocolVersion::Pv3,
            emergency_call: false,
            called_party: AddressInfo {
                number_type: NUMBER_TYPE,
                numbering_plan: NUMBERING_PLAN,
                address: AddressValue::Tbcd("1002".into()),
            },
            last_try: false,
            scm: STATION_CLASS_MARK,
            service_code: ServiceCode::DigitalSpeech,
        },
        options: crate::l3::access_call::CallOptions {
            voices: vec![
                crate::l3::access_call::LegacyVoice {
                    coder: 3,
                    privacy: 0,
                },
                crate::l3::access_call::LegacyVoice {
                    coder: 1,
                    privacy: 0,
                },
            ],
            encryption: Some(crate::l3::registration::EncryptionMode {
                algorithm: 0,
                domain: 1,
                key: 1,
            }),
            bandwidth: Some(crate::l3::access_call::Bandwidth::DoublePreferred),
            calling_party: Some(AddressInfo {
                number_type: NUMBER_TYPE,
                numbering_plan: NUMBERING_PLAN,
                address: AddressValue::Tbcd("9999".into()),
            }),
            ..Default::default()
        },
    }
    .encode()
    .unwrap()
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registered_handsets_call_exchange_changing_speech_and_callee_release() {
    run_call(Scenario::Connected {
        hangup: CALLEE,
        sms: None,
        tones: false,
        information: false,
        services: false,
    });
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registered_handsets_call_exchange_changing_speech_and_caller_release() {
    run_call(Scenario::Connected {
        hangup: CALLER,
        sms: None,
        tones: false,
        information: false,
        services: false,
    });
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn unanswered_call_expires_and_releases_both_traffic_pairs() {
    run_call(Scenario::Unanswered);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn established_call_exchanges_sms_on_both_legs_without_stopping_voice() {
    run_call(Scenario::Connected {
        hangup: CALLEE,
        sms: Some(SmsSize::Short),
        tones: false,
        information: false,
        services: false,
    });
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn established_call_exchanges_maximum_single_relay_sms_without_control_channel_conversion() {
    run_call(Scenario::Connected {
        hangup: CALLEE,
        sms: Some(SmsSize::Maximum),
        tones: false,
        information: false,
        services: false,
    });
}
fn capability_report(index: usize) -> crate::l3::legacy_traffic::MobileCapabilities {
    use crate::l3::legacy_traffic::{CharacterSupport, MobileCapabilities, MobilePv1, MobilePv5};
    let mut common = super::super::super::capabilities::station_response().common;
    common.protocol_version = ProtocolVersion::Pv7;
    common.voice_coder_map = 0x1f;
    MobileCapabilities {
        common,
        supported_bands: 1,
        analog_800_speech: true,
        pv1: Some(MobilePv1 {
            scm: 21,
            software_vintage: 18,
            firmware_vintage: 17,
            model: 0x33 + index as u8,
            manufacturer: 0x22,
            max_supported_pfc: 5,
            characters: CharacterSupport {
                latin1: true,
                ..Default::default()
            },
            ..Default::default()
        }),
        advice: Some(false),
        pv5: Some(MobilePv5 {
            modulation_map: 1,
            interleaving_map: 1,
            ..Default::default()
        }),
        extended_characters: Some([false; 13]),
    }
}
fn status_report(index: usize) -> StatusFields {
    StatusFields {
        serial_numbers: Some(vec![0x10203040 + index as u32]),
        call_modes: Some(vec![3]),
        terminals: Some(vec![TerminalInformation {
            protocol_version: ProtocolVersion::Is136RevA,
            manufacturer: 0x22,
            model: 0x33 + index as u8,
            firmware: 17,
            access_overload_class: 2,
            local_control: false,
            registration: true,
        }]),
        ..Default::default()
    }
}
fn handset_queue_words(scenario: Scenario) -> usize {
    if matches!(
        scenario,
        Scenario::Connected {
            sms: Some(SmsSize::Maximum),
            ..
        }
    ) {
        crate::l3::legacy_traffic::MAX_MESSAGE_BITS.div_ceil(crate::link::traffic::WORD_BITS) + 1
    } else {
        FACCH_QUEUE_WORDS
    }
}
fn sms_text(scenario: Scenario, index: usize) -> Vec<u8> {
    if matches!(
        scenario,
        Scenario::Connected {
            sms: Some(SmsSize::International),
            ..
        }
    ) {
        return ["café depuis le téléphone", "短信 Ελληνικά"][index]
            .as_bytes()
            .to_vec();
    }
    let mut text = SMS_TEXT[index].to_vec();
    if matches!(
        scenario,
        Scenario::Connected {
            sms: Some(SmsSize::Maximum),
            ..
        }
    ) {
        text.resize(
            crate::l3::sms::MAX_SINGLE_RELAY_IRA_CHARACTERS,
            b'a' + index as u8,
        );
    }
    text
}
fn mobile_sms(scenario: Scenario, index: usize) -> TrafficMessage {
    let relay = AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: SMS_TRANSACTION,
            protocol: HigherLayerProtocol::Sms,
            payload: SmsMessage::Submit {
                metadata: Metadata {
                    message_reference: index as u16 + 1,
                    privacy: 0,
                    urgency: 0,
                    delivery_acknowledgment: false,
                    manual_acknowledgment: false,
                    new_message: true,
                },
                user_data: UserData::from_text(
                    std::str::from_utf8(&sms_text(scenario, index)).unwrap(),
                )
                .unwrap(),
            }
            .encode()
            .unwrap(),
        },
        addresses: RelayAddresses {
            destination: Some(AddressInfo {
                number_type: NUMBER_TYPE,
                numbering_plan: NUMBERING_PLAN,
                address: AddressValue::Tbcd(if index == CALLER { "1002" } else { "1001" }.into()),
            }),
            ..Default::default()
        },
    };
    TrafficMessage::Relay(TrafficRelay::from_addressed(&relay).unwrap())
}
fn operator_controls(scenario: Scenario) -> bool {
    matches!(
        scenario,
        Scenario::Connected {
            hangup: CALLER,
            sms: None,
            ..
        }
    )
}
fn run_call(scenario: Scenario) {
    run_call_with_channel(scenario, ChannelModel::SinglePath);
}
fn run_call_with_channel(scenario: Scenario, channel_model: ChannelModel) {
    run_call_with_arq(scenario, channel_model, false);
}
fn run_call_with_arq(scenario: Scenario, channel_model: ChannelModel, mixed_arq: bool) {
    run_call_with_feedback(scenario, channel_model, mixed_arq, AlertFeedback::Normal);
}
fn run_call_with_feedback(
    scenario: Scenario,
    channel_model: ChannelModel,
    mixed_arq: bool,
    alert_feedback: AlertFeedback,
) {
    run_call_with_page_reply(
        scenario,
        channel_model,
        mixed_arq,
        alert_feedback,
        paging::ReplyOn::FirstComplete,
    );
}
fn run_call_with_page_reply(
    scenario: Scenario,
    channel_model: ChannelModel,
    mixed_arq: bool,
    alert_feedback: AlertFeedback,
    page_reply: paging::ReplyOn,
) {
    run_call_with_termination(
        scenario,
        channel_model,
        mixed_arq,
        alert_feedback,
        page_reply,
        ReleaseReason::Normal,
    );
}
fn run_call_with_termination(
    scenario: Scenario,
    channel_model: ChannelModel,
    mixed_arq: bool,
    alert_feedback: AlertFeedback,
    page_reply: paging::ReplyOn,
    release_reason: ReleaseReason,
) {
    run_call_with_extended(
        scenario,
        channel_model,
        mixed_arq,
        alert_feedback,
        page_reply,
        release_reason,
        TestFeatures::default(),
    );
}
#[derive(Default)]
struct TestFeatures {
    extended: bool,
    neighbors: bool,
}
fn run_call_with_extended(
    scenario: Scenario,
    channel_model: ChannelModel,
    mixed_arq: bool,
    alert_feedback: AlertFeedback,
    page_reply: paging::ReplyOn,
    release_reason: ReleaseReason,
    features: TestFeatures,
) {
    let extended = features.extended;
    let timing_fault = channel_model == ChannelModel::TimingOutOfRange;
    let _paced = PACED_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let sms = matches!(scenario, Scenario::Connected { sms: Some(_), .. });
    let maximum_sms = matches!(
        scenario,
        Scenario::Connected {
            sms: Some(SmsSize::Maximum),
            ..
        }
    );
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = Database(std::env::temp_dir().join(format!(
        "136bts-serving-voice-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = db.0.clone();
    if features.neighbors {
        config.cell.neighbors = (2..=25)
            .map(crate::l3::neighbors::tests::neighbor)
            .collect();
    }
    let startup_blocks = if mixed_arq {
        arq_reuse_policy(&config).unwrap().quiet_blocks()
    } else {
        0
    };
    if mixed_arq {
        config.subscribers.push(crate::config::SubscriberConfig {
            identity: crate::config::IdentityConfig::Min34 { value: 3 },
            number: "3".repeat(32),
        });
    }
    if matches!(scenario, Scenario::Unanswered) {
        config.calls.answer_timeout_seconds = UNANSWERED_TIMEOUT_SECONDS;
    }
    let registration = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: STATION_CLASS_MARK,
        protocol_version: ProtocolVersion::Pv3,
    }
    .encode()
    .unwrap();
    let mut packets = vec![(ACCESS_BLOCK, packet_version(ProtocolVersion::Pv3))];
    packets.extend(
        access_packets(MobileIdentity::Min34(2), &registration)
            .into_iter()
            .map(|(offset, packet)| (SECOND_REGISTRATION_BLOCK + offset, packet)),
    );
    packets.extend(
        access_packets(MobileIdentity::Min34(1), &origination())
            .into_iter()
            .map(|(offset, packet)| (startup_blocks + FIRST_ORIGINATION_BLOCK + offset, packet)),
    );
    if mixed_arq {
        let request = crate::l3::registration::RegistrationRequest {
            registration_type: RegistrationType::PowerUp,
            scm: STATION_CLASS_MARK,
            protocol_version: ProtocolVersion::Pv3,
            options: crate::l3::registration::RegistrationOptions {
                number_request: Some(1),
                ..Default::default()
            },
        }
        .encode()
        .unwrap();
        packets.extend(
            access_packets(MobileIdentity::Min34(3), &request)
                .into_iter()
                .map(|(offset, packet)| (startup_blocks + ACCESS_BLOCK + offset, packet)),
        );
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut broadcast = crate::scheduler::broadcast::BroadcastContent::try_new(
        AssignedSlots::Full14,
        messages(&config),
        optional_messages().unwrap(),
    )
    .unwrap()
    .with_time_zone(crate::l3::time_date::TimeZoneOffset::try_new(0, false).unwrap())
    .unwrap();
    if features.neighbors {
        broadcast = broadcast.with_neighbors(&config.cell.neighbors).unwrap();
    }
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) =
        &broadcast.messages()[0]
    else {
        panic!()
    };
    let allocation = crate::scheduler::paging::PagingAllocation::try_new(structure).unwrap();
    let return_channel = ControlChannel {
        hyperband: match config.cell.band {
            crate::config::ConfigBand::Cellular800 => Hyperband::Cellular800,
            crate::config::ConfigBand::Pcs1900 => Hyperband::Pcs1900,
        },
        channel: config.cell.channel,
        dvcc: config.cell.dvcc,
    };
    let mut handsets = Handsets::new(
        scenario,
        channel_model,
        mixed_arq,
        allocation,
        return_channel,
    );
    let extended_gate = Arc::new(AtomicBool::new(false));
    handsets.extended_gate = extended.then(|| extended_gate.clone());
    handsets.release_reason = release_reason;
    handsets.alert_feedback = alert_feedback;
    handsets.page_reply = page_reply;
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        clock_sample: 0,
        tx_observation_offset_ticks: 0,
        epoch: None,
        tx: Vec::new(),
        slot_count: 0,
        stop_slots: startup_blocks as usize * CONTROL_SLOT_PERIOD as usize
            + if maximum_sms {
                MAXIMUM_SMS_TEST_STOP_SLOTS
            } else if sms {
                SMS_TEST_STOP_SLOTS
            } else {
                TEST_STOP_SLOTS
            },
        underflow_after_slot: None,
        stall_after_slot: None,
        realtime_epoch: Some(Instant::now()),
        handset_segmented: None,
        handset_sms: None,
        handset_voice: Some(handsets),
        handset_arq: None,
        handset_idle: None,
        traffic_packets: Vec::new(),
        packets,
        tx_active: false,
        rx_active: false,
        shutdown: shutdown.clone(),
    }));
    let prepared = PreparedRadio::from_radio(&config, Box::new(Device(state.clone()))).unwrap();
    let (hub, interface) = crate::operations::Hub::new(&config);
    let extended_operator =
        extended.then(|| extended::operator(hub.clone(), shutdown.clone(), extended_gate.clone()));
    let uplink_ber_seen = std::sync::Arc::new(AtomicBool::new(false));
    let operator = operator_controls(scenario).then(|| {
        let hub = hub.clone();
        let shutdown = shutdown.clone();
        let uplink_ber_seen = uplink_ber_seen.clone();
        std::thread::spawn(move || {
            let mut manual_sent = false;
            while !shutdown.load(Ordering::Relaxed) {
                let snapshot = hub.snapshot();
                if snapshot
                    .calls
                    .iter()
                    .flat_map(|call| &call.legs)
                    .flat_map(|leg| &leg.measurements)
                    .any(|metric| metric.name == "Uplink BER" && metric.value.is_some())
                {
                    uplink_ber_seen.store(true, Ordering::Relaxed);
                }
                let ready = snapshot.calls.iter().any(|call| {
                    call.id == FIRST_CALL_ID
                        && call.phase() == crate::operations::proto::CallPhase::Connected
                        && call.legs.iter().all(|leg| {
                            leg.measurements.iter().any(|metric| {
                                metric.name == "Confirmed mobile power level"
                                    && metric.value.as_deref() == Some("7")
                            })
                        })
                });
                let manually_aligned = snapshot
                    .calls
                    .iter()
                    .filter(|call| call.id == FIRST_CALL_ID)
                    .flat_map(|call| &call.legs)
                    .any(|leg| {
                        leg.party == "Caller"
                            && leg.measurements.iter().any(|metric| {
                                metric.name == "Confirmed timing advance"
                                    && metric.value.as_deref() == Some("30")
                            })
                            && leg.measurements.iter().any(|metric| {
                                metric.name == "Timing control"
                                    && metric.value.as_deref() == Some("Manual")
                            })
                    });
                let request = if !manual_sent && ready {
                    Some(crate::operations::proto::SetPhysicalControlRequest {
                        call_id: FIRST_CALL_ID,
                        party: crate::operations::proto::CallParty::Caller as i32,
                        mobile_power_level: Some(OPERATOR_POWER_LEVEL),
                        timing_advance_half_symbols: Some(OPERATOR_ADVANCE_UNITS),
                        automatic_timing: None,
                    })
                } else if manual_sent && manually_aligned {
                    Some(crate::operations::proto::SetPhysicalControlRequest {
                        call_id: FIRST_CALL_ID,
                        party: crate::operations::proto::CallParty::Caller as i32,
                        automatic_timing: Some(true),
                        ..Default::default()
                    })
                } else {
                    None
                };
                if let Some(request) = request {
                    let (response, result) = tokio::sync::oneshot::channel();
                    hub.commands
                        .send(crate::operations::PendingCommand {
                            command: crate::operations::Command::PhysicalControl(request),
                            response,
                        })
                        .unwrap();
                    result.blocking_recv().unwrap().unwrap();
                    if manual_sent {
                        return;
                    }
                    manual_sent = true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("cell stopped before manual timing and automatic restoration completed");
        })
    });
    let information_operator = matches!(
        scenario,
        Scenario::Connected {
            information: true,
            ..
        }
    )
    .then(|| {
        let hub = hub.clone();
        let shutdown = shutdown.clone();
        std::thread::spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                let ready = hub.snapshot().calls.iter().any(|call| {
                    call.id == FIRST_CALL_ID
                        && call.phase() == crate::operations::proto::CallPhase::Connected
                });
                if ready {
                    for party in [
                        crate::operations::proto::CallParty::Caller,
                        crate::operations::proto::CallParty::Callee,
                    ] {
                        let callee = party == crate::operations::proto::CallParty::Callee;
                        let request = crate::operations::proto::SendCallInformationRequest {
                            call_id: FIRST_CALL_ID,
                            party: party as i32,
                            calling_name: Some(
                                if callee { "Line 1001" } else { "Line 1002" }.into(),
                            ),
                            voice_messages: Some(if callee { 63 } else { 0 }),
                            signal_code: Some(0),
                            other_messages: vec![crate::operations::proto::WaitingMessage {
                                kind: 1,
                                count: if callee { 2 } else { 0 },
                            }],
                            ..Default::default()
                        };
                        let (response, result) = tokio::sync::oneshot::channel();
                        hub.commands
                            .send(crate::operations::PendingCommand {
                                command: crate::operations::Command::CallInformation(request),
                                response,
                            })
                            .unwrap();
                        result.blocking_recv().unwrap().unwrap();
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("cell stopped before call-information commands completed");
        })
    });
    let result = run_prepared_with_interface(
        &config,
        prepared,
        &shutdown,
        Some(
            Duration::from_secs(if maximum_sms {
                MAXIMUM_SMS_TEST_DURATION_SECONDS
            } else if sms {
                SMS_TEST_DURATION_SECONDS
            } else {
                TEST_DURATION_SECONDS
            }) + Duration::from_millis(startup_blocks * TDMA_BLOCK_MS),
        ),
        Some(interface),
    );
    shutdown.store(true, Ordering::Relaxed);
    if let Some(operator) = information_operator {
        operator.join().unwrap();
    }
    if let Some(operator) = operator {
        operator.join().unwrap();
        assert!(uplink_ber_seen.load(Ordering::Relaxed));
    }
    if let Some(operator) = extended_operator {
        operator.join().unwrap();
        assert!(extended_gate.load(Ordering::Relaxed));
    }
    let summary = result.unwrap();
    assert_eq!(summary.completed_registrations, 2);
    if matches!(scenario, Scenario::Connected { sms: None, .. }) {
        let snapshot = hub.snapshot();
        assert!(snapshot.subscribers.iter().all(|subscriber| {
            subscriber
                .reverse_quality
                .and_then(|quality| quality.bit_error_percent)
                .is_some()
        }));
        assert!(
            snapshot
                .air_messages
                .iter()
                .any(|message| message.direction == "Reverse"
                    && message.name == "Origination"
                    && message.decoded.contains("VoiceModes(VC/PM)=3/0,1/0")
                    && message.decoded.contains("Bandwidth=DoublePreferred"))
        );
        assert!(snapshot.air_messages.iter().any(|message| {
            message.direction == "Reverse"
                && message.name == "Page response"
                && message
                    .decoded
                    .contains("ExtendedVoice(PM:VC/MOD/INT digits)=0:300,101")
                && message.decoded.contains("Bandwidth=HalfPreferred")
        }));
    }

    if matches!(scenario, Scenario::Connected { .. }) {
        let snapshot = hub.snapshot();
        let call = snapshot
            .recent_calls
            .iter()
            .find(|call| call.id == FIRST_CALL_ID)
            .expect("completed voice call retained");
        if let Scenario::Connected { hangup, .. } = scenario
            && channel_model == ChannelModel::SinglePath
        {
            let party = if hangup == CALLER { "Caller" } else { "Callee" };
            let leg = call.legs.iter().find(|leg| leg.party == party).unwrap();
            assert!(
                leg.measurements
                    .iter()
                    .any(|metric| metric.name == "Mobile release reason"
                        && metric.value.as_deref() == Some(format!("{release_reason:?}").as_str()))
            );
        }
        if channel_model == ChannelModel::WideSpread {
            assert_eq!(call.phase(), crate::operations::proto::CallPhase::Ended);
            assert_eq!(call.end_reason, "Bursts from the two phones overlapped");
        }
        if channel_model == ChannelModel::DelayedEcho {
            let caller = call.legs.iter().find(|leg| leg.party == "Caller").unwrap();
            for (name, expected) in [
                ("Significant postcursor", "5"),
                ("Timing target arrival", "-2.50"),
                ("Adjacent-slot overlap", "0.00"),
            ] {
                assert!(
                    caller
                        .measurements
                        .iter()
                        .any(|metric| metric.name == name
                            && metric.value.as_deref() == Some(expected)),
                    "{name}: {:?}",
                    caller.measurements
                );
            }
            assert!(
                call.legs
                    .iter()
                    .any(|leg| leg.measurements.iter().any(|metric| metric.name
                        == "Overlap detections"
                        && metric
                            .value
                            .as_deref()
                            .is_some_and(|value| value.parse::<u64>().unwrap() > 0))),
                "channel measurements: {:?}",
                call.legs
                    .iter()
                    .map(|leg| (
                        &leg.party,
                        leg.measurements
                            .iter()
                            .filter(|metric| metric.name.contains("cursor")
                                || metric.name.contains("Channel")
                                || metric.name.contains("Overlap")
                                || metric.name.contains("timing"))
                            .collect::<Vec<_>>()
                    ))
                    .collect::<Vec<_>>()
            );
        }
        if timing_fault {
            assert_eq!(call.phase(), crate::operations::proto::CallPhase::Ended);
            assert_eq!(
                call.end_reason,
                "The called phone moved out of timing range"
            );
            let callee = call.legs.iter().find(|leg| leg.party == "Callee").unwrap();
            assert!(
                callee
                    .measurements
                    .iter()
                    .any(|metric| metric.name == "Timing range"
                        && metric.value.as_deref() == Some("Limit reached"))
            );
        }
        for (party, expected) in [("Caller", "-79"), ("Callee", "-77")] {
            let leg = call.legs.iter().find(|leg| leg.party == party).unwrap();
            let expected_esn = if party == "Caller" {
                "10203040"
            } else {
                "10203041"
            };
            assert!(
                leg.measurements
                    .iter()
                    .any(|metric| metric.name == "Phone ESN"
                        && metric.value.as_deref() == Some(expected_esn)),
                "{party}: {:?}",
                leg.measurements
            );
            assert!(
                leg.measurements
                    .iter()
                    .any(|metric| metric.name == "Status query"
                        && metric.value.as_deref() == Some("Acknowledged"))
            );

            if matches!(scenario, Scenario::Connected { services: true, .. }) {
                for (name, expected) in [
                    ("Service response", "Acknowledged"),
                    ("Handset service requests", "4"),
                ] {
                    assert!(
                        leg.measurements.iter().any(|metric| metric.name == name
                            && metric.value.as_deref() == Some(expected)),
                        "{party}: {name}, {:?}",
                        leg.measurements
                    );
                }
            }
            for (name, expected) in [
                ("Timing control", "Automatic"),
                ("Initial alignment", "Complete"),
                ("Physical control", "Acknowledged"),
                ("Capability query", "Acknowledged"),
                ("Phone capability protocol", "Pv7"),
                ("Common capability protocol", "Pv2"),
                ("Phone voice coder map", "0x1F"),
                ("Phone maximum supported PFC", "6"),
            ] {
                assert!(
                    leg.measurements
                        .iter()
                        .any(|metric| metric.name == name
                            && metric.value.as_deref() == Some(expected)),
                    "{party}: {name}, {:?}",
                    leg.measurements
                );
            }
            let advanced = operator_controls(scenario) && party == "Caller";
            for (name, expected) in [
                (
                    "Confirmed mobile power level",
                    if advanced { "6" } else { "7" },
                ),
                (
                    "Confirmed timing advance",
                    if party == "Caller" {
                        if matches!(
                            channel_model,
                            ChannelModel::DelayedEcho | ChannelModel::WideSpread
                        ) {
                            "9"
                        } else {
                            "4"
                        }
                    } else if channel_model == ChannelModel::WideSpread {
                        "12"
                    } else if timing_fault {
                        "30"
                    } else {
                        "7"
                    },
                ),
            ] {
                assert!(
                    leg.measurements
                        .iter()
                        .any(|metric| metric.name == name
                            && metric.value.as_deref() == Some(expected)),
                    "{party}: {name}, {:?}",
                    leg.measurements
                );
            }
            assert!(
                leg.measurements
                    .iter()
                    .any(|metric| metric.name == "Phone reported downlink RSSI"
                        && metric.value.as_deref() == Some(expected)),
                "{party}: metrics={:?}",
                leg.measurements
            );
        }
        if matches!(
            scenario,
            Scenario::Connected {
                information: true,
                ..
            }
        ) {
            for leg in &call.legs {
                for (name, expected) in [
                    ("Call information", "Acknowledged"),
                    ("Handset feature requests", "2"),
                    ("Supplementary service execution", "Not configured"),
                ] {
                    assert!(
                        leg.measurements.iter().any(|metric| metric.name == name
                            && metric.value.as_deref() == Some(expected)),
                        "{name}: {:?}",
                        leg.measurements
                    );
                }
            }
            for direction in ["Forward", "Reverse"] {
                for name in [
                    "Flash with information",
                    "Flash with information acknowledgment",
                ] {
                    assert!(
                        snapshot
                            .air_messages
                            .iter()
                            .any(|message| message.direction == direction && message.name == name),
                        "missing {direction} {name}"
                    );
                }
            }
        }
        if matches!(scenario, Scenario::Connected { tones: true, .. }) {
            for identity in ["Min34(1)", "Min34(2)"] {
                for (direction, name) in [
                    ("Reverse", "Send burst DTMF"),
                    ("Forward", "Send burst DTMF acknowledgment"),
                    ("Reverse", "Send continuous DTMF"),
                    ("Forward", "Send continuous DTMF acknowledgment"),
                ] {
                    assert!(
                        snapshot
                            .air_messages
                            .iter()
                            .any(|message| message.identity == identity
                                && message.channel == "FACCH"
                                && message.direction == direction
                                && message.name == name),
                        "missing {identity} {direction} {name}"
                    );
                }
            }
        }
        for direction in ["Forward", "Reverse"] {
            for name in ["Capability update request", "Capability update response"] {
                assert!(
                    snapshot
                        .air_messages
                        .iter()
                        .any(|message| message.channel == "FACCH"
                            && message.direction == direction
                            && message.name == name),
                    "missing {direction} {name}"
                );
            }
        }
        assert!(
            snapshot
                .air_messages
                .iter()
                .any(|message| message.channel == "SACCH"
                    && message.name == "Channel quality report")
        );
    }
    if sms {
        let store = StateStore::open(&db.0).unwrap();
        for identity in [MobileIdentity::Min34(1), MobileIdentity::Min34(2)] {
            let pending = store.pending_for(identity).unwrap();
            assert!(
                pending.is_empty(),
                "{identity:?}: pending={pending:?}, messages={:?}, carrier={:?}",
                store.operation_messages(10).unwrap(),
                hub.snapshot().radio
            );
        }
        assert_eq!(store.operation_messages(10).unwrap().len(), 2);
        let snapshot = hub.snapshot();
        for direction in ["Forward", "Reverse"] {
            assert!(
                snapshot
                    .air_messages
                    .iter()
                    .any(|message| message.direction == direction && message.name == "R-DATA")
            );
        }
    }
    assert!(summary.stop_acknowledged);
    let state = state.lock().unwrap();
    assert!(!state.tx_active);
    assert!(!state.rx_active);
    let handsets = state.handset_voice.as_ref().unwrap();
    assert!(handsets.page_control.primary_pages >= 1);
    assert!(handsets.page_control.secondary_pages >= 1);
    if page_reply == paging::ReplyOn::Secondary {
        assert!(!handsets.replied_page_superframe.unwrap().is_multiple_of(2));
    }
    let (feedback_block, assignment_block) = handsets.page_access.assignment().unwrap();
    assert!(assignment_block >= feedback_block);
    if mixed_arq {
        let first_begin = handsets.third_begin_block.unwrap();
        assert!(
            handsets
                .assignment_blocks
                .iter()
                .all(|block| block.unwrap() > first_begin)
        );
        let snapshot = hub.snapshot();
        let expiry = snapshot
            .air_messages
            .iter()
            .find(|message| {
                message.identity == "Min34(3)"
                    && message.status == "Expired without ARQ confirmation"
            })
            .expect("silent ARQ registration expires");
        for identity in ["Min34(1)", "Min34(2)"] {
            assert!(
                snapshot
                    .air_messages
                    .iter()
                    .any(|message| message.direction == "Forward"
                        && message.identity == identity
                        && message.raw.first() == Some(&DESIGNATION_TYPE)
                        && message.sequence < expiry.sequence)
            );
        }
        assert!(!handsets.third_replies.is_empty());
        for message in &handsets.third_replies {
            assert_eq!(message.identity, MobileIdentity::Min34(3));
            let accept = crate::l3::registration::AcceptOptions::decode(&message.message).unwrap();
            assert_eq!(
                accept.directory_address.unwrap().address,
                AddressValue::Tbcd("3".repeat(32))
            );
        }
    }

    assert_eq!(
        handsets.release_sent,
        matches!(scenario, Scenario::Connected { .. })
            && !matches!(
                channel_model,
                ChannelModel::TimingOutOfRange | ChannelModel::WideSpread
            ),
        "slots={}, legs={:?}",
        state.slot_count,
        handsets
            .legs
            .iter()
            .map(|leg| leg.as_ref().map(|leg| (
                leg.next_tx,
                leg.timing_advance_units,
                leg.physical_controls,
                leg.speech.len(),
                leg.dtmf.as_ref().map(|probe| probe.progress()),
                leg.flash.progress()
            )))
            .collect::<Vec<_>>()
    );
    assert!(handsets.second_origin);
    if release_reason == ReleaseReason::PowerDown {
        assert_eq!(handsets.pages, 1);
    } else {
        assert!(handsets.pages >= 2);
    }
    for (destination, leg) in handsets.legs.iter().enumerate() {
        let leg = leg.as_ref().unwrap();
        if matches!(
            scenario,
            Scenario::Connected {
                information: true,
                ..
            }
        ) {
            leg.flash.verify(destination);
        }
        leg.service.verify();
        if let Some(probe) = &leg.dtmf {
            probe.verify(destination);
        }
        assert!(leg.released);
        if leg.powered_down {
            assert!(!leg.return_channel_seen);
            assert_eq!(leg.stop_at, Some(leg.next_tx));
        } else {
            assert!(leg.return_channel_seen);
        }
        if leg.alerted {
            assert!(leg.caller_seen_before_answer);
        }
        assert!(leg.unused_after_release >= 2);
        match scenario {
            Scenario::Connected { .. } => {
                assert!(leg.status_queries >= 1);
                assert!(leg.capability_queries >= 1);
                assert!(leg.physical_controls >= 2);
                assert!(leg.shortened_sent >= 1);
                assert_eq!(leg.capability_responses, 1);
                assert!(leg.status_acknowledged);
                assert!(leg.speech.len() >= REQUIRED_SPEECH_FRAMES);
                if sms {
                    assert!(leg.sms_sent && leg.sms_accepted);
                    assert_eq!(leg.sms_received.len(), 1);
                    assert!(leg.speech.len() > MEASUREMENT_REPORT_INTERVALS as usize);
                }
                assert!(leg.speech.iter().any(|frame| frame != &leg.speech[0]));
            }
            Scenario::Unanswered => {
                assert!(leg.speech.is_empty());
                assert!(!leg.connect_sent);
                assert!(!leg.connected);
            }
        }
    }
    if alert_feedback == AlertFeedback::LoseFirstAck {
        assert!(handsets.legs[CALLEE].as_ref().unwrap().alert_messages >= 2);
    }
    match scenario {
        Scenario::Connected { hangup, .. } => {
            assert!(handsets.legs[CALLEE].as_ref().unwrap().connected);
            assert!(handsets.legs[1 - hangup].as_ref().unwrap().release_received);
        }
        Scenario::Unanswered => {
            assert!(handsets.legs[CALLEE].as_ref().unwrap().alerted);
            assert!(
                handsets
                    .legs
                    .iter()
                    .all(|leg| leg.as_ref().unwrap().release_received)
            );
        }
    }
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn established_call_releases_both_legs_when_timing_cannot_be_corrected() {
    run_call_with_channel(
        Scenario::Connected {
            hangup: CALLEE,
            sms: None,
            tones: false,
            information: false,
            services: false,
        },
        ChannelModel::TimingOutOfRange,
    );
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn connected_handsets_exchange_dtmf_requests_retries_stops_and_decoded_tones() {
    run_call(Scenario::Connected {
        hangup: CALLEE,
        sms: None,
        tones: true,
        information: false,
        services: false,
    });
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn delayed_echo_overlap_is_corrected_without_recentering_the_main_path() {
    run_call_with_channel(
        Scenario::Connected {
            hangup: CALLEE,
            sms: None,
            tones: false,
            information: false,
            services: false,
        },
        ChannelModel::DelayedEcho,
    );
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn uncorrectable_adjacent_phone_spread_releases_after_confirmed_endpoint_attempts() {
    run_call_with_channel(
        Scenario::Connected {
            hangup: CALLEE,
            sms: None,
            tones: false,
            information: false,
            services: false,
        },
        ChannelModel::WideSpread,
    );
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn connected_handsets_exchange_flash_requests_and_receive_operator_call_information() {
    run_call(Scenario::Connected {
        hangup: CALLEE,
        sms: None,
        tones: false,
        information: true,
        services: false,
    });
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn connected_handsets_negotiate_service_rejections_and_unchanged_voice_over_iq() {
    run_call(Scenario::Connected {
        hangup: CALLEE,
        sms: None,
        tones: false,
        information: false,
        services: true,
    });
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn call_assignment_waits_for_callee_scf_and_completes_during_an_unresponsive_arq_registration() {
    run_call_with_arq(
        Scenario::Connected {
            hangup: CALLEE,
            sms: None,
            tones: false,
            information: false,
            services: false,
        },
        ChannelModel::SinglePath,
        true,
    );
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn established_call_exchanges_latin1_and_bmp_sms_without_changing_speech() {
    run_call(Scenario::Connected {
        hangup: CALLER,
        sms: Some(SmsSize::International),
        tones: false,
        information: false,
        services: false,
    });
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn caller_identity_remains_in_alert_retries_after_a_lost_mobile_ack_before_answer() {
    run_call_with_feedback(
        Scenario::Connected {
            hangup: CALLEE,
            sms: None,
            tones: false,
            information: false,
            services: false,
        },
        ChannelModel::SinglePath,
        false,
        AlertFeedback::LoseFirstAck,
    );
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn secondary_fragmented_page_assigns_a_call_while_an_unresponsive_arq_registration_remains_pending()
{
    run_call_with_page_reply(
        Scenario::Connected {
            hangup: CALLEE,
            sms: None,
            tones: false,
            information: false,
            services: false,
        },
        ChannelModel::SinglePath,
        true,
        AlertFeedback::Normal,
        paging::ReplyOn::Secondary,
    );
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn power_down_release_stops_the_phone_and_prevents_another_page() {
    run_call_with_termination(
        Scenario::Connected {
            hangup: CALLEE,
            sms: None,
            tones: false,
            information: false,
            services: false,
        },
        ChannelModel::SinglePath,
        false,
        AlertFeedback::Normal,
        paging::ReplyOn::FirstComplete,
        ReleaseReason::PowerDown,
    );
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn full_neighbor_allocation_preserves_paging_call_speech_and_release_over_iq() {
    run_call_with_extended(
        Scenario::Connected {
            hangup: CALLEE,
            sms: None,
            tones: false,
            information: false,
            services: false,
        },
        ChannelModel::SinglePath,
        false,
        AlertFeedback::Normal,
        paging::ReplyOn::FirstComplete,
        ReleaseReason::Normal,
        TestFeatures {
            extended: false,
            neighbors: true,
        },
    );
}
