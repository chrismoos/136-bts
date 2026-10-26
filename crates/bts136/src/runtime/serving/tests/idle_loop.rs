use super::*;
use crate::{
    l3::capability_report::{CapabilityReport, ReportOptions, SupportedServices},
    operations::{Command, PendingCommand, proto},
    scheduler::paging::PagingAllocation,
};
const OBSERVE_SLOTS: usize = 12;
const WINDOW_SLOTS: usize = 24;
const RESPONSE_LEAD_BLOCKS: u64 = 4;
const SECOND_REGISTRATION_BLOCK: u64 = 12;
const TEST_STOP_SLOTS: usize = 900;
const TEST_DURATION_SECONDS: u64 = 6;
const REPORT_MODEL: u8 = 0x34;
#[derive(Clone, Copy)]
enum ReplyOn {
    Primary,
    Secondary,
}
#[derive(Clone, Copy)]
enum QueryMode {
    Capabilities,
    MessageWaiting,
    UserAlert,
    Audit { force_registration: bool },
}
pub(super) struct Handsets {
    reply_on: ReplyOn,
    combined_audit: bool,
    mode: QueryMode,
    receiver: SpachReceiver,
    waiting: waiting::Observer,
    allocation: PagingAllocation,
    last_sample: Option<f64>,
    primary: [Option<(u64, [bool; crate::phy::control::INFORMATION_BITS])>; 2],
    secondary: [bool; 2],
    responded: [bool; 2],
    registration_seen: Vec<MobileIdentity>,
    response_blocks: [Option<u64>; 2],
}
impl Handsets {
    fn new(reply_on: ReplyOn, allocation: PagingAllocation, mode: QueryMode) -> Self {
        Self {
            reply_on,
            combined_audit: false,
            mode,
            receiver: SpachReceiver::default(),
            waiting: if matches!(mode, QueryMode::UserAlert) {
                waiting::Observer::for_user_alert()
            } else {
                waiting::Observer::default()
            },
            allocation,
            last_sample: None,
            primary: [None; 2],
            secondary: [false; 2],
            responded: [false; 2],
            registration_seen: Vec::new(),
            response_blocks: [None; 2],
        }
    }
    pub fn observe(&mut self, state: &mut Backend) -> Result<(), HalError> {
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
        for burst in decode_forward_control(&samples, RATE as f64, LogicalChannel::Spach, 1)
            .map_err(|error| HalError::from(error.to_string()))?
        {
            let first = offset as f64 + burst.slot_start_sample;
            if self.last_sample.is_some_and(|last| {
                first < last + (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL / 2) as f64
            }) {
                continue;
            }
            self.last_sample = Some(first);
            if matches!(self.mode, QueryMode::MessageWaiting | QueryMode::UserAlert) {
                if let Ok(message) =
                    crate::link::spach::decode_single(&burst.fields.block.information)
                    && message.channel == crate::link::spach::Channel::Arch
                    && crate::l3::registration::RegistrationResponse::decode_prefix(
                        &message.message,
                    )
                    .is_ok_and(|decoded| {
                        matches!(
                            decoded.message,
                            crate::l3::registration::RegistrationResponse::Accept
                        )
                    })
                {
                    self.registration_seen.push(message.identity);
                }
                let observation = self.waiting.observe(
                    &burst.fields.block.information,
                    first,
                    self.allocation,
                )?;
                if let Some(observation) = observation {
                    let index = observation.index;
                    let sf = observation.superframe;
                    if sf.is_multiple_of(2) {
                        self.primary[index] = Some((sf, observation.information));
                    } else {
                        self.secondary[index] = true;
                    }
                    if !self.responded[index]
                        && matches!(
                            (self.reply_on, sf.is_multiple_of(2)),
                            (ReplyOn::Primary, true) | (ReplyOn::Secondary, false)
                        )
                    {
                        let block = (state.slot_count as u64).div_ceil(CONTROL_SLOT_PERIOD)
                            + RESPONSE_LEAD_BLOCKS
                            + index as u64;
                        let ready = block * CONTROL_SLOT_PERIOD * SYMBOLS_PER_SLOT as u64
                            - REVERSE_LEAD_SYMBOLS as u64;
                        let final_symbol = (observation.physical + 1) * SYMBOLS_PER_SLOT as u64;
                        if ready < final_symbol
                            || (matches!(self.mode, QueryMode::MessageWaiting)
                                && ready - final_symbol
                                    > SYMBOL_RATE_HZ as u64
                                        * super::super::super::timers::MOBILE_RESPONSE_WINDOW_MS
                                        / 1000)
                        {
                            return Err(
                                "Message Waiting response readiness exceeded one superframe".into(),
                            );
                        }
                        let confirmation = crate::l3::notification::SpachConfirmation {
                            confirmed_type: observation.confirmed_type,
                            ..Default::default()
                        }
                        .encode()
                        .unwrap();
                        self.response_blocks[index] = Some(block);
                        state.packets.extend(
                            access_packets(observation.identity, &confirmation)
                                .into_iter()
                                .map(|(offset, packet)| (block + offset, packet)),
                        );
                        self.responded[index] = true;
                    }
                }
                continue;
            }
            let Ok(Some(message)) = self.receiver.push(&burst.fields.block.information) else {
                continue;
            };
            if message.channel != crate::link::spach::Channel::Pch {
                continue;
            }
            match self.mode {
                QueryMode::MessageWaiting | QueryMode::UserAlert => unreachable!(),
                QueryMode::Capabilities => {
                    if message.message != crate::l3::capability_report::request() {
                        continue;
                    }
                }
                QueryMode::Audit { force_registration } => {
                    let Ok(order) = crate::l3::audit::AuditOrder::decode(&message.message) else {
                        continue;
                    };
                    if order.force_registration != force_registration
                        || order.debug_display_allowed
                        || order.subaddress.is_some()
                        || !order.unparsed_bits.is_empty()
                    {
                        return Err("unexpected audit flags or scope".into());
                    }
                }
            }
            let index = match message.identity {
                MobileIdentity::Min34(1) => 0,
                MobileIdentity::Min34(2) => 1,
                _ => return Err("unexpected capability poll identity".into()),
            };
            let physical = ((first - (FILTER_SPAN * SAMPLES_PER_SYMBOL) as f64)
                / (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL) as f64)
                .round() as u64;
            let sf = physical / PHYSICAL_SLOTS;
            let phase = ((physical % PHYSICAL_SLOTS) / CONTROL_SLOT_PERIOD) as u8;
            if phase
                != self
                    .allocation
                    .phase(message.identity)
                    .map_err(|error| HalError::from(error.to_string()))?
            {
                return Err("capability request used wrong paging subchannel".into());
            }
            if sf.is_multiple_of(2) {
                if self.primary[index].is_some() {
                    return Err("capability request primary repeated unexpectedly".into());
                }
                self.primary[index] = Some((
                    sf,
                    burst
                        .fields
                        .block
                        .information
                        .as_slice()
                        .try_into()
                        .unwrap(),
                ));
            } else {
                let (primary, information) =
                    self.primary[index].ok_or("secondary poll without primary")?;
                if sf != primary + 1
                    || information.as_slice() != burst.fields.block.information.as_slice()
                {
                    return Err("capability request secondary changed PCH information".into());
                }
                self.secondary[index] = true;
            }
            if !self.responded[index]
                && matches!(
                    (self.reply_on, sf.is_multiple_of(2)),
                    (ReplyOn::Primary, true) | (ReplyOn::Secondary, false)
                )
            {
                let report = CapabilityReport {
                    protocol_version: ProtocolVersion::Pv7,
                    scm: 15,
                    software_vintage: 2,
                    firmware_vintage: 3,
                    model: 4 + index as u8,
                    manufacturer: 0x20,
                    max_supported_pfc: 5,
                    supported_bands: 3,
                    services: SupportedServices {
                        ira: true,
                        ..Default::default()
                    },
                    options: ReportOptions {
                        complete_model: Some(REPORT_MODEL + index as u8),
                        voice_coder_map: Some(0x1f),
                        extensions: crate::l3::capability_report::ReportExtensions {
                            dads: Some(true),
                            advice: Some(false),
                            positioning: Some(15),
                            frequency_bands: Some(1),
                            multilingual: Some(0xf000),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                }
                .encode()
                .unwrap();
                let block = (state.slot_count as u64).div_ceil(CONTROL_SLOT_PERIOD)
                    + RESPONSE_LEAD_BLOCKS
                    + index as u64;
                let first_reverse_symbol = block * CONTROL_SLOT_PERIOD * SYMBOLS_PER_SLOT as u64
                    - REVERSE_LEAD_SYMBOLS as u64;
                let last_request_symbol = (physical + 1) * SYMBOLS_PER_SLOT as u64;
                if first_reverse_symbol < last_request_symbol
                    || first_reverse_symbol - last_request_symbol
                        > SYMBOL_RATE_HZ as u64
                            * super::super::super::timers::MOBILE_RESPONSE_WINDOW_MS
                            / 1000
                {
                    return Err("capability response readiness exceeded one superframe".into());
                }
                match self.mode {
                    QueryMode::MessageWaiting | QueryMode::UserAlert => unreachable!(),
                    QueryMode::Capabilities => state.packets.extend(
                        access_packets(message.identity, &report)
                            .into_iter()
                            .map(|(offset, packet)| (block + offset, packet)),
                    ),
                    QueryMode::Audit { force_registration } => {
                        let confirmation = crate::l3::audit::AuditConfirmation::default()
                            .encode()
                            .unwrap();
                        let mut confirmation_block = block;
                        if force_registration {
                            let registration = AccessMessage::Registration {
                                registration_type: RegistrationType::Forced,
                                scm: 15,
                                protocol_version: ProtocolVersion::Is136RevA,
                            }
                            .encode()
                            .unwrap();
                            if self.combined_audit {
                                state.packets.extend(
                                    rach::encode(
                                        BurstFormat::Normal,
                                        message.identity,
                                        &[&confirmation, &registration, &report],
                                    )
                                    .unwrap()
                                    .iter()
                                    .enumerate()
                                    .map(|(part, frame)| {
                                        (
                                            block + part as u64 * ACCESS_PATH_BLOCKS,
                                            burst_packet(frame),
                                        )
                                    }),
                                );
                                self.responded[index] = true;
                                continue;
                            }
                            let frames = rach::encode(
                                BurstFormat::Normal,
                                message.identity,
                                &[&registration, &report],
                            )
                            .unwrap();
                            let registration_block = if index == 0 {
                                block + ACCESS_PATH_BLOCKS
                            } else {
                                block
                            };
                            if index == 1 {
                                confirmation_block =
                                    block + frames.len() as u64 * ACCESS_PATH_BLOCKS;
                            }
                            state
                                .packets
                                .extend(frames.iter().enumerate().map(|(part, frame)| {
                                    (
                                        registration_block + part as u64 * ACCESS_PATH_BLOCKS,
                                        burst_packet(frame),
                                    )
                                }));
                        }
                        let ready =
                            confirmation_block * CONTROL_SLOT_PERIOD * SYMBOLS_PER_SLOT as u64
                                - REVERSE_LEAD_SYMBOLS as u64;
                        if ready - last_request_symbol
                            > SYMBOL_RATE_HZ as u64
                                * super::super::super::timers::MOBILE_RESPONSE_WINDOW_MS
                                / 1000
                        {
                            return Err("audit response readiness exceeded one superframe".into());
                        }
                        state.packets.extend(
                            access_packets(message.identity, &confirmation)
                                .into_iter()
                                .map(|(offset, packet)| (confirmation_block + offset, packet)),
                        );
                    }
                }
                self.responded[index] = true;
            }
        }
        Ok(())
    }
}
fn run(reply_on: ReplyOn, mode: QueryMode) {
    run_with_combined_audit(reply_on, mode, false);
}
fn run_with_combined_audit(reply_on: ReplyOn, mode: QueryMode, combined_audit: bool) {
    let _paced = PACED_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = Database(std::env::temp_dir().join(format!(
        "136bts-idle-cap-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = db.0.clone();
    let messages = messages(&config);
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) = &messages[0] else {
        panic!()
    };
    let allocation = PagingAllocation::try_new(structure).unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let second = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 15,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    let mut packets = vec![(ACCESS_BLOCK, packet())];
    packets.extend(
        access_packets(MobileIdentity::Min34(2), &second)
            .into_iter()
            .map(|(offset, packet)| (SECOND_REGISTRATION_BLOCK + offset, packet)),
    );
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        clock_sample: 0,
        tx_observation_offset_ticks: 0,
        epoch: None,
        tx: Vec::new(),
        slot_count: 0,
        stop_slots: if matches!(mode, QueryMode::MessageWaiting | QueryMode::UserAlert) {
            1500
        } else {
            TEST_STOP_SLOTS
        },
        underflow_after_slot: None,
        stall_after_slot: None,
        realtime_epoch: Some(Instant::now()),
        handset_segmented: None,
        handset_sms: None,
        handset_voice: None,
        handset_arq: None,
        handset_idle: Some({
            let mut handsets = Handsets::new(reply_on, allocation, mode);
            handsets.combined_audit = combined_audit;
            handsets
        }),
        traffic_packets: Vec::new(),
        packets,
        tx_active: false,
        rx_active: false,
        shutdown: shutdown.clone(),
    }));
    let prepared = PreparedRadio::from_radio(&config, Box::new(Device(state.clone()))).unwrap();
    let (hub, interface) = crate::operations::Hub::new(&config);
    let operator_hub = hub.clone();
    let operator_stop = shutdown.clone();
    let operator = std::thread::spawn(move || {
        while !operator_stop.load(Ordering::Relaxed) {
            if operator_hub
                .snapshot()
                .subscribers
                .iter()
                .filter(|subscriber| subscriber.registered)
                .count()
                == 2
            {
                for identity in [1, 2] {
                    let (response, result) = tokio::sync::oneshot::channel();
                    operator_hub
                        .commands
                        .try_send(PendingCommand {
                            command: match mode {
                                QueryMode::UserAlert => {
                                    let alert = waiting::alert();
                                    Command::UserAlert(proto::SendUserAlertRequest {
                                        subscriber_id: Some(proto::SubscriberIdentity {
                                            kind: 2,
                                            value: identity,
                                            esn: None,
                                        }),
                                        signal: alert.signal.map(|signal| proto::ControlSignal {
                                            pitch: signal.pitch as u32,
                                            cadence: u32::from(signal.cadence),
                                            duration_code: u32::from(signal.duration_code),
                                        }),
                                        display: alert.display,
                                    })
                                }
                                QueryMode::MessageWaiting => {
                                    Command::MessageWaiting(proto::SendMessageWaitingRequest {
                                        subscriber_id: Some(proto::SubscriberIdentity {
                                            kind: 2,
                                            value: identity,
                                            esn: None,
                                        }),
                                        messages: waiting::message()
                                            .entries
                                            .iter()
                                            .map(|entry| proto::WaitingMessage {
                                                kind: u32::from(entry.kind),
                                                count: u32::from(entry.count),
                                            })
                                            .collect(),
                                        display: waiting::message().display,
                                    })
                                }
                                QueryMode::Capabilities => {
                                    Command::Capabilities(proto::SubscriberIdentity {
                                        kind: 2,
                                        value: identity,
                                        esn: None,
                                    })
                                }
                                QueryMode::Audit { force_registration } => {
                                    Command::Audit(proto::AuditSubscriberRequest {
                                        subscriber_id: Some(proto::SubscriberIdentity {
                                            kind: 2,
                                            value: identity,
                                            esn: None,
                                        }),
                                        force_registration,
                                        debug_display_allowed: false,
                                    })
                                }
                            },
                            response,
                        })
                        .unwrap();
                    assert!(result.blocking_recv().unwrap().is_ok());
                }
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    });
    let result = run_prepared_with_interface(
        &config,
        prepared,
        &shutdown,
        Some(Duration::from_secs(
            if matches!(mode, QueryMode::MessageWaiting | QueryMode::UserAlert) {
                10
            } else {
                TEST_DURATION_SECONDS
            },
        )),
        Some(interface),
    );
    shutdown.store(true, Ordering::Relaxed);
    assert!(operator.join().unwrap());
    let summary = result.unwrap();
    assert_eq!(
        summary.completed_registrations,
        if matches!(
            mode,
            QueryMode::Audit {
                force_registration: true
            }
        ) {
            4
        } else {
            2
        }
    );
    assert!(summary.stop_acknowledged);
    {
        let state = state.lock().unwrap();
        let handsets = state.handset_idle.as_ref().unwrap();
        assert!(
            handsets.responded.iter().all(|value| *value),
            "primary={:?} secondary={:?} responded={:?}",
            handsets
                .primary
                .iter()
                .map(|value| value.map(|(sf, _)| sf))
                .collect::<Vec<_>>(),
            handsets.secondary,
            handsets.responded
        );
        assert!(handsets.secondary.iter().all(|value| *value));
    }
    let snapshot = hub.snapshot();
    for subscriber in &snapshot.subscribers {
        assert!(subscriber.registered);
        let forced = matches!(
            mode,
            QueryMode::Audit {
                force_registration: true
            }
        );
        if forced {
            assert!(subscriber.last_registration_superframe.unwrap() > 0);
        } else {
            assert_eq!(subscriber.last_registration_superframe, Some(0));
        }
        let label = match mode {
            QueryMode::Capabilities => "Idle capability poll",
            QueryMode::MessageWaiting => "Idle Message Waiting",
            QueryMode::UserAlert => "Idle User Alert",
            QueryMode::Audit { .. } => "Idle audit",
        };
        assert!(
            subscriber
                .measurements
                .iter()
                .any(|metric| metric.name == label && metric.value.as_deref() == Some("Answered")),
            "{:?}",
            subscriber.measurements
        );
        if matches!(mode, QueryMode::Capabilities) || forced {
            assert!(
                subscriber
                    .measurements
                    .iter()
                    .any(|metric| metric.name == "Handset model"
                        && metric.value.as_deref()
                            == Some(if subscriber.number == "1001" {
                                "0x34"
                            } else {
                                "0x35"
                            }))
            );
        }
        if matches!(mode, QueryMode::Capabilities) || forced {
            for (name, value) in [
                ("DADS support", "true"),
                ("ADVICE support", "false"),
                ("Reported positioning map", "0x0F"),
                ("Reported extended bands", "0x01"),
                ("Reported extended character map", "0xF000"),
            ] {
                assert!(subscriber.measurements.iter().any(|metric|metric.name==name&&metric.value.as_deref()==Some(value)),"{name}: {:?}",subscriber.measurements);
            }
        }
        if matches!(mode, QueryMode::Audit { .. }) {
            assert!(
                subscriber
                    .measurements
                    .iter()
                    .any(|metric| metric.name == "Reported current PFC"
                        && metric.value.as_deref() == Some("1"))
            );
            if forced {
                assert!(
                    subscriber
                        .measurements
                        .iter()
                        .any(|metric| metric.name == "Audit forced registration received"
                            && metric.value.as_deref() == Some("true"))
                );
            }
        }
    }
    let (request_name, response_name) = match mode {
        QueryMode::Capabilities => ("Capability request", "Capability report"),
        QueryMode::MessageWaiting => ("Message Waiting", "SPACH Confirmation"),
        QueryMode::UserAlert => ("User Alert", "SPACH Confirmation"),
        QueryMode::Audit { .. } => ("Audit order", "Audit confirmation"),
    };
    assert_eq!(
        snapshot
            .air_messages
            .iter()
            .filter(|message| message.direction == "Forward" && message.name == request_name)
            .count(),
        2
    );
    assert_eq!(
        snapshot
            .air_messages
            .iter()
            .filter(|message| message.direction == "Reverse" && message.name == response_name)
            .count(),
        2
    );
    if matches!(
        mode,
        QueryMode::Capabilities
            | QueryMode::Audit {
                force_registration: true
            }
    ) {
        assert_eq!(
            snapshot
                .air_messages
                .iter()
                .filter(|message| message.direction == "Reverse"
                    && message.name == "Capability report"
                    && message.decoded.contains("LanguagesExt=0xF000"))
                .count(),
            2
        );
    }
    let state = state.lock().unwrap();
    let handsets = state.handset_idle.as_ref().unwrap();
    assert!(handsets.responded.iter().all(|value| *value));
    assert!(handsets.secondary.iter().all(|value| *value));
    assert!(!state.tx_active && !state.rx_active);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn dedicated_capability_polls_accept_standalone_reports_and_repeat_after_primary_responses() {
    run(ReplyOn::Primary, QueryMode::Capabilities);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn dedicated_capability_polls_recover_when_phones_only_answer_secondary_pch() {
    run(ReplyOn::Secondary, QueryMode::Capabilities);
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn idle_audit_confirms_without_refreshing_registration() {
    run(
        ReplyOn::Primary,
        QueryMode::Audit {
            force_registration: false,
        },
    );
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn idle_audit_force_receives_confirmation_and_registration_in_either_order() {
    run(
        ReplyOn::Secondary,
        QueryMode::Audit {
            force_registration: true,
        },
    );
}

mod waiting;
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn idle_message_waiting_fragments_and_confirms_without_changing_registration() {
    run(ReplyOn::Primary, QueryMode::MessageWaiting);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn idle_message_waiting_confirms_after_secondary_copy_reassembly() {
    run(ReplyOn::Secondary, QueryMode::MessageWaiting);
}

mod mixed;

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn idle_user_alert_fragments_and_confirms_without_changing_registration() {
    run(ReplyOn::Primary, QueryMode::UserAlert);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn idle_user_alert_confirms_after_secondary_copy_reassembly() {
    run(ReplyOn::Secondary, QueryMode::UserAlert);
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn nokia_combined_audit_and_forced_registration_complete_through_iq() {
    run_with_combined_audit(
        ReplyOn::Secondary,
        QueryMode::Audit {
            force_registration: true,
        },
        true,
    );
}
