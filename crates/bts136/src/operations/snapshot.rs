use super::{
    Command, RuntimeInterface, metric,
    proto::{self, Snapshot},
    unavailable, unix_ms,
};
use crate::{
    config::Config,
    link::rach::MobileIdentity,
    radio::timed_tx::Observation,
    runtime::{sms::SmsRuntime, voice::VoiceRuntime},
};
const LIVE_MESSAGE_LIMIT: usize = 200;
const MAX_ACCESS_READINGS: usize = 256;
pub(super) fn initial(config: &Config) -> Snapshot {
    let mut carrier = vec![
        metric(
            "Band",
            match config.cell.band {
                crate::config::ConfigBand::Cellular800 => "800 MHz cellular",
                crate::config::ConfigBand::Pcs1900 => "1900 MHz PCS",
            },
            "",
        ),
        metric("Channel", config.cell.channel, ""),
        metric("Control slots", "1+4", ""),
        metric("SID", config.cell.sid, ""),
        metric(
            "Network name",
            config
                .cell
                .network_name
                .as_deref()
                .unwrap_or("Not broadcast"),
            "",
        ),
        metric("Time source", "Local system", ""),
        metric("Home registration", config.registration.home, ""),
        metric("Roaming registration", config.registration.roaming, ""),
        metric(
            "Registration period",
            config
                .registration
                .period_superframes
                .map_or_else(|| "Disabled".to_owned(), registration_period),
            "",
        ),
        metric(
            "Registration area",
            config
                .registration
                .area
                .map_or_else(|| "Disabled".to_owned(), |value| value.to_string()),
            "",
        ),
        metric("Configured neighbors", config.cell.neighbors.len(), ""),
        metric(
            "Operator identity",
            config.cell.operator.as_ref().map_or_else(
                || "Not broadcast".to_owned(),
                |value| format!("SOC={} BSMC={}", value.soc, value.bsmc),
            ),
            "",
        ),
        metric("DVCC", config.cell.dvcc, ""),
        metric("Authentication", "Disabled", ""),
        metric("Speech codecs", "VSELP / ACELP CC1", ""),
        metric("Access power level", config.cell.access_power_level, ""),
        metric("Traffic power level", config.cell.traffic_power_level, ""),
    ];
    if let Ok(prepared) = config.prepare() {
        carrier.extend([
            metric("Forward TX", prepared.frequencies.forward_hz, "Hz"),
            metric("Reverse RX", prepared.frequencies.reverse_hz, "Hz"),
        ]);
    }
    carrier.extend([
        metric("Radio backend", config.radio.device.kind_name(), ""),
        metric("Answer timeout", config.calls.answer_timeout_seconds, "s"),
    ]);
    if let Some(gain) = config.radio.device.rx_gain_db_f64() {
        carrier.push(metric("RX gain requested", gain, "dB"));
    }
    if let sdr_hal::config::DeviceConfig::Uhd {
        tx_gain_db,
        antenna,
        master_clock_rate,
        clock_source,
        time_source,
        ..
    } = &config.radio.device
    {
        carrier.extend([
            metric("TX gain requested", tx_gain_db, "dB"),
            metric("TX antenna", antenna, ""),
            match master_clock_rate {
                Some(rate) => metric("Master clock requested", rate, "Hz"),
                None => metric("Master clock requested", "Automatic", ""),
            },
            metric(
                "Clock source requested",
                clock_source.as_deref().unwrap_or("Driver default"),
                "",
            ),
            metric(
                "Time source requested",
                time_source.as_deref().unwrap_or("Driver default"),
                "",
            ),
        ]);
    }
    Snapshot {
        phase: proto::CellPhase::Starting as i32,
        carrier,
        ..Default::default()
    }
}
impl RuntimeInterface {
    pub(crate) fn access_reading(
        &mut self,
        identity: crate::link::rach::MobileIdentity,
        frequency_offset_hz: f64,
        sync_correlation: f64,
        physical_slot: u64,
        procedure: &str,
    ) {
        if self.access_readings.len() >= MAX_ACCESS_READINGS
            && !self.access_readings.contains_key(&identity)
        {
            return;
        }
        self.access_readings.insert(
            identity,
            vec![
                metric("Last access procedure", procedure, ""),
                metric("Last access physical slot", physical_slot, ""),
                metric(
                    "Access frequency offset",
                    format!("{frequency_offset_hz:.2}"),
                    "Hz",
                ),
                metric(
                    "Access sync correlation",
                    format!("{sync_correlation:.4}"),
                    "",
                ),
            ],
        );
    }
    pub(crate) fn maca_reading(
        &mut self,
        identity: MobileIdentity,
        association: &crate::runtime::maca::Association,
        physical_slot: u64,
    ) {
        if self.maca_readings.len() >= MAX_ACCESS_READINGS
            && !self.maca_readings.contains_key(&identity)
        {
            return;
        }
        use crate::runtime::maca::Association;
        let mut readings = vec![metric("MACA last access slot", physical_slot, "")];
        match association {
            Association::Unmapped(reason) => readings.push(metric("MACA report", reason, "")),
            Association::Observed(measurements) => {
                readings.push(metric(
                    "MACA report",
                    "Associated with advertised targets",
                    "",
                ));
                for reading in &measurements.readings {
                    let name = format!(
                        "MACA {:?} channel {} RSS",
                        reading.hyperband, reading.channel
                    );
                    readings.push(match reading.rss_code {
                        Some(code) => metric(
                            name,
                            reading
                                .format
                                .rss_description(code)
                                .expect("associated RSS code is validated"),
                            "",
                        ),
                        None => unavailable(&name, "Not measured"),
                    });
                }
                if let Some(long_term) = measurements.long_term {
                    readings.push(metric("MACA long-term WER code", long_term.wer, ""));
                    readings.push(metric("MACA long-term BER code", long_term.ber, ""));
                    readings.push(metric("MACA full measurement interval", long_term.full, ""));
                    if let Some(format) = measurements.format {
                        readings.push(metric(
                            "MACA long-term RSS",
                            format
                                .rss_description(long_term.rss)
                                .expect("associated RSS code is validated"),
                            "",
                        ));
                    }
                }
            }
        }
        self.maca_readings.insert(identity, readings);
    }
    pub(crate) fn process(
        &mut self,
        sms: &mut SmsRuntime,
        voice: &mut VoiceRuntime,
        idle: &mut crate::runtime::idle_queries::IdleQueries,
        now_ms: u64,
    ) {
        for _ in 0..1 {
            let Ok(pending) = self.commands.try_recv() else {
                break;
            };
            let result = match pending.command {
                Command::StartCall(request) => {
                    let identity = request
                        .subscriber_id
                        .ok_or_else(|| {
                            tonic::Status::invalid_argument("Subscriber identity is required")
                        })
                        .and_then(subscriber_identity);
                    identity.and_then(|identity| {
                        if sms.subscriber_busy(identity) {
                            return Err(tonic::Status::failed_precondition(
                                "Subscriber has an active SMS transfer",
                            ));
                        }
                        voice
                            .begin_audio_call(
                                sms.core().registry(),
                                identity,
                                &request.calling_number,
                                now_ms,
                            )
                            .map(|call_id| proto::CommandResult {
                                call_id: Some(call_id),
                                message: "Call paging requested".into(),
                                ..Default::default()
                            })
                            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))
                    })
                }
                Command::Release(request) => voice
                    .release_call(request.call_id, now_ms)
                    .map(|()| proto::CommandResult {
                        message: "Release requested".into(),
                        ..Default::default()
                    })
                    .map_err(|error| tonic::Status::not_found(error.to_string())),
                Command::Measurements(request) => set_measurements(voice, request, now_ms),
                Command::Status(request) => request_status(voice, request, now_ms),
                Command::UserAlert(request) => {
                    super::user_alert::send(idle, sms, voice, request, now_ms)
                }
                Command::MessageWaiting(request) => {
                    send_message_waiting(idle, sms, voice, request, now_ms)
                }
                Command::Audit(request) => audit_subscriber(idle, sms, voice, request, now_ms),
                Command::Capabilities(request) => {
                    request_capabilities(idle, sms, voice, request, now_ms)
                }
                Command::CallInformation(request) => {
                    super::call_information::send(voice, request, now_ms)
                }
                Command::PhysicalControl(request) => set_physical_control(voice, request, now_ms),
                Command::Sms(request) => send_sms(sms, request, self.sms_display_time),
                Command::SaveSubscriber(request) => save_subscriber(sms, voice, request, now_ms),
                Command::DeleteSubscriber(request) => delete_subscriber(sms, voice, request),
            };
            let _ = pending.response.send(result);
        }
    }
    pub(crate) fn publish(
        &mut self,
        sms: &SmsRuntime,
        voice: &VoiceRuntime,
        idle: &crate::runtime::idle_queries::IdleQueries,
        observation: Observation,
        status: RuntimeStatus,
    ) {
        let current = self.hub.snapshots.borrow().clone();
        let mut published = (*current).clone();
        let snapshot = &mut published.snapshot;
        snapshot.revision += 1;
        snapshot
            .carrier
            .retain(|metric| !metric.name.starts_with("SMPP") && metric.name != "SIP state");
        snapshot.sip = voice.sip_trunk();
        if let Some(sip) = voice.sip_status() {
            snapshot.carrier.push(metric("SIP state", sip, ""));
        }
        snapshot.sms_hub = sms.core().sms_hub_status().map(|hub| proto::TrunkStatus {
            state: match hub.state {
                "Disabled" => proto::TrunkState::Disabled,
                "Bound" => proto::TrunkState::Up,
                "Backoff" => proto::TrunkState::Backoff,
                "Stopped" => proto::TrunkState::Stopped,
                _ => proto::TrunkState::Connecting,
            } as i32,
            detail: format!("{} {}", hub.state, hub.peer),
            last_error: hub.error.clone().unwrap_or_default(),
            queued: hub.pending as u64,
        });
        if let Some(hub) = sms.core().sms_hub_status() {
            snapshot.carrier.extend([
                metric("SMPP state", hub.state, ""),
                metric("SMPP peer", hub.peer, ""),
                metric("SMPP queued", hub.pending, ""),
            ]);
            if let Some(error) = hub.error {
                snapshot.carrier.push(metric("SMPP last error", error, ""));
            }
        }

        snapshot.unix_ms = unix_ms();
        snapshot.set_phase(match observation.state {
            crate::radio::timed_tx::State::Prepared => proto::CellPhase::Starting,
            crate::radio::timed_tx::State::Running => proto::CellPhase::Running,
            crate::radio::timed_tx::State::Stopped => proto::CellPhase::Stopped,
            crate::radio::timed_tx::State::Failed => proto::CellPhase::Failed,
        });
        let recent = self.radio_history.recent(RadioSample {
            now_ms: status.now_ms,
            missed_slots: status.skipped_slots,
            underflows: observation.health.underflows,
        });
        snapshot.health = Some(proto::RadioHealth {
            missed_slots_last_minute: recent.missed_slots,
            underflows_last_minute: recent.underflows,
        });
        snapshot.radio = vec![
            metric("Uptime", status.now_ms, "ms"),
            metric("Missed TX slots, last minute", recent.missed_slots, ""),
            metric("TX underflows, last minute", recent.underflows, ""),
            metric(
                "TX underflows (nonfatal)",
                observation.health.underflows,
                "",
            ),
            metric(
                "Late packets (nonfatal)",
                observation.health.late_packets,
                "",
            ),
            metric("Sequence errors", observation.health.sequence_errors, ""),
            metric("Dropped packets", observation.health.dropped_packets, ""),
            metric("Unknown events", observation.health.unknown_events, ""),
            metric("Burst acknowledgments", observation.health.burst_acks, ""),
            metric("Missed TX slots", status.skipped_slots, ""),
            metric("RX discontinuities", status.rx_discontinuities, ""),
            metric("TX queue depth", status.queue_depth, "slots"),
            metric(
                "TX queue capacity",
                crate::runtime::serving::QUEUED_SLOTS,
                "slots",
            ),
            metric("Hardware timestamp", observation.hardware_tick, "ticks"),
            unavailable("LO lock", "The radio does not report LO lock"),
            unavailable(
                "Clock offset",
                "No independent frequency reference is measured",
            ),
            unavailable("Slot jitter", "Scheduling jitter is not measured"),
        ];
        if let Some(power) = status.rx_power_dbfs {
            snapshot
                .radio
                .push(metric("Last RX block power", format!("{power:.2}"), "dBFS"));
        }
        if let Some(offset) = status.last_frequency_offset_hz {
            snapshot.radio.push(metric(
                "Last reverse burst frequency offset",
                format!("{offset:.2}"),
                "Hz",
            ));
        }
        if let Some(correlation) = status.last_sync_correlation {
            snapshot.radio.push(metric(
                "Last reverse sync correlation",
                format!("{correlation:.4}"),
                "",
            ));
        }
        snapshot.service = vec![
            metric("Software version", env!("CARGO_PKG_VERSION"), ""),
            metric(
                "Scheduled hyperframe counter",
                status.position.hyperframe_counter,
                "",
            ),
            metric(
                "Scheduled superframe phase",
                format!("{:?}", status.position.superframe),
                "",
            ),
            metric("Physical slot", status.submitted, ""),
            metric("Frame", status.submitted / 6, ""),
            metric(
                "Superframe",
                status.submitted / crate::scheduler::broadcast::PHYSICAL_SLOTS,
                "",
            ),
            metric(
                "Registration replies completed",
                status.completed_registrations,
                "",
            ),
            metric("Reverse access bursts", status.access_bursts, ""),
            metric("Reverse traffic bursts", status.traffic_bursts, ""),
            metric(
                "Active calls",
                voice.operation_calls(status.now_ms).len(),
                "",
            ),
            metric("Authentication", "Disabled", ""),
        ];
        let mut subscribers: Vec<proto::Subscriber> = sms
            .core()
            .registry()
            .subscribers()
            .iter()
            .map(|subscriber| {
                let registration = subscriber.registration;
                let (kind, _, identity_value) = subscriber.provisioning.identity.parts();
                proto::Subscriber {
                    subscriber_id: Some(proto::SubscriberIdentity {
                        kind: u32::from(kind),
                        value: identity_value,
                        esn: None,
                    }),
                    identity: format!("{:?}", subscriber.provisioning.identity),
                    number: subscriber.provisioning.number.clone(),
                    registered: registration.is_some(),
                    provisioned_esn: sms
                        .core()
                        .devices()
                        .iter()
                        .find(|device| device.identity == Some(subscriber.provisioning.identity))
                        .map(|device| device.esn),
                    esn: registration.and_then(|registration| registration.esn),
                    scm: registration.map(|registration| u32::from(registration.scm)),
                    last_registration_superframe: registration
                        .map(|registration| registration.last_registration_superframe),
                    last_response_unix_ms: None,
                    reverse_quality: None,
                    last_registration_unix_ms: registration.map(|registration| {
                        unix_ms().saturating_sub(
                            ((status.submitted / crate::scheduler::broadcast::PHYSICAL_SLOTS)
                                .saturating_sub(registration.last_registration_superframe)
                                * crate::runtime::timers::SUPERFRAME_MS)
                                as i64,
                        )
                    }),
                    protocol_version: registration
                        .map(|registration| format!("{:?}", registration.protocol_version)),
                    measurements: super::subscriber_capabilities::measurements(
                        subscriber.capabilities.as_ref(),
                    )
                    .into_iter()
                    .chain(super::subscriber_capabilities::registration(
                        subscriber.registration_options.as_ref(),
                    ))
                    .chain(super::subscriber_capabilities::serial(
                        subscriber.reported_serial.as_ref(),
                    ))
                    .chain(registration.map(|registration| {
                        metric(
                            "Reachability learned from",
                            if registration.last_observed_access_superframe.is_some() {
                                "Call access"
                            } else {
                                "Registration"
                            },
                            "",
                        )
                    }))
                    .collect(),
                }
            })
            .collect();
        for device in sms
            .core()
            .devices()
            .iter()
            .filter(|device| device.identity.is_none())
        {
            subscribers.push(proto::Subscriber {
                identity: format!("ESN({})", device.esn),
                number: device.number.clone(),
                provisioned_esn: Some(device.esn),
                measurements: vec![metric(
                    "Mobile identity",
                    "Waiting for first registration",
                    "",
                )],
                ..Default::default()
            });
        }
        for subscriber in &mut subscribers {
            if let Some(id) = subscriber
                .subscriber_id
                .and_then(|id| subscriber_identity(id).ok())
                && let Some(state) = idle.state(id)
            {
                subscriber
                    .measurements
                    .push(metric("Idle capability poll", state.label(), ""));
            }

            if let Some(id) = subscriber
                .subscriber_id
                .and_then(|id| subscriber_identity(id).ok())
            {
                if let Some(state) = idle.user_alert_state(id) {
                    subscriber
                        .measurements
                        .push(metric("Idle User Alert", state.label(), ""));
                }
                if let Some(state) = idle.message_waiting_state(id) {
                    subscriber
                        .measurements
                        .push(metric("Idle Message Waiting", state.label(), ""));
                }
                if let Some(state) = idle.audit_state(id) {
                    subscriber
                        .measurements
                        .push(metric("Idle audit", state.label(), ""));
                }
                if let Some(confirmation) = idle.audit_confirmation(id) {
                    subscriber
                        .measurements
                        .push(metric("Audit confirmation received", true, ""));
                    subscriber.measurements.push(metric(
                        "Reported current PFC",
                        confirmation.pfc_minus_one + 1,
                        "frames",
                    ));
                    if let Some(display) = &confirmation.display {
                        subscriber.measurements.push(metric(
                            "Audit debug display",
                            display.escape_default(),
                            "",
                        ));
                    }
                }
                if let Some(received) = idle.forced_registration_received(id) {
                    subscriber.measurements.push(metric(
                        "Audit forced registration received",
                        received,
                        "",
                    ));
                }
            }
            if let Some(source) = sms
                .core()
                .registry()
                .subscribers()
                .iter()
                .find(|source| source.provisioning.number == subscriber.number)
                && let Some(readings) = self.access_readings.get(&source.provisioning.identity)
            {
                subscriber.measurements.extend(readings.iter().cloned());
            }
            if let Some(id) = subscriber
                .subscriber_id
                .and_then(|id| subscriber_identity(id).ok())
                && let Some(readings) = self.maca_readings.get(&id)
            {
                subscriber.measurements.extend(readings.iter().cloned());
            }
        }
        snapshot.calls = voice.operation_calls(status.now_ms);
        snapshot.registered_phones = subscribers
            .iter()
            .filter(|subscriber| subscriber.registered)
            .count() as u32;
        published.subscribers = subscribers;
        published.recent_calls = voice.recent_calls();
        published.live_messages = sms.operation_live_messages(LIVE_MESSAGE_LIMIT);
        self.hub
            .snapshots
            .send_replace(std::sync::Arc::new(published));
    }
    pub(crate) fn stopped(&mut self, acknowledged: bool, failure: Option<String>) {
        let current = self.hub.snapshots.borrow().clone();
        let mut published = (*current).clone();
        let snapshot = &mut published.snapshot;
        snapshot.revision += 1;
        snapshot.unix_ms = unix_ms();
        snapshot.set_phase(match (acknowledged, &failure) {
            (false, _) => proto::CellPhase::StopUnconfirmed,
            (true, None) => proto::CellPhase::Stopped,
            (true, Some(_)) => proto::CellPhase::Failed,
        });
        snapshot.failure = failure.unwrap_or_default();
        snapshot
            .radio
            .push(metric("Stop acknowledged", acknowledged, ""));
        self.hub
            .snapshots
            .send_replace(std::sync::Arc::new(published));
    }
}
const RADIO_HEALTH_WINDOW_MS: u64 = 60_000;

#[derive(Default)]
pub(crate) struct RadioHistory {
    samples: std::collections::VecDeque<RadioSample>,
}
#[derive(Clone, Copy)]
struct RadioSample {
    now_ms: u64,
    missed_slots: u64,
    underflows: u64,
}
impl RadioHistory {
    fn recent(&mut self, current: RadioSample) -> RadioSample {
        self.samples.push_back(current);
        while self.samples.len() > 1
            && self.samples[1]
                .now_ms
                .saturating_add(RADIO_HEALTH_WINDOW_MS)
                <= current.now_ms
        {
            self.samples.pop_front();
        }
        let oldest = self.samples[0];
        RadioSample {
            now_ms: current.now_ms.saturating_sub(oldest.now_ms),
            missed_slots: current.missed_slots.saturating_sub(oldest.missed_slots),
            underflows: current.underflows.saturating_sub(oldest.underflows),
        }
    }
}
pub(crate) struct RuntimeStatus {
    pub position: crate::scheduler::broadcast::Position,
    pub now_ms: u64,
    pub submitted: u64,
    pub completed_registrations: u64,
    pub skipped_slots: u64,
    pub rx_discontinuities: u64,
    pub queue_depth: usize,
    pub access_bursts: u64,
    pub traffic_bursts: u64,
    pub rx_power_dbfs: Option<f64>,
    pub last_frequency_offset_hz: Option<f64>,
    pub last_sync_correlation: Option<f64>,
}
fn send_sms(
    sms: &mut SmsRuntime,
    request: proto::SendSmsRequest,
    display_time: crate::l3::sms::DisplayTime,
) -> Result<proto::CommandResult, tonic::Status> {
    use crate::{
        core::store::NewOperatorSms,
        l3::sms::{Metadata, NORMAL_URGENCY, SmsMessage, UserData, Validity},
    };
    if sms
        .core()
        .devices()
        .iter()
        .any(|device| device.identity.is_none() && device.number == request.recipient_number)
    {
        return Err(tonic::Status::failed_precondition(
            "Wait for the ESN subscriber's first registration before sending SMS",
        ));
    }
    let registry = sms.core().registry();
    let recipient = registry
        .by_number(&request.recipient_number)
        .ok_or_else(|| tonic::Status::not_found("Recipient number is not provisioned"))?
        .provisioning
        .identity;
    let message = SmsMessage::Deliver {
        metadata: Metadata {
            message_reference: 0,
            privacy: 0,
            urgency: NORMAL_URGENCY,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        validity: Validity::Indefinite,
        display_time,
        user_data: UserData::from_text(&request.text)
            .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?,
    };
    let key = format!("operator:{}", request.request_id);
    let id = sms
        .core_mut()
        .store_mut()
        .enqueue_operator(NewOperatorSms {
            dedup_key: &key,
            sender_number: &request.sender_number,
            recipient,
            message: &message,
            created_at_unix_ms: unix_ms(),
        })
        .map_err(|error| match error {
            crate::core::store::StoreError::IdempotencyConflict => {
                tonic::Status::already_exists("Request ID was used for a different message")
            }
            crate::core::store::StoreError::Traffic(
                crate::l3::legacy_traffic::TrafficMessageError::Relay(
                    crate::l3::relay::RelayError::Length,
                )
                | crate::l3::legacy_traffic::TrafficMessageError::OptionalLength,
            ) => tonic::Status::invalid_argument("Message does not fit a single SMS relay"),
            _ => tonic::Status::internal(error.to_string()),
        })?;
    tracing::info!(sms_id = id.value(), "operator SMS queued");
    Ok(proto::CommandResult {
        sms_id: Some(id.value()),
        message: "Queued for delivery".into(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests;

fn audit_subscriber(
    idle: &mut crate::runtime::idle_queries::IdleQueries,
    sms: &SmsRuntime,
    voice: &VoiceRuntime,
    request: proto::AuditSubscriberRequest,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    use crate::runtime::idle_queries::PollError;
    let identity = subscriber_identity(
        request
            .subscriber_id
            .ok_or_else(|| tonic::Status::invalid_argument("Subscriber identity is required"))?,
    )?;
    let order = crate::l3::audit::AuditOrder {
        force_registration: request.force_registration,
        debug_display_allowed: request.debug_display_allowed,
        ..Default::default()
    };
    idle.request_audit(
        sms.core().registry(),
        identity,
        order,
        voice.subscriber_busy(identity) || sms.subscriber_busy(identity),
        now_ms,
    )
    .map_err(|error| match error {
        PollError::UnknownSubscriber => tonic::Status::not_found(error.to_string()),
        PollError::Full => tonic::Status::resource_exhausted(error.to_string()),
        _ => tonic::Status::failed_precondition(error.to_string()),
    })?;
    Ok(proto::CommandResult {
        message: "Audit order queued".into(),
        ..Default::default()
    })
}
fn request_capabilities(
    idle: &mut crate::runtime::idle_queries::IdleQueries,
    sms: &SmsRuntime,
    voice: &VoiceRuntime,
    request: proto::SubscriberIdentity,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    use crate::runtime::idle_queries::PollError;
    let identity = subscriber_identity(request)?;
    idle.request(
        sms.core().registry(),
        identity,
        voice.subscriber_busy(identity) || sms.subscriber_busy(identity),
        now_ms,
    )
    .map_err(|error| match error {
        PollError::UnknownSubscriber => tonic::Status::not_found(error.to_string()),
        PollError::Full => tonic::Status::resource_exhausted(error.to_string()),
        _ => tonic::Status::failed_precondition(error.to_string()),
    })?;
    Ok(proto::CommandResult {
        message: "Capability request queued".into(),
        ..Default::default()
    })
}
pub(super) fn subscriber_identity(
    id: proto::SubscriberIdentity,
) -> Result<crate::link::rach::MobileIdentity, tonic::Status> {
    use crate::link::rach::MobileIdentity;
    if id.esn.is_some() {
        return Err(tonic::Status::invalid_argument(
            "ESN is a provisioning key, not an air-interface address",
        ));
    }
    let identity = match id.kind {
        0 => MobileIdentity::Tmsi20(
            u32::try_from(id.value).map_err(|_| tonic::Status::invalid_argument("TMSI range"))?,
        ),
        1 => MobileIdentity::Tmsi24(
            u32::try_from(id.value).map_err(|_| tonic::Status::invalid_argument("TMSI range"))?,
        ),
        2 => MobileIdentity::Min34(id.value),
        3 => MobileIdentity::Imsi50(id.value),
        _ => {
            return Err(tonic::Status::invalid_argument(
                "Unknown subscriber identity kind",
            ));
        }
    };
    let (_, width, value) = identity.parts();
    if value == 0 || value >= 1 << width {
        return Err(tonic::Status::invalid_argument(
            "Subscriber identity is outside its wire range",
        ));
    }
    Ok(identity)
}
pub(super) fn check_subscriber_busy(
    sms: &SmsRuntime,
    voice: &VoiceRuntime,
    identity: crate::link::rach::MobileIdentity,
) -> Result<(), tonic::Status> {
    if voice.subscriber_busy(identity) || sms.subscriber_busy(identity) {
        return Err(tonic::Status::failed_precondition(
            "Wait for the SMS procedure or release the active call before editing this subscriber",
        ));
    }
    Ok(())
}
fn save_subscriber(
    sms: &mut SmsRuntime,
    voice: &VoiceRuntime,
    request: proto::SaveSubscriberRequest,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    use crate::{
        core::registry::ProvisionedSubscriber,
        l3::address::{AddressInfo, AddressValue},
    };
    AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd(request.number.clone()),
    }
    .encode()
    .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
    if let Some(esn) = request.esn {
        return super::device_subscribers::save(sms, voice, request, esn, now_ms);
    }
    let identity = subscriber_identity(
        request
            .subscriber_id
            .ok_or_else(|| tonic::Status::invalid_argument("Subscriber identity is required"))?,
    )?;
    check_subscriber_busy(sms, voice, identity)?;
    sms.core_mut()
        .save_subscriber(ProvisionedSubscriber {
            identity,
            number: request.number,
        })
        .map_err(|error| match error {
            crate::core::messaging::MessagingError::Registry(
                crate::core::registry::RegistryError::DuplicateNumber,
            ) => tonic::Status::already_exists(error.to_string()),
            crate::core::messaging::MessagingError::DeviceProvisioning(_)
            | crate::core::messaging::MessagingError::Registry(_) => {
                tonic::Status::invalid_argument(error.to_string())
            }
            _ => tonic::Status::internal(error.to_string()),
        })?;
    sms.sync_subscribers(now_ms)
        .map_err(|error| tonic::Status::internal(error.to_string()))?;
    tracing::info!(?identity, "subscriber saved");
    Ok(proto::CommandResult {
        message: "Subscriber saved".into(),
        ..Default::default()
    })
}
fn delete_subscriber(
    sms: &mut SmsRuntime,
    voice: &VoiceRuntime,
    request: proto::SubscriberIdentity,
) -> Result<proto::CommandResult, tonic::Status> {
    if let Some(esn) = request.esn {
        if request.kind != 0 || request.value != 0 {
            return Err(tonic::Status::invalid_argument(
                "Select either an ESN or a mobile identity for deletion",
            ));
        }
        return super::device_subscribers::delete(sms, voice, esn);
    }
    let identity = subscriber_identity(request)?;
    check_subscriber_busy(sms, voice, identity)?;
    sms.core_mut()
        .delete_subscriber(identity)
        .map_err(|error| match error {
            crate::core::messaging::MessagingError::Store(
                crate::core::store::StoreError::MissingSubscriber,
            ) => tonic::Status::not_found(error.to_string()),
            _ => tonic::Status::internal(error.to_string()),
        })?;
    tracing::info!(?identity, "subscriber removed");
    Ok(proto::CommandResult {
        message: "Subscriber removed. Message history and queued SMS retained".into(),
        ..Default::default()
    })
}

fn set_measurements(
    voice: &mut VoiceRuntime,
    request: proto::SetMeasurementsRequest,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    let party = match proto::CallParty::try_from(request.party)
        .map_err(|_| tonic::Status::invalid_argument("Unknown call party"))?
    {
        proto::CallParty::Caller => crate::core::call_session::Party::Caller,
        proto::CallParty::Callee => crate::core::call_session::Party::Callee,
    };
    if !voice.has_active_call(request.call_id) {
        return Err(tonic::Status::not_found("Call is no longer active"));
    }
    super::extended_measurements::validate(&request)?;
    if let Some(extended) = request.extended {
        let targets = super::extended_measurements::targets(&extended)?;
        voice
            .request_extended_measurements(request.call_id, party, targets, now_ms)
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
    } else {
        voice
            .set_measurements(request.call_id, party, request.enabled, now_ms)
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
    }
    Ok(proto::CommandResult {
        message: "Measurement reporting requested".into(),
        ..Default::default()
    })
}

fn request_status(
    voice: &mut VoiceRuntime,
    request: proto::RequestStatusRequest,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    let party = match proto::CallParty::try_from(request.party)
        .map_err(|_| tonic::Status::invalid_argument("Unknown call party"))?
    {
        proto::CallParty::Caller => crate::core::call_session::Party::Caller,
        proto::CallParty::Callee => crate::core::call_session::Party::Callee,
    };
    if !voice.has_active_call(request.call_id) {
        return Err(tonic::Status::not_found("Call is no longer active"));
    }
    voice
        .request_status(request.call_id, party, now_ms)
        .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
    Ok(proto::CommandResult {
        message: "Phone status requested".into(),
        ..Default::default()
    })
}

fn set_physical_control(
    voice: &mut VoiceRuntime,
    request: proto::SetPhysicalControlRequest,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    if request.automatic_timing == Some(true) && request.timing_advance_half_symbols.is_some() {
        return Err(tonic::Status::invalid_argument(
            "Automatic timing and an explicit timing advance cannot be combined",
        ));
    }
    let party = match proto::CallParty::try_from(request.party)
        .map_err(|_| tonic::Status::invalid_argument("Unknown call party"))?
    {
        proto::CallParty::Caller => crate::core::call_session::Party::Caller,
        proto::CallParty::Callee => crate::core::call_session::Party::Callee,
    };
    if !voice.has_active_call(request.call_id) {
        return Err(tonic::Status::not_found("Call is no longer active"));
    }
    let power_level = request
        .mobile_power_level
        .map(u8::try_from)
        .transpose()
        .map_err(|_| tonic::Status::invalid_argument("Invalid mobile power level"))?;
    let mut time_alignment = request
        .timing_advance_half_symbols
        .map(u8::try_from)
        .transpose()
        .map_err(|_| tonic::Status::invalid_argument("Invalid timing advance"))?
        .map(crate::l3::designation::TimeAlignment::OffsetUnits);
    let timing_mode = request.automatic_timing.map(|automatic| {
        if automatic {
            crate::runtime::voice::TimingMode::Automatic
        } else {
            crate::runtime::voice::TimingMode::Manual
        }
    });
    // A timed-out manual command can leave the phone at an unconfirmed offset.
    if timing_mode.is_some() && time_alignment.is_none() {
        time_alignment = Some(crate::l3::designation::TimeAlignment::Maintain);
    }
    let control = crate::l3::legacy_traffic::PhysicalControlRequest {
        power_level,
        time_alignment,
        ..Default::default()
    };
    control
        .validate()
        .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
    voice
        .set_physical_control(request.call_id, party, control, timing_mode, now_ms)
        .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
    Ok(proto::CommandResult {
        message: "Physical controls requested. Confirmed values follow the phone acknowledgment"
            .into(),
        ..Default::default()
    })
}

pub(super) fn message_waiting_fields(
    request: &proto::SendMessageWaitingRequest,
) -> Result<(MobileIdentity, crate::l3::message_waiting::MessageWaiting), tonic::Status> {
    let identity = subscriber_identity(
        request
            .subscriber_id
            .ok_or_else(|| tonic::Status::invalid_argument("Subscriber identity is required"))?,
    )?;
    const MAX_WAITING_VALUES: usize = 16;
    if request.messages.is_empty() || request.messages.len() > MAX_WAITING_VALUES {
        return Err(tonic::Status::invalid_argument(
            "Supply 1–16 distinct message types",
        ));
    }
    let message = crate::l3::message_waiting::MessageWaiting {
        entries: request
            .messages
            .iter()
            .map(|message| {
                Ok(crate::l3::message_waiting::Waiting {
                    kind: u8::try_from(message.kind).map_err(|_| {
                        tonic::Status::invalid_argument("Message kind is out of range")
                    })?,
                    count: u8::try_from(message.count).map_err(|_| {
                        tonic::Status::invalid_argument("Message count is out of range")
                    })?,
                })
            })
            .collect::<Result<Vec<_>, tonic::Status>>()?,
        display: request.display.clone(),
        ..Default::default()
    };
    message
        .encode()
        .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
    Ok((identity, message))
}
fn send_message_waiting(
    idle: &mut crate::runtime::idle_queries::IdleQueries,
    sms: &SmsRuntime,
    voice: &VoiceRuntime,
    request: proto::SendMessageWaitingRequest,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    use crate::runtime::idle_queries::PollError;
    let (identity, message) = message_waiting_fields(&request)?;
    idle.request_message_waiting(
        sms.core().registry(),
        identity,
        message,
        voice.subscriber_busy(identity) || sms.subscriber_busy(identity),
        now_ms,
    )
    .map_err(|error| match error {
        PollError::UnknownSubscriber => tonic::Status::not_found(error.to_string()),
        PollError::Full => tonic::Status::resource_exhausted(error.to_string()),
        _ => tonic::Status::failed_precondition(error.to_string()),
    })?;
    Ok(proto::CommandResult {
        message: "Message Waiting queued. Handset confirmation is reported in subscriber details"
            .into(),
        ..Default::default()
    })
}

#[cfg(test)]
mod codec_tests {
    use super::*;
    #[test]
    fn startup_shows_supported_codecs_and_optional_uhd_master_clock() {
        let mut config = Config::from_yaml(include_str!("../../../../config/uhd.yaml")).unwrap();
        let snapshot = initial(&config);
        assert!(
            snapshot
                .carrier
                .iter()
                .any(|m| m.name == "Speech codecs"
                    && m.value.as_deref() == Some("VSELP / ACELP CC1"))
        );
        assert!(
            snapshot
                .carrier
                .iter()
                .any(|m| m.name == "Master clock requested"
                    && m.value.as_deref() == Some("24883200"))
        );
        if let sdr_hal::config::DeviceConfig::Uhd {
            master_clock_rate, ..
        } = &mut config.radio.device
        {
            *master_clock_rate = None;
        } else {
            panic!("UHD example expected");
        }
        let snapshot = initial(&config);
        assert!(
            snapshot
                .carrier
                .iter()
                .any(|m| m.name == "Master clock requested"
                    && m.value.as_deref() == Some("Automatic")
                    && m.unit.is_empty())
        );
    }
}

#[cfg(test)]
mod radio_history_tests {
    use super::*;

    fn sample(now_ms: u64, missed_slots: u64) -> RadioSample {
        RadioSample {
            now_ms,
            missed_slots,
            underflows: 0,
        }
    }

    #[test]
    fn recent_faults_drop_out_after_the_window() {
        let mut history = RadioHistory::default();
        assert_eq!(history.recent(sample(0, 5)).missed_slots, 0);
        assert_eq!(history.recent(sample(30_000, 8)).missed_slots, 3);
        assert_eq!(history.recent(sample(90_000, 8)).missed_slots, 0);
        assert_eq!(history.recent(sample(100_000, 9)).missed_slots, 1);
    }
}
fn registration_period(superframes: u32) -> String {
    let seconds = u64::from(superframes) * crate::runtime::timers::SUPERFRAME_MS / 1000;
    let time = match (seconds / 3600, seconds % 3600 / 60, seconds % 60) {
        (0, 0, s) => format!("{s} s"),
        (0, m, 0) => format!("{m} min"),
        (0, m, s) => format!("{m} min {s} s"),
        (h, 0, _) => format!("{h} h"),
        (h, m, _) => format!("{h} h {m} min"),
    };
    format!("{superframes} superframes ({time})")
}
