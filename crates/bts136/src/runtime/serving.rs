#[cfg(test)]
use crate::l3::broadcast::ProtocolVersion;
mod diagnostics;
mod protocol_clock;
mod registration_arq;
use super::{access_channels::AccessChannels, registration::ReplyQueue};
use crate::{
    config::Config,
    core::messaging::LocalMessaging,
    l3::{
        broadcast::{
            AccessParameters, DcchStructure, MandatoryBroadcast, RegistrationParameters,
            SelectionParameters, SlotConfiguration, SystemIdentity,
        },
        designation::AssignedSlots,
    },
    link::rach::Outcome,
    phy::{
        acquisition::reverse_stream::ReverseControlStream,
        modulation::Symbol,
        pulse::SYMBOL_RATE_HZ,
        rach::BurstFormat,
        sync::SyncWord,
        traffic::{self, DATA_BITS, SACCH_BITS, SLOT_BITS},
    },
    radio::{
        PreparedRadio,
        carrier::Carrier,
        duplex::Duplex,
        receiver::{self, ReadOutcome},
        timed_tx,
    },
    scheduler::broadcast::{BroadcastClock, PHYSICAL_SLOTS, Position, Superframe},
};
use num_complex::Complex32;
use registration_arq::Backlog;
use std::{
    error::Error,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
// Pulse-tail completion can retain one slot after its samples finish.
pub(crate) const QUEUED_SLOTS: usize = 5;
/// Store requests from other threads served per loop pass. Each runs against the
/// in-memory state, so this bounds the work the TX refill waits behind.
const STORE_REQUESTS_PER_PASS: usize = 4;
const STORE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REPLIES: usize = 32;
const MAX_BURSTS: usize = 8;
const START_LEAD_MS: u64 = 100;
const SLOT_SUBMISSION_LEAD_US: u64 = 5000;
// Device clock reads are USB register reads on a USRP, costing up to a few milliseconds.
const HARDWARE_TIME_REFRESH: Duration = Duration::from_millis(500);
const TX_CLOCK_SETTLE: Duration = Duration::from_millis(100);
const MICROSECONDS_PER_SECOND: u64 = 1000000;
const MILLISECONDS_PER_SECOND: u64 = 1000;
const SYMBOLS_PER_SLOT: usize = SLOT_BITS / 2;
const PUBLIC_NETWORK: u8 = 0b100;
const FBCCH_SLOTS: u8 = 4;
// Short sample read timeouts break bladeRF RX streams.
const RX_TIMEOUT_US: i64 = 250_000;
const IDLE_WAIT_US: u64 = 100;
// TIA-136-123-E Table 14 allows one superframe to prepare an R-DATA response.
const MAX_RX_AGE_MS: u64 = super::timers::SUPERFRAME_MS;
const CONTROL_SLOT_PERIOD: u64 = 3;
const UNASSIGNED_COLOR_MASK: u8 = u8::MAX;
macro_rules! retry_late_tx {
    ($slot:expr, $result:expr) => {
        match $result {
            Ok(value) => value,
            Err(error) if error.is_late() => {
                tracing::warn!(
                    physical_slot = $slot,
                    "TX slot became late before submission. Retrying on the existing timeline"
                );
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    };
}
#[derive(Debug, Clone)]
enum ArqOwner {
    Registration(crate::core::registry::RegistrationReply),
    Sms(super::sms::ArqMessage),
}
enum Completion {
    Idle,
    Arq(super::spach_arq::PreparedFrame),
    Query(super::idle_queries::Completion),
    Registration(crate::core::registry::RegistrationReply),
    Sms(super::sms::Completion),
    Voice(super::voice::Completion),
}
impl From<Option<crate::core::registry::RegistrationReply>> for Completion {
    fn from(value: Option<crate::core::registry::RegistrationReply>) -> Self {
        value.map_or(Self::Idle, Self::Registration)
    }
}
fn unix_ms_at(origin_unix_ms: i64, protocol_ms: u64) -> Result<i64, Box<dyn Error>> {
    origin_unix_ms
        .checked_add(i64::try_from(protocol_ms)?)
        .ok_or_else(|| "protocol time exceeds the Unix time range".into())
}
fn queue_registration_reply(
    replies: &mut ReplyQueue,
    arq: &mut super::spach_arq::Sender<ArqOwner>,
    backlog: &mut Backlog,
    reply: crate::core::registry::RegistrationReply,
    now_ms: u64,
) -> Result<(), Box<dyn Error>> {
    let bytes = reply.encode()?;
    let frames = crate::link::spach::SingleMessage {
        channel: crate::link::spach::Channel::Arch,
        flags: crate::link::spach::HeaderFlags::default(),
        identity: reply.identity,
        message: bytes.clone(),
    }
    .encode_fragmented()?;
    let deadline_ms = now_ms
        .checked_add(super::timers::SPACH_MS)
        .ok_or("reply deadline range")?;
    if frames.len() > 1 {
        backlog.enqueue(super::spach_arq::Request {
            identity: reply.identity,
            channel: crate::link::spach::Channel::Arch,
            message: bytes,
            completion: ArqOwner::Registration(reply.clone()),
            deadline_ms,
        })?;
        arq.cancel(reply.identity);
        replies.cancel_queued(reply.identity);
    } else {
        replies.enqueue_replacing(reply.clone(), deadline_ms)?;
        backlog.cancel(reply.identity);
        arq.cancel(reply.identity);
    }
    Ok(())
}
fn prepare_arq(
    sender: &mut super::spach_arq::Sender<ArqOwner>,
    arbiter: &super::spach_mux::Arbiter,
    repeats: usize,
    block: u64,
    now_ms: u64,
) -> Result<Option<super::spach_arq::PreparedFrame>, super::spach_arq::Error> {
    let mut frame = if !arbiter.can_interrupt(1, repeats)
        && let Some(echo) = arbiter.oldest()
    {
        sender.prepare_prioritized(echo, block, now_ms)?
    } else {
        sender.prepare(block, now_ms)?
    };
    if frame
        .as_ref()
        .is_some_and(|frame| !arbiter.contains(frame.partial_echo()))
        && !arbiter.can_begin(repeats)
    {
        frame = if let Some(echo) = arbiter.oldest() {
            sender.prepare_prioritized(echo, block, now_ms)?
        } else {
            None
        };
    }
    Ok(frame)
}
fn drain_arq_events(
    arq: &mut super::spach_arq::Sender<ArqOwner>,
    protocol: Option<&crate::operations::protocol::ProtocolLog>,
    completed_registrations: &mut u64,
    sms: &mut super::sms::SmsRuntime,
    now_ms: u64,
) -> Result<(), Box<dyn Error>> {
    use super::spach_arq::Event;
    while let Some(event) = arq.pop_event() {
        match event {
            Event::Complete(ArqOwner::Sms(message)) => {
                sms.arq_ended(message, super::sms::ArqOutcome::Confirmed, now_ms)?;
            }
            Event::Expired(ArqOwner::Sms(message)) => {
                sms.arq_ended(message, super::sms::ArqOutcome::Expired, now_ms)?;
            }
            Event::Cancelled(ArqOwner::Sms(message)) => {
                sms.arq_ended(message, super::sms::ArqOutcome::Cancelled, now_ms)?;
            }
            Event::Failed {
                completion: ArqOwner::Sms(message),
                error,
            } => {
                tracing::warn!(identity=%message.identity,%error,"SMS ARQ failed");
                sms.arq_ended(message, super::sms::ArqOutcome::Failed, now_ms)?;
            }
            Event::Complete(ArqOwner::Registration(reply)) => {
                *completed_registrations += 1;
                if let Some(protocol) = protocol {
                    protocol.message(
                        "Forward",
                        "ARCH",
                        reply.identity,
                        &reply.encode()?,
                        "TX completed and ARQ status confirmed",
                    );
                }
                tracing::info!(identity=%reply.identity,response=?reply.message,"registration reply acknowledged by ARQ");
            }
            Event::Expired(ArqOwner::Registration(reply)) => {
                if let Some(protocol) = protocol {
                    protocol.arq_outcome(reply.identity, "Expired without ARQ confirmation");
                }
                tracing::warn!(identity=%reply.identity,"ARQ registration reply expired");
            }
            Event::Cancelled(ArqOwner::Registration(reply)) => {
                if let Some(protocol) = protocol {
                    protocol.arq_outcome(reply.identity, "Cancelled");
                }
                tracing::debug!(identity=%reply.identity,"ARQ registration reply cancelled");
            }
            Event::Failed {
                completion: ArqOwner::Registration(reply),
                error,
            } => {
                if let Some(protocol) = protocol {
                    protocol.arq_outcome(reply.identity, "Failed without ARQ confirmation");
                }
                tracing::warn!(identity=%reply.identity,%error,"ARQ registration reply failed");
            }
        }
    }
    Ok(())
}
pub struct Summary {
    pub submitted_slots: u64,
    pub completed_registrations: u64,
    pub stop_acknowledged: bool,
}

pub fn messages(config: &Config) -> [MandatoryBroadcast; 5] {
    [
        MandatoryBroadcast::DcchStructure(DcchStructure {
            fbcch_slots: FBCCH_SLOTS,
            ebcch_slots: 2,
            sbcch_slots: 0,
            reserved_slots: 0,
            hyperframe_counter: 0,
            secondary_superframe: false,
            slot_configuration: SlotConfiguration::OneFullRate,
            dvcc: config.cell.dvcc,
            max_supported_pfc: 1,
            pch_displacement: 0,
            pfm_direction: false,
            non_pch_subchannel_code: 0,
        }),
        MandatoryBroadcast::AccessParameters(AccessParameters {
            auth: false,
            s: true,
            rand: 0,
            ms_acc_pwr: config.cell.access_power_level,
            abbreviated_burst: false,
            max_retries_code: 3,
            max_busy_reserved_code: false,
            max_repetitions: 2,
            max_stop_counter_code: false,
            rdata_message_length_code: 7,
            cell_barred: 0,
            subaddressing_support: false,
            delay_compensation_mode: false,
        }),
        MandatoryBroadcast::SelectionParameters(SelectionParameters {
            ss_suff_code: 0,
            rss_acc_min_code: 0,
            scan_interval_code: 0,
            initial_selection_control: false,
            delay_code: 0,
            scanning_option: false,
        }),
        MandatoryBroadcast::RegistrationParameters(RegistrationParameters {
            present_rnum: config.registration.area,
            period_code: config.registration.period_code(),
            regh: config.registration.home,
            regr: config.registration.roaming,
            pureg: true,
            pdreg: true,
            syreg: true,
            lareg: config.registration.area.is_some(),
            dereg: false,
            foreg: false,
            capability_request: true,
        }),
        MandatoryBroadcast::SystemIdentity(SystemIdentity {
            sid: config.cell.sid,
            network_types: PUBLIC_NETWORK,
            protocol_version: super::capabilities::SERVING_PROTOCOL,
            network_name: config.cell.network_name.clone(),
        }),
    ]
}

pub(crate) fn optional_messages() -> Result<Vec<Vec<u8>>, crate::l3::service_menu::Error> {
    Ok(vec![
        super::capabilities::service_menu().encode(super::capabilities::SERVING_PROTOCOL)?,
    ])
}

fn arq_reuse_policy(
    config: &Config,
) -> Result<super::spach_arq::reuse::ReusePolicy, super::spach_arq::Error> {
    let MandatoryBroadcast::AccessParameters(parameters) = &messages(config)[1] else {
        unreachable!("access parameters are second")
    };
    super::spach_arq::reuse::ReusePolicy::full_rate_contention(parameters)
}
pub fn run(
    config: &Config,
    shutdown: &AtomicBool,
    duration: Option<Duration>,
) -> Result<Summary, Box<dyn Error>> {
    run_with_interface(config, shutdown, duration, None)
}
pub fn run_with_interface(
    config: &Config,
    shutdown: &AtomicBool,
    duration: Option<Duration>,
    interface: Option<crate::operations::RuntimeInterface>,
) -> Result<Summary, Box<dyn Error>> {
    let configured = config.prepare()?;
    tracing::info!(
        band = ?config.cell.band,
        channel = config.cell.channel,
        control_slot = config.cell.control_slot,
        sid = config.cell.sid,
        dvcc = config.cell.dvcc,
        tx_frequency_hz = configured.frequencies.forward_hz,
        rx_frequency_hz = configured.frequencies.reverse_hz,
        tx_sample_rate_hz = config.sample_rate_hz(),
        rx_sample_rate_hz = config.sample_rate_hz(),
        "cell configuration"
    );
    if config.cell.control_slot != 1 {
        return Err("serving runtime requires control slots 1/4".into());
    }
    if duration.is_some_and(|duration| duration.is_zero()) {
        return Err("run duration must be positive".into());
    }
    let prepared = PreparedRadio::open(config)?;
    serve(
        config,
        prepared,
        shutdown,
        duration,
        interface,
        HARDWARE_TIME_REFRESH,
    )
}
pub fn run_prepared(
    config: &Config,
    prepared: PreparedRadio,
    shutdown: &AtomicBool,
    duration: Option<Duration>,
) -> Result<Summary, Box<dyn Error>> {
    run_prepared_with_interface(config, prepared, shutdown, duration, None)
}
pub fn run_prepared_with_interface(
    config: &Config,
    prepared: PreparedRadio,
    shutdown: &AtomicBool,
    duration: Option<Duration>,
    interface: Option<crate::operations::RuntimeInterface>,
) -> Result<Summary, Box<dyn Error>> {
    serve(
        config,
        prepared,
        shutdown,
        duration,
        interface,
        Duration::ZERO,
    )
}
fn serve(
    config: &Config,
    prepared: PreparedRadio,
    shutdown: &AtomicBool,
    duration: Option<Duration>,
    mut interface: Option<crate::operations::RuntimeInterface>,
    hardware_time_refresh: Duration,
) -> Result<Summary, Box<dyn Error>> {
    config.prepare()?;
    if duration.is_some_and(|duration| duration.is_zero()) {
        return Err("run duration must be positive".into());
    }
    let mut core = LocalMessaging::from_store(config.open_replicated_store()?)?;
    core.set_allow_unknown_registration(config.registration.allow_unknown);
    core.enable_sms_hub(config.sms_hub.enabled);
    core.set_display_time(config.sms.display_time.wire());
    let (store_client, store_requests) = crate::core::store::requests::StoreRequests::channel();
    let sms_hub = crate::sms_hub::Gateway::start(config.sms_hub.clone(), store_client)?;
    // Declared after the hub so it drops first. The hub joins its worker on drop, and a
    // worker waiting on a store reply only returns once these requests are gone.
    let mut store_requests = store_requests;
    core.set_sms_hub_status(sms_hub.status.clone());
    let settings = prepared.settings();
    let mut receive_window = crate::radio::receive_window::ReceiveWindow::try_new(
        settings.tx_tick_rate_hz,
        settings.rx_tick_rate_hz,
        config.radio.device.rx_to_tx_delay_ns().unwrap_or(0),
        MAX_RX_AGE_MS,
    )?;
    let samples_per_symbol = settings.applied_tx_sample_rate_hz / SYMBOL_RATE_HZ as usize;
    let mut traffic_reverse = crate::phy::acquisition::reverse_stream::ReverseStream::try_new(
        settings.applied_rx_sample_rate_hz as f64,
        MAX_BURSTS,
    )?;
    let mut reverse = ReverseControlStream::try_new_control(
        settings.applied_rx_sample_rate_hz as f64,
        MAX_BURSTS,
        BurstFormat::Normal,
        config.cell.dvcc,
    )?;
    let mut streams = prepared.into_streams(
        timed_tx::Limits {
            max_queued_blocks: QUEUED_SLOTS,
            max_block_samples: SYMBOLS_PER_SLOT * samples_per_symbol,
        },
        receiver::Limits {
            max_block_samples: reverse.max_read_samples(),
            timeout_us: RX_TIMEOUT_US,
        },
    )?;
    let sip_gateway = if config.sip.enabled {
        Some(crate::sip::Gateway::start(config.sip.clone())?)
    } else {
        None
    };
    streams.tx.set_hardware_time_refresh(hardware_time_refresh);
    let observation = if hardware_time_refresh.is_zero() {
        streams.tx.prepared_observation()?
    } else {
        streams.rx.start(None)?;
        let rx = &streams.rx;
        streams.tx.enable_and_observe(TX_CLOCK_SETTLE, &mut || {
            if let Err(error) = rx.hardware_time() {
                tracing::debug!(%error, "RX clock refresh failed");
            }
        })?
    };
    let wall_unix_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    // Run time advances with the radio clock from this origin, so a wall-clock step
    // during the run cannot move it behind timestamps the store already holds.
    let protocol_unix_origin_ms = match core.store().latest_unix_ms()? {
        Some(latest) if latest >= wall_unix_ms => {
            tracing::warn!(
                stored_unix_ms = latest,
                wall_unix_ms,
                "system clock is behind stored state, continuing from the stored time"
            );
            latest.saturating_add(1)
        }
        _ => wall_unix_ms,
    };
    let protocol_clock = protocol_clock::ProtocolClock::try_new(
        observation.hardware_tick,
        settings.tx_tick_rate_hz,
    )?;
    let lead = settings
        .tx_tick_rate_hz
        .checked_mul(START_LEAD_MS)
        .ok_or("TX clock range")?
        / MILLISECONDS_PER_SECOND;
    let epoch = observation
        .hardware_tick
        .checked_add(lead)
        .ok_or("TX epoch range")?;
    let mut duplex = Duplex::try_new(streams, samples_per_symbol)?;
    duplex.buffer_receive(config.realtime);
    let mut carrier = Carrier::try_new(duplex, QUEUED_SLOTS)?;
    let mut replies = ReplyQueue::try_new(MAX_REPLIES)?;
    let policy = arq_reuse_policy(config)?;
    let mut arq =
        super::spach_arq::Sender::after_restart(super::spach_arq::REFRESH_CAPACITY, policy)?;
    let mut backlog = Backlog::default();
    let mut arbiter = super::spach_mux::Arbiter::default();
    tracing::info!(
        arq_quiet_ms = policy.quiet_ms,
        "ARQ PEA reuse protection active after startup"
    );
    let clock = BroadcastClock::try_new(Position {
        hyperframe_counter: 0,
        superframe: Superframe::Primary,
    })?;
    let mut optional = optional_messages()?;
    if let Some(operator) = &config.cell.operator {
        optional.push(operator.message().encode()?.to_vec());
    }
    if let Some(plan) = config.maca_plan()? {
        optional.extend_from_slice(plan.messages());
    }
    let initial_time = match config.time.source {
        crate::config::TimeSource::LocalSystem => super::system_time::sample(0)?,
    };
    tracing::info!(rx_to_tx_delay_ns=config.radio.device.rx_to_tx_delay_ns().unwrap_or(0),
        sms_display_time=?config.sms.display_time,
        "Receive timing and operator SMS display configured");
    tracing::info!(time_source="local_system", utc_offset_minutes=initial_time.time_zone.minutes(), daylight_saving=initial_time.time_zone.daylight_saving(), network_name=?config.cell.network_name,"Broadcast clock and network display configured");
    let mut desired = crate::scheduler::broadcast::BroadcastContent::try_new(
        AssignedSlots::Full14,
        messages(config),
        optional,
    )?
    .with_time_zone(initial_time.time_zone)?;
    if !config.cell.neighbors.is_empty() {
        desired = desired.with_neighbors(&config.cell.neighbors)?;
    }
    let mut broadcast_session = super::broadcast_session::BroadcastSession::try_new(
        core.store_mut(),
        crate::core::store::broadcast::CellKey::try_new(
            config.cell.band.band(),
            config.cell.channel,
        )?,
        clock,
        desired,
        protocol_unix_origin_ms,
        protocol_clock.milliseconds(epoch)?,
    )?;
    let maca = super::maca::MacaRuntime::try_new(
        core.store(),
        crate::core::store::broadcast::CellKey::try_new(
            config.cell.band.band(),
            config.cell.channel,
        )?,
        config.maca_serving(),
        broadcast_session.desired(),
    )?;
    let messages = broadcast_session.desired().messages();
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) = &messages[0] else {
        unreachable!("broadcast structure is first")
    };
    let allocation = crate::scheduler::paging::PagingAllocation::try_new(structure)?;
    let session_key = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let mut sms = super::sms::SmsRuntime::try_new(core, allocation, session_key)?;
    let MandatoryBroadcast::AccessParameters(parameters) = &messages[1] else {
        unreachable!("access parameters are second")
    };
    sms.set_access_parameters(parameters, MAX_RX_AGE_MS)?;
    let mut access = AccessChannels::try_new(parameters.max_repetitions)?;
    sms.start_incoming_sms(0, protocol_unix_origin_ms)?;
    sms.start_outgoing_sms(0)?;
    let mut voice = super::voice::VoiceRuntime::try_new(
        config.prepare()?.traffic,
        allocation,
        config.cell.dvcc,
        config.cell.traffic_power_level,
        &config.calls,
    )?;
    voice.set_sip_gateway(sip_gateway, config.sip.generate_ringback);
    voice.set_return_channel(crate::l3::legacy_traffic::ControlChannel {
        hyperband: match config.cell.band {
            crate::config::ConfigBand::Cellular800 => {
                crate::l3::legacy_traffic::Hyperband::Cellular800
            }
            crate::config::ConfigBand::Pcs1900 => crate::l3::legacy_traffic::Hyperband::Pcs1900,
        },
        channel: config.cell.channel,
        dvcc: config.cell.dvcc,
    })?;
    let mut idle = super::idle_queries::IdleQueries::after_restart(allocation, policy);
    if let Some(interface) = interface.as_ref() {
        let MandatoryBroadcast::DcchStructure(active) = &broadcast_session.content(0).messages()[0]
        else {
            unreachable!("broadcast structure is first")
        };
        carrier.set_observer(interface.hub.protocol.observer(
            active.dvcc,
            active.fbcch_slots,
            active.ebcch_slots,
        ));
        voice.set_protocol_log(interface.hub.protocol.clone());
        sms.set_protocol_log(interface.hub.protocol.clone());
    }

    let mut snapshot = broadcast_session.snapshot(0)?;
    let mut snapshot_version = broadcast_session.version();
    let mut observed_revision = broadcast_session.revision(0).clone();
    let MandatoryBroadcast::DcchStructure(active) = &broadcast_session.content(0).messages()[0]
    else {
        unreachable!("broadcast structure is first")
    };
    let mut active_dvcc = active.dvcc;
    if let Some(offset) = broadcast_session.content(0).time_zone() {
        let mut time = super::system_time::for_superframe(
            protocol_clock.milliseconds(epoch)?,
            0,
            active.fbcch_slots + active.ebcch_slots - 1,
            0,
        )?;
        time.time_zone = offset;
        snapshot.set_time_date(time)?;
    }
    let mut iq = vec![Complex32::new(0.0, 0.0); reverse.max_read_samples()];
    let mut symbols = vec![Symbol { i: 0.0, q: 0.0 }; iq.len()];
    let mut bursts = Vec::with_capacity(MAX_BURSTS);
    let mut shortened_reverse =
        crate::phy::acquisition::reverse_stream::ReverseStream::<
            crate::phy::shortened::ShortenedBurst,
        >::try_new_shortened(settings.applied_rx_sample_rate_hz as f64, MAX_BURSTS)?;
    let mut shortened_bursts = Vec::with_capacity(MAX_BURSTS);
    let mut traffic_bursts = Vec::with_capacity(MAX_BURSTS);
    tracing::info!(
        queued_slots = QUEUED_SLOTS,
        samples_per_slot = SYMBOLS_PER_SLOT * samples_per_symbol,
        queued_duration_us =
            QUEUED_SLOTS as u64 * SYMBOLS_PER_SLOT as u64 * MICROSECONDS_PER_SECOND
                / SYMBOL_RATE_HZ as u64,
        startup_lead_ms = START_LEAD_MS,
        submission_margin_us = SLOT_SUBMISSION_LEAD_US,
        rx_backend_timeout_us = RX_TIMEOUT_US,
        rx_worker_buffers = 1,
        rx_poll_idle_us = IDLE_WAIT_US,
        "TX scheduling configured"
    );
    let mut receipts = Vec::with_capacity(QUEUED_SLOTS);
    let mut completed: Vec<crate::radio::carrier::Completed<Completion>> =
        Vec::with_capacity(QUEUED_SLOTS);
    let submission_lead_ticks = settings
        .tx_tick_rate_hz
        .checked_mul(SLOT_SUBMISSION_LEAD_US)
        .ok_or("submission lead range")?
        / MICROSECONDS_PER_SECOND;
    let mut snapshot_superframe = 0;
    let mut reachability_superframe = 0;
    let mut submitted: u64 = 0;
    let mut completed_registrations = 0;
    carrier.start_continuous(epoch, None)?;
    tracing::info!(
        band = ?config.cell.band,
        channel = config.cell.channel,
        control_slot = config.cell.control_slot,
        sid = config.cell.sid,
        dvcc = config.cell.dvcc,
        forward_hz = config.prepare()?.frequencies.forward_hz,
        reverse_hz = config.prepare()?.frequencies.reverse_hz,
        authentication = false,
        "cell started"
    );
    let began = Instant::now();
    let mut last_observation = observation;
    let mut next_publication_ms = 0;
    let mut skipped_slots = 0;
    let mut rx_discontinuities = 0;
    let mut access_burst_count = 0;
    let mut traffic_burst_count = 0;
    let mut rx_power_dbfs = None;
    let mut last_frequency_offset_hz = None;
    let mut last_sync_correlation = None;
    let mut tx_diagnostics = diagnostics::Diagnostics::new();
    let mut received = ReadOutcome::Timeout {
        hardware_overflow: false,
    };
    let _scheduling = crate::realtime::apply(config.realtime, crate::realtime::Role::Tx);
    let result = (|| -> Result<(), Box<dyn Error>> {
        while !shutdown.load(Ordering::Relaxed)
            && duration.is_none_or(|limit| began.elapsed() < limit)
        {
            let loop_started = Instant::now();
            let mut timing = tx_diagnostics.begin(loop_started);
            let mut phase_started = loop_started;
            receipts.clear();
            let observation = carrier.poll_completed(&mut receipts, &mut completed)?;
            if super::broadcast_completion::disrupted(last_observation.health, observation.health) {
                broadcast_session.invalidate_through(submitted.saturating_sub(1) / PHYSICAL_SLOTS);
            }
            last_observation = observation;
            let now_ms = protocol_clock.milliseconds(last_observation.hardware_tick)?;
            let unix_ms = unix_ms_at(protocol_unix_origin_ms, now_ms)?;
            timing.poll_us = diagnostics::micros(phase_started.elapsed());
            phase_started = Instant::now();
            for completion in completed.drain(..) {
                let completed_ms = protocol_clock.milliseconds(completion.scheduled_end_tick)?;
                let allowed_before = broadcast_session.serving_allowed();
                let events = broadcast_session.completed(
                    sms.core_mut().store_mut(),
                    completion.slot_index,
                    now_ms,
                    unix_ms,
                )?;
                let superframe = completion.slot_index / PHYSICAL_SLOTS;
                if events.data_cycle {
                    tracing::info!(superframe, "Complete broadcast cycle observed by host");
                }
                if events.notification_hyperframe {
                    tracing::info!(
                        superframe,
                        "Complete BCCH notification hyperframe observed by host"
                    );
                }
                if events.confirmed {
                    tracing::info!(superframe, "Broadcast revision durably confirmed by host");
                }
                if !allowed_before && broadcast_session.serving_allowed() {
                    access.discontinuity();
                    reverse.discontinuity();
                    shortened_reverse.discontinuity();
                    traffic_reverse.discontinuity();
                    tracing::info!(superframe, "Configured broadcast revision active");
                }
                match completion.completion {
                    Completion::Idle => {}
                    Completion::Arq(frame) => {
                        if let Some(interface) = interface.as_ref() {
                            interface
                                .hub
                                .protocol
                                .arq_frame(frame.identity, &frame.information);
                        }
                        arq.transmitted_with(
                            &frame.token,
                            submitted / CONTROL_SLOT_PERIOD,
                            now_ms,
                            |owner| {
                                if let ArqOwner::Sms(message) = owner
                                    && let Err(error) =
                                        sms.arq_transmitted_at(message, completed_ms, now_ms)
                                {
                                    tracing::warn!(%error,"SMS ARQ physical completion rejected");
                                }
                            },
                        )?;
                    }
                    Completion::Query(completion) => {
                        idle.transmitted(&completion, now_ms)?;
                        if completion.final_frame()
                            && let Some(interface) = interface.as_ref()
                        {
                            interface.hub.protocol.message(
                                "Forward",
                                "PCH",
                                completion.identity,
                                &completion.bytes,
                                "TX completion observed",
                            );
                        }
                    }
                    Completion::Voice(completion) => {
                        if let Err(error) = voice.transmitted_at(completion, completed_ms, now_ms) {
                            tracing::warn!(%error,"call completion rejected");
                        }
                    }
                    Completion::Registration(reply) => {
                        completed_registrations += 1;
                        if let Some(interface) = interface.as_ref() {
                            interface.hub.protocol.message(
                                "Forward",
                                "ARCH",
                                reply.identity,
                                &reply.encode()?,
                                "TX completion observed",
                            );
                        }
                        tracing::info!(identity=%reply.identity,response=?reply.message,"registration reply transmitted");
                    }
                    Completion::Sms(super::sms::Completion::Delivery(prepared)) => {
                        if let Err(error) = sms.transmitted_at(&prepared, completed_ms, now_ms) {
                            tracing::warn!(%error,"SMS completion rejected");
                        }
                    }
                    Completion::Sms(super::sms::Completion::Auxiliary(packet)) => {
                        sms.auxiliary_transmitted(&packet, completed_ms, now_ms)?;
                    }
                    Completion::Sms(super::sms::Completion::Notification {
                        identity,
                        generation,
                    }) => sms.notification_transmitted(identity, generation),
                    Completion::Sms(super::sms::Completion::None) => {}
                    Completion::Sms(super::sms::Completion::Reply(packet)) => {
                        sms.reply_transmitted(&packet, completed_ms, now_ms)?;
                        if let Some(interface) = interface.as_ref() {
                            interface.hub.protocol.message(
                                "Forward",
                                "SMSCH",
                                packet.identity(),
                                packet.bytes(),
                                "TX completion observed",
                            );
                        }
                    }
                    Completion::Sms(super::sms::Completion::Message { identity, bytes }) => {
                        if let Some(interface) = interface.as_ref() {
                            interface.hub.protocol.message(
                                "Forward",
                                "SMSCH",
                                identity,
                                &bytes,
                                "TX completion observed",
                            );
                        }
                    }
                }
            }
            timing.completions_us = diagnostics::micros(phase_started.elapsed());
            phase_started = Instant::now();
            if let Some(interface) = interface.as_mut() {
                interface.process(&mut sms, &mut voice, &mut idle, now_ms);
            }
            voice.advance_sip(sms.core().registry(), now_ms, submitted)?;
            voice.advance(now_ms)?;
            voice.exchange_sms(&mut sms, now_ms, unix_ms)?;
            sms.advance(now_ms)?;
            timing.maintenance_us = diagnostics::micros(phase_started.elapsed());
            phase_started = Instant::now();
            if let ReadOutcome::Block(block) = received {
                use crate::radio::receive_window::Admission;
                let admission = receive_window.admit(&block, last_observation.hardware_tick)?;
                let (accepted, reset) = match admission {
                    Admission::Accept { segment, reset } => {
                        if reset {
                            tracing::warn!(continuity=?block.continuity,"RX discontinuity resets receive contexts");
                        }
                        (Some(segment), reset)
                    }
                    Admission::Discard { age_ms, reset } => {
                        if reset {
                            tracing::warn!(
                                age_ms,
                                maximum_age_ms = MAX_RX_AGE_MS,
                                "Discarding stale RX blocks and resetting receive contexts"
                            );
                        }
                        (None, reset)
                    }
                };
                if reset {
                    rx_discontinuities += 1;
                    rx_power_dbfs = None;
                    last_frequency_offset_hz = None;
                    last_sync_correlation = None;
                    access.discontinuity();
                    reverse.discontinuity();
                    traffic_reverse.discontinuity();
                    shortened_reverse.discontinuity();
                }
                if let Some(rx_segment) = accepted {
                    if interface.is_some() && block.samples_read != 0 {
                        let power = iq[..block.samples_read]
                            .iter()
                            .map(|value| f64::from(value.norm_sqr()))
                            .sum::<f64>()
                            / block.samples_read as f64;
                        rx_power_dbfs = (power > 0.0).then(|| 10.0 * power.log10());
                    }
                    for (symbol, value) in symbols.iter_mut().zip(&iq[..block.samples_read]) {
                        *symbol = Symbol {
                            i: f64::from(value.re),
                            q: f64::from(value.im),
                        };
                    }
                    reverse.push(
                        rx_segment,
                        block.first_sample,
                        &symbols[..block.samples_read],
                        &mut bursts,
                    )?;
                    if voice.needs_initial_alignment() {
                        shortened_reverse.push(
                            rx_segment,
                            block.first_sample,
                            &symbols[..block.samples_read],
                            &mut shortened_bursts,
                        )?;
                        for burst in shortened_bursts.drain(..) {
                            let sample_delta = burst.first_sample - block.first_sample as f64;
                            let rx_tick = block.time_ticks as f64
                                + sample_delta * settings.rx_tick_rate_hz as f64
                                    / settings.applied_rx_sample_rate_hz as f64;
                            let relative_seconds =
                                config.radio.device.rx_to_tx_delay_ns().unwrap_or(0) as f64
                                    / 1_000_000_000.0
                                    + rx_tick / settings.rx_tick_rate_hz as f64
                                    - epoch as f64 / settings.tx_tick_rate_hz as f64;
                            if let Some(interface) = interface.as_ref() {
                                let identity = voice.reverse_identity(
                                    relative_seconds * SYMBOL_RATE_HZ,
                                    burst.acquired.fields.sync,
                                    burst.acquired.fields.color.dvcc,
                                );
                                interface.hub.protocol.quality(identity, &burst.acquired);
                            }
                            if let Err(error) = voice.receive_shortened(
                                relative_seconds * SYMBOL_RATE_HZ,
                                &burst.acquired.fields,
                                now_ms,
                            ) {
                                tracing::debug!(%error, "shortened burst rejected");
                            }
                        }
                    } else {
                        shortened_reverse.discontinuity();
                    }
                    if voice.needs_traffic_reception() {
                        traffic_reverse.push(
                            rx_segment,
                            block.first_sample,
                            &symbols[..block.samples_read],
                            &mut traffic_bursts,
                        )?;
                    } else {
                        traffic_reverse.discontinuity();
                    }
                    traffic_burst_count += traffic_bursts.len() as u64;
                    for burst in traffic_bursts.drain(..) {
                        last_frequency_offset_hz = Some(burst.acquired.frequency_offset_hz);
                        last_sync_correlation = Some(burst.acquired.sync_correlation);
                        let sample_delta = burst.first_sample - block.first_sample as f64;
                        let rx_tick = block.time_ticks as f64
                            + sample_delta * settings.rx_tick_rate_hz as f64
                                / settings.applied_rx_sample_rate_hz as f64;
                        let relative_seconds = config.radio.device.rx_to_tx_delay_ns().unwrap_or(0)
                            as f64
                            / 1_000_000_000.0
                            + rx_tick / settings.rx_tick_rate_hz as f64
                            - epoch as f64 / settings.tx_tick_rate_hz as f64;
                        if let Some(interface) = interface.as_ref() {
                            let identity = voice.reverse_identity(
                                relative_seconds * SYMBOL_RATE_HZ,
                                burst.acquired.fields.sync,
                                burst.acquired.fields.color.dvcc,
                            );
                            interface.hub.protocol.quality(identity, &burst.acquired);
                        }
                        if let Err(error) = voice.receive_traffic(
                            relative_seconds * SYMBOL_RATE_HZ,
                            rx_segment,
                            &burst.acquired.fields,
                            burst.channel_profile,
                            now_ms,
                        ) {
                            tracing::warn!(%error,"reverse traffic rejected");
                        }
                    }
                    access_burst_count += bursts.len() as u64;
                    for burst in bursts.drain(..) {
                        if let Some(interface) = interface.as_ref() {
                            interface.hub.protocol.quality(None, &burst.acquired);
                        }
                        last_frequency_offset_hz = Some(burst.acquired.frequency_offset_hz);
                        last_sync_correlation = Some(burst.acquired.sync_correlation);
                        let sample_delta = burst.first_sample - block.first_sample as f64;
                        let rx_tick = block.time_ticks as f64
                            + sample_delta * settings.rx_tick_rate_hz as f64
                                / settings.applied_rx_sample_rate_hz as f64;
                        let relative_seconds = config.radio.device.rx_to_tx_delay_ns().unwrap_or(0)
                            as f64
                            / 1_000_000_000.0
                            + rx_tick / settings.rx_tick_rate_hz as f64
                            - epoch as f64 / settings.tx_tick_rate_hz as f64;
                        if tracing::enabled!(tracing::Level::DEBUG) {
                            let fields = &burst.acquired.fields;
                            let information: Vec<u8> = fields
                                .block
                                .information
                                .chunks(u8::BITS as usize)
                                .map(|chunk| {
                                    chunk.iter().enumerate().fold(0, |value, (bit, set)| {
                                        value | (u8::from(*set) << (u8::BITS as usize - 1 - bit))
                                    })
                                })
                                .collect();
                            tracing::debug!(rx_tick, relative_symbols=relative_seconds*SYMBOL_RATE_HZ,
                                sync_correlation=burst.acquired.sync_correlation, frequency_offset_hz=burst.acquired.frequency_offset_hz,
                                sync_evm_percent=?burst.acquired.quality.map(|quality|quality.sync_evm_percent),
                                sync_level_dbfs=?burst.acquired.quality.map(|quality|quality.sync_level_dbfs),
                                format=?fields.format, sync=?fields.sync, information_bits=fields.block.information.len(),
                                raw_hex=%format!("{information:02x?}"), "Reverse access burst decoded");
                        }
                        match access.receive(
                            relative_seconds * SYMBOL_RATE_HZ,
                            &burst.acquired.fields,
                            submitted,
                        ) {
                            Ok(Outcome::Complete(set)) => {
                                if let Some(interface) = interface.as_ref() {
                                    interface
                                        .hub
                                        .protocol
                                        .quality(Some(set.identity), &burst.acquired);
                                    interface.hub.protocol.bit_errors(
                                        set.identity,
                                        burst.acquired.fields.bit_errors(),
                                    );
                                }
                                for bytes in &set.messages {
                                    crate::operations::protocol::debug_reverse_access(
                                        set.identity,
                                        bytes,
                                    );
                                }
                                if let Some(interface) = interface.as_ref() {
                                    for bytes in &set.messages {
                                        interface.hub.protocol.message(
                                            "Reverse",
                                            "RACH",
                                            set.identity,
                                            bytes,
                                            "RX decoded and reassembled",
                                        );
                                    }
                                }
                                if !broadcast_session.serving_allowed() {
                                    tracing::debug!(identity=%set.identity,"Access deferred during broadcast recovery");
                                    continue;
                                }
                                let parsed = match super::access::parse(&set) {
                                    Ok(parsed) => parsed,
                                    Err(error) => {
                                        tracing::warn!(%error,"invalid access message set");
                                        continue;
                                    }
                                };
                                let procedure = parsed.procedure;
                                let set = parsed.primary.as_ref();
                                if let Some(companion) = &parsed.maca {
                                    let association = maca.associate(
                                        broadcast_session.desired(),
                                        sms.core().registry(),
                                        set,
                                        companion,
                                    );
                                    tracing::debug!(identity=%set.identity, source=?companion.source, ?association, "MACA report association");
                                    if let Some(interface) = interface.as_mut() {
                                        interface.maca_reading(
                                            set.identity,
                                            &association,
                                            submitted,
                                        );
                                    }
                                }

                                if let Some(interface) = interface.as_mut() {
                                    interface.access_reading(
                                        set.identity,
                                        burst.acquired.frequency_offset_hz,
                                        burst.acquired.sync_correlation,
                                        submitted,
                                        &format!("{procedure:?}"),
                                    );
                                }
                                if let Some(confirmation) = parsed.audit
                                    && let Err(error) =
                                        idle.received_audit(set.identity, confirmation, now_ms)
                                {
                                    if error == super::idle_queries::PollError::ResponseFields {
                                        tracing::warn!(identity=%set.identity,"Audit confirmation contains fields outside the requested scope");
                                    } else {
                                        return Err(error.into());
                                    }
                                }
                                if procedure == super::access::Procedure::Registration {
                                    match sms.core_mut().register(set, submitted / PHYSICAL_SLOTS) {
                                        Ok(dispatch) => {
                                            tracing::info!(identity=%set.identity,response=?dispatch.reply.message,"registration processed");
                                            let accepted = matches!(
                                            dispatch.reply.message,
                                            crate::l3::registration::RegistrationResponse::Accept
                                        );
                                            if accepted {
                                                sms.sync_subscribers(now_ms)?;
                                            }
                                            let queued = queue_registration_reply(
                                                &mut replies,
                                                &mut arq,
                                                &mut backlog,
                                                dispatch.reply,
                                                now_ms,
                                            );
                                            if let Err(error) = queued {
                                            tracing::warn!(%error,"registration reply could not be queued");
                                        } else if accepted
                                            && let Ok(primary)=crate::l3::registration::decode_prefix(&set.messages[0])
                                            && let crate::l3::registration::AccessMessage::Registration { registration_type,.. }=primary.message {
                                            if matches!(registration_type, crate::l3::registration::RegistrationType::PowerDown | crate::l3::registration::RegistrationType::Deregistration) { sms.cancel_offline(set.identity, now_ms)?; }
                                            idle.registration_received(sms.core().registry(),set.identity,registration_type,voice.subscriber_busy(set.identity)||sms.subscriber_busy(set.identity),now_ms)?;
                                        }
                                        }
                                        Err(error) => {
                                            tracing::warn!(%error,"registration rejected by parser")
                                        }
                                    }
                                } else if matches!(
                                    procedure,
                                    super::access::Procedure::Origination
                                        | super::access::Procedure::PageResponse
                                ) {
                                    match sms
                                        .core_mut()
                                        .observe_call_access(set, submitted / PHYSICAL_SLOTS)
                                    {
                                        Ok(true) => {
                                            sms.sync_subscribers(now_ms)?;
                                            tracing::debug!(identity=%set.identity, "Handset reachability refreshed from call access");
                                        }
                                        Ok(false) => {}
                                        Err(error) => {
                                            tracing::warn!(identity=%set.identity, %error, "Call access identity validation failed");
                                            let cause = match error {
                                                crate::core::messaging::MessagingError::Registry(crate::core::registry::RegistryError::CallAdmission(error)) => error.reject_cause(),
                                                _ => crate::l3::registration::RejectCause::UnknownMsid,
                                            };
                                            voice.reject_access(set.identity, cause, now_ms)?;
                                            continue;
                                        }
                                    }
                                    if procedure == super::access::Procedure::Origination {
                                        idle.registration_needed(
                                            sms.core().registry(),
                                            set.identity,
                                        );
                                    }
                                    if let Err(error) = voice.receive_access(
                                        sms.core().registry(),
                                        set,
                                        now_ms,
                                        submitted,
                                    ) {
                                        tracing::warn!(%error,"call access rejected");
                                        let cause =
                                            super::voice::access_rejection_cause(error.as_ref());
                                        let response = if procedure
                                            == super::access::Procedure::PageResponse
                                        {
                                            if let Some(cause) =
                                                super::voice::page_response_rejection_cause(
                                                    error.as_ref(),
                                                )
                                            {
                                                voice.reject_page_response(
                                                    set.identity,
                                                    cause,
                                                    now_ms,
                                                )
                                            } else {
                                                Ok(())
                                            }
                                        } else {
                                            voice.reject_access(set.identity, cause, now_ms)
                                        };
                                        if let Err(error) = response {
                                            tracing::warn!(%error,"call response could not be queued");
                                        }
                                    }
                                } else if procedure == super::access::Procedure::Audit {
                                    idle.advance(
                                        sms.core().registry(),
                                        |identity| {
                                            voice.subscriber_busy(identity)
                                                || sms.subscriber_busy(identity)
                                        },
                                        now_ms,
                                    )?;
                                    let confirmation = crate::l3::audit::AuditConfirmation::decode(
                                        &set.messages[0],
                                    )?;
                                    if let Err(error) =
                                        idle.received_audit(set.identity, confirmation, now_ms)
                                    {
                                        if error == super::idle_queries::PollError::ResponseFields {
                                            tracing::warn!(identity=%set.identity,"Audit confirmation contains fields outside the requested scope");
                                        } else {
                                            return Err(error.into());
                                        }
                                    }
                                } else if procedure == super::access::Procedure::Capabilities {
                                    idle.advance(
                                        sms.core().registry(),
                                        |identity| {
                                            voice.subscriber_busy(identity)
                                                || sms.subscriber_busy(identity)
                                        },
                                        now_ms,
                                    )?;
                                    let report =
                                        crate::l3::capability_report::decode(&set.messages[0])?;
                                    match sms.core_mut().record_capabilities(
                                        set.identity,
                                        report,
                                        submitted / PHYSICAL_SLOTS,
                                    ) {
                                        Ok(()) => idle.received(set.identity, now_ms)?,
                                        Err(error) => {
                                            tracing::warn!(%error, "standalone capability report rejected")
                                        }
                                    }
                                } else if matches!(
                                    procedure,
                                    super::access::Procedure::MessageWaiting
                                        | super::access::Procedure::UserAlert
                                ) {
                                    idle.advance(
                                        sms.core().registry(),
                                        |identity| {
                                            voice.subscriber_busy(identity)
                                                || sms.subscriber_busy(identity)
                                        },
                                        now_ms,
                                    )?;
                                    let confirmation =
                                        crate::l3::notification::SpachConfirmation::decode(
                                            &set.messages[0],
                                        )?;
                                    match idle.received_indication(
                                        set.identity,
                                        &confirmation,
                                        now_ms,
                                    ) {
                                        Ok(true) => {
                                            tracing::info!(identity=%set.identity, confirmed_type=confirmation.confirmed_type, "Idle PCH indication confirmed")
                                        }
                                        Ok(false) => {
                                            tracing::debug!(identity=%set.identity, "Unsolicited or expired idle indication confirmation ignored")
                                        }
                                        Err(super::idle_queries::PollError::ResponseFields) => {
                                            tracing::warn!(identity=%set.identity, "Idle indication confirmation has unsupported scope or fields")
                                        }
                                        Err(error) => return Err(error.into()),
                                    }
                                } else if procedure == super::access::Procedure::Sms {
                                    let unix_ms = unix_ms_at(protocol_unix_origin_ms, now_ms)?;
                                    if let Err(error) = sms.receive(set, now_ms, unix_ms) {
                                        tracing::warn!(%error,"SMS access rejected");
                                    }
                                } else {
                                    tracing::warn!(identity=%set.identity,"access procedure is not connected to this runtime yet");
                                }
                            }
                            Ok(Outcome::ArqStatus(status)) => {
                                let identity = arq.identity_for_echo(status.partial_echo);
                                if let Some(interface) = interface.as_ref() {
                                    interface.hub.protocol.quality(identity, &burst.acquired);
                                    if let Some(identity) = identity {
                                        interface.hub.protocol.bit_errors(
                                            identity,
                                            burst.acquired.fields.bit_errors(),
                                        );
                                    }
                                }
                                let matched =
                                    arq.status(&status, submitted / CONTROL_SLOT_PERIOD, now_ms)?;
                                if let Some(interface) = interface.as_ref() {
                                    interface.hub.protocol.arq_status(
                                        identity,
                                        &status,
                                        &burst.acquired.fields.block.information,
                                        matched,
                                    );
                                }
                            }
                            Ok(outcome) => {
                                if let Outcome::Pending(identity) | Outcome::Duplicate(identity) =
                                    &outcome
                                    && let Some(interface) = interface.as_ref()
                                {
                                    interface
                                        .hub
                                        .protocol
                                        .quality(Some(*identity), &burst.acquired);
                                }
                                tracing::debug!(?outcome, "access burst received");
                            }
                            Err(error) => {
                                tracing::warn!(%error, relative_symbols=relative_seconds * SYMBOL_RATE_HZ,
                                sync_correlation=burst.acquired.sync_correlation, frequency_offset_hz=burst.acquired.frequency_offset_hz,
                                sync_evm_percent=?burst.acquired.quality.map(|quality|quality.sync_evm_percent),
                                sync_level_dbfs=?burst.acquired.quality.map(|quality|quality.sync_level_dbfs),
                                rx_tick, next_forward_slot=submitted, rx_segment,
                                rx_block_first_sample=block.first_sample, rx_block_time_ticks=block.time_ticks,
                                burst_first_sample=burst.first_sample, rx_sample_offset=sample_delta,
                                rx_to_tx_delay_ns=config.radio.device.rx_to_tx_delay_ns().unwrap_or(0), tx_epoch_tick=epoch,
                                "access burst rejected")
                            }
                        }
                    }
                }
            }
            timing.rx_processing_us = diagnostics::micros(phase_started.elapsed());
            phase_started = Instant::now();
            if voice.has_power_down_reports() {
                voice.exchange_sms(&mut sms, now_ms, unix_ms)?;
            }
            while let Some(registration) = voice.take_power_down_report() {
                if !sms.power_down(&registration, now_ms)? {
                    tracing::debug!(identity=%registration.identity(), "Stale traffic power-down report ignored after registration changed");
                }
            }
            sms.advance_auxiliary_jobs(now_ms)?;
            voice.exchange_sms(&mut sms, now_ms, unix_ms)?;
            arq.cancel_matching(|identity, owner| {
                matches!(owner, ArqOwner::Sms(message) if !sms.arq_current(message))
                    || voice.subscriber_busy(identity)
                    || !sms
                        .core()
                        .registry()
                        .subscribers()
                        .iter()
                        .any(|subscriber| subscriber.provisioning.identity == identity)
            });
            backlog.advance(
                &mut arq,
                submitted / CONTROL_SLOT_PERIOD,
                now_ms,
                |identity| {
                    voice.subscriber_busy(identity)
                        || !sms
                            .core()
                            .registry()
                            .subscribers()
                            .iter()
                            .any(|subscriber| subscriber.provisioning.identity == identity)
                },
                interface
                    .as_ref()
                    .map(|interface| interface.hub.protocol.as_ref()),
            )?;
            if arq.pending() < super::spach_arq::REFRESH_CAPACITY
                && let Some(message) = sms.arq_offer(|identity| {
                    !arq.contains(identity) && !voice.subscriber_busy(identity)
                })
            {
                let request = super::spach_arq::Request {
                    identity: message.identity,
                    channel: crate::link::spach::Channel::Smsch,
                    message: message.bytes.clone(),
                    completion: ArqOwner::Sms(message.clone()),
                    deadline_ms: message.deadline_ms,
                };
                match arq.enqueue(request, submitted / CONTROL_SLOT_PERIOD, now_ms) {
                    Ok(_) => sms.arq_admitted(message),
                    Err(error) => tracing::debug!(%error,"SMS ARQ admission deferred"),
                }
            }
            arq.refresh_waiting(submitted / CONTROL_SLOT_PERIOD, now_ms)?;
            drain_arq_events(
                &mut arq,
                interface
                    .as_ref()
                    .map(|interface| interface.hub.protocol.as_ref()),
                &mut completed_registrations,
                &mut sms,
                now_ms,
            )?;
            sms.settle_auxiliary_fences(&carrier, &voice, now_ms)?;
            sms.settle_reply_fences(&carrier, &voice, now_ms)?;
            sms.dispatch_incoming_responses(now_ms, unix_ms)?;
            sms.dispatch_outgoing_sms(now_ms)?;
            sms.settle_incoming_retirements(&carrier, now_ms, |peer| {
                access.retains(peer) || voice.retains_received_signaling(peer)
            })?;
            sms.settle_outgoing_retirements(&carrier, now_ms, |peer| {
                access.retains(peer) || voice.retains_received_signaling(peer)
            })?;
            let superframe = submitted / PHYSICAL_SLOTS;
            if reachability_superframe != superframe {
                reachability_superframe = superframe;
                let busy: Vec<_> = sms
                    .core()
                    .registry()
                    .subscribers()
                    .iter()
                    .map(|subscriber| subscriber.provisioning.identity)
                    .filter(|identity| {
                        voice.subscriber_busy(*identity) || sms.subscriber_busy(*identity)
                    })
                    .collect();
                let expired = sms
                    .core_mut()
                    .expire_observed_access(superframe, |identity| busy.contains(&identity));
                for identity in expired {
                    sms.cancel_offline(identity, now_ms)?;
                    tracing::info!(%identity, "Observed handset reachability expired");
                }
                for subscriber in sms.core().registry().subscribers() {
                    let identity = subscriber.provisioning.identity;
                    if subscriber.registration.is_some_and(|registration| {
                        registration.last_observed_access_superframe.is_some()
                    }) && subscriber.capabilities.is_none()
                        && !busy.contains(&identity)
                        && idle.state(identity).is_none()
                        && let Err(error) =
                            idle.request(sms.core().registry(), identity, false, now_ms)
                    {
                        tracing::debug!(%identity, %error, "Capability query deferred for observed phone");
                    }
                }
            }
            idle.solicit_registrations(
                sms.core().registry(),
                |identity| voice.subscriber_busy(identity) || sms.subscriber_busy(identity),
                now_ms,
            )?;
            idle.advance(
                sms.core().registry(),
                |identity| voice.subscriber_busy(identity) || sms.subscriber_busy(identity),
                now_ms,
            )?;
            broadcast_session.advance(sms.core_mut().store_mut(), submitted, now_ms, unix_ms)?;
            store_requests.run_pending(sms.core_mut().store_mut(), STORE_REQUESTS_PER_PASS);
            sms.core_mut().store_mut().flush()?;
            sms.stamp_durability();
            store_requests.answer_durable(sms.core().store());
            timing.protocol_us = diagnostics::micros(phase_started.elapsed());
            phase_started = Instant::now();
            while carrier.pending_slots() < QUEUED_SLOTS {
                let slot = submitted % PHYSICAL_SLOTS;
                if submitted / PHYSICAL_SLOTS != snapshot_superframe
                    || snapshot_version != broadcast_session.version()
                {
                    snapshot_superframe = submitted / PHYSICAL_SLOTS;
                    let MandatoryBroadcast::DcchStructure(structure) =
                        &broadcast_session.content(snapshot_superframe).messages()[0]
                    else {
                        unreachable!("broadcast structure is first")
                    };
                    let mut time = super::system_time::for_superframe(
                        protocol_clock.milliseconds(epoch)?,
                        snapshot_superframe,
                        structure.fbcch_slots + structure.ebcch_slots - 1,
                        now_ms,
                    )?;
                    if broadcast_session.set_time_zone(time.time_zone)? {
                        tracing::info!(
                            utc_offset_minutes = time.time_zone.minutes(),
                            daylight_saving = time.time_zone.daylight_saving(),
                            "Scheduling broadcast timezone change"
                        );
                        broadcast_session.advance(
                            sms.core_mut().store_mut(),
                            submitted,
                            now_ms,
                            unix_ms,
                        )?;
                    }
                    snapshot_version = broadcast_session.version();
                    snapshot = broadcast_session.snapshot(snapshot_superframe)?;
                    if let Some(offset) = broadcast_session.content(snapshot_superframe).time_zone()
                    {
                        time.time_zone = offset;
                        snapshot.set_time_date(time)?;
                    }
                    let active = broadcast_session.content(snapshot_superframe);
                    let MandatoryBroadcast::DcchStructure(structure) = &active.messages()[0] else {
                        unreachable!("broadcast structure is first")
                    };
                    active_dvcc = structure.dvcc;
                    let revision = broadcast_session.revision(snapshot_superframe);
                    if !observed_revision.same_revision(revision) {
                        if let Some(interface) = interface.as_ref() {
                            carrier.set_observer(interface.hub.protocol.observer(
                                structure.dvcc,
                                structure.fbcch_slots,
                                structure.ebcch_slots,
                            ));
                        }
                        observed_revision = revision.clone();
                    }
                }
                let skipped = carrier.skip_expired_slots(
                    submitted,
                    submission_lead_ticks,
                    PHYSICAL_SLOTS - slot,
                )?;
                if skipped != 0 {
                    broadcast_session.invalidate_through(submitted / PHYSICAL_SLOTS);
                    skipped_slots += skipped;
                    for skipped_slot in submitted..submitted + skipped {
                        if snapshot.channel(skipped_slot % PHYSICAL_SLOTS)?
                            == Some(crate::phy::control::LogicalChannel::Spach)
                        {
                            arbiter.opportunity()?;
                        }
                        voice.skipped_slot(skipped_slot)?;
                    }
                    tracing::warn!(
                        physical_slot = submitted,
                        skipped_slots = skipped,
                        resume_slot = submitted + skipped,
                        queue_depth = carrier.pending_slots(),
                        loop_elapsed_us = diagnostics::micros(loop_started.elapsed()),
                        refill_elapsed_us = diagnostics::micros(phase_started.elapsed()),
                        current_timing = ?timing,
                        previous_timing = ?tx_diagnostics.previous,
                        "Missed TX slot. Resuming on the existing TDMA timeline"
                    );
                    submitted += skipped;
                    continue;
                }
                let is_spach =
                    snapshot.channel(slot)? == Some(crate::phy::control::LogicalChannel::Spach);
                if is_spach {
                    arbiter.opportunity()?;
                }
                let feedback = if !broadcast_session.serving_allowed() {
                    crate::link::feedback::AccessFeedback {
                        availability: crate::link::feedback::Availability::Busy,
                        reception: crate::link::feedback::Reception::NotReceived,
                        partial_echo: 0,
                    }
                    .encode()?
                } else if slot.is_multiple_of(CONTROL_SLOT_PERIOD) {
                    access.feedback(submitted)
                } else {
                    crate::link::feedback::AccessFeedback {
                        availability: crate::link::feedback::Availability::Idle,
                        reception: crate::link::feedback::Reception::NotReceived,
                        partial_echo: 0,
                    }
                    .encode()?
                };
                if !slot.is_multiple_of(CONTROL_SLOT_PERIOD)
                    && let Some((call_id, party, burst)) = voice.traffic(submitted)?
                {
                    let mut failure = None;
                    retry_late_tx!(
                        submitted,
                        carrier.submit_slot_with(submitted, burst.bits(), || {
                            match voice.traffic_submitted_at(call_id, party, &burst, now_ms) {
                                Ok(command) => {
                                    Completion::Voice(super::voice::Completion::Traffic {
                                        party,
                                        call_id,
                                        command,
                                    })
                                }
                                Err(error) => {
                                    failure = Some(error);
                                    Completion::Idle
                                }
                            }
                        })
                    );
                    if let Some(error) = failure {
                        return Err(error);
                    }
                    submitted += 1;
                    continue;
                }
                if is_spach && broadcast_session.serving_allowed() {
                    let superframe = submitted / PHYSICAL_SLOTS;
                    let phase = (slot / CONTROL_SLOT_PERIOD) as u8;
                    let block = submitted / CONTROL_SLOT_PERIOD;
                    arq.refresh_waiting(block, now_ms)?;
                    arbiter.retain(|echo| arq.has_echo(echo));
                    for echo in arbiter.overdue() {
                        if arq.interrupt(echo, block, now_ms)? {
                            tracing::warn!(
                                partial_echo = echo,
                                "Restarting ARQ after a missed SPACH interruption deadline"
                            );
                        }
                    }
                    arbiter.retain(|echo| arq.has_echo(echo));
                    if (idle.has_repeat(superframe, phase) || idle.in_progress())
                        && let Some(information) = idle.frame(superframe, phase).copied()
                    {
                        let control = snapshot
                            .encode_slot(slot, &feedback, Some(&information))?
                            .expect("PCH repeat slot selected");
                        retry_late_tx!(
                            submitted,
                            carrier
                                .submit_slot(
                                    submitted,
                                    &control.bits,
                                    idle.completion()
                                        .map_or(Completion::Idle, Completion::Query)
                                )
                                .map_err(|rejected| rejected.error)
                        );
                        idle.submitted()?;
                        arbiter.other_transmitted();
                        submitted += 1;
                        continue;
                    }
                    if (voice.has_repeat(superframe, phase) || voice.page_in_progress())
                        && let Some(information) = voice.control(superframe, phase).copied()
                    {
                        let control = snapshot
                            .encode_slot(slot, &feedback, Some(&information))?
                            .expect("voice PCH repeat selected");
                        retry_late_tx!(
                            submitted,
                            carrier
                                .submit_slot(
                                    submitted,
                                    &control.bits,
                                    Completion::Voice(voice.control_completion())
                                )
                                .map_err(|rejected| rejected.error)
                        );
                        voice.control_submitted()?;
                        arbiter.other_transmitted();
                        submitted += 1;
                        continue;
                    }
                    if sms.has_repeat(superframe, phase)
                        && let Some(information) = sms.non_arq_frame(superframe, phase).copied()
                    {
                        let control = snapshot
                            .encode_slot(slot, &feedback, Some(&information))?
                            .expect("SMS PCH repeat selected");
                        retry_late_tx!(
                            submitted,
                            carrier
                                .submit_slot(
                                    submitted,
                                    &control.bits,
                                    Completion::Sms(sms.completion())
                                )
                                .map_err(|rejected| rejected.error)
                        );
                        sms.submitted_at(now_ms)?;
                        arbiter.other_transmitted();
                        submitted += 1;
                        continue;
                    }
                    let repeats = idle.repeat_count(superframe, phase)
                        + voice.repeat_count(superframe, phase)
                        + sms.repeat_count(superframe, phase);
                    let atomic = replies.in_progress()
                        || sms.in_progress()
                        || voice.control_in_progress()
                        || idle.in_progress();
                    macro_rules! submit_arq {
                        () => {
                            if let Some(frame) =
                                prepare_arq(&mut arq, &arbiter, repeats, block, now_ms)?
                            {
                                let control = snapshot
                                    .encode_slot(slot, &feedback, Some(&frame.information))?
                                    .expect("ARQ SPACH selected");
                                retry_late_tx!(
                                    submitted,
                                    carrier
                                        .submit_slot(
                                            submitted,
                                            &control.bits,
                                            Completion::Arq(frame.clone())
                                        )
                                        .map_err(|rejected| rejected.error)
                                );
                                arq.submitted(&frame.token, block, now_ms)?;
                                arbiter.transmitted(frame.partial_echo());
                                if let Some(ArqOwner::Sms(message)) =
                                    arq.submitted_message(&frame.token)
                                {
                                    sms.arq_submitted_at(message, now_ms)?;
                                }
                                submitted += 1;
                                continue;
                            }
                        };
                    }
                    if !atomic
                        && ((arbiter.prefers_arq() && arq.ready())
                            || !arbiter.can_interrupt(1, repeats))
                    {
                        submit_arq!();
                    }
                    if !idle.in_progress()
                        && !replies.in_progress()
                        && !sms.in_progress()
                        && let Some(information) = voice.control(superframe, phase).copied()
                        && (voice.control_in_progress()
                            || arbiter.can_interrupt(
                                voice.control_frame_count(),
                                repeats + voice.control_adds_repeats(),
                            ))
                    {
                        let control = snapshot
                            .encode_slot(slot, &feedback, Some(&information))?
                            .expect("voice SPACH selected");
                        retry_late_tx!(
                            submitted,
                            carrier
                                .submit_slot(
                                    submitted,
                                    &control.bits,
                                    Completion::Voice(voice.control_completion())
                                )
                                .map_err(|rejected| rejected.error)
                        );
                        voice.control_submitted()?;
                        arbiter.other_transmitted();
                        submitted += 1;
                        continue;
                    }
                    if !idle.in_progress() && !sms.in_progress() && !voice.control_in_progress() {
                        let reply_now_ms =
                            protocol_clock.milliseconds(last_observation.hardware_tick)?;
                        let frames = replies.frame_count(reply_now_ms);
                        if frames > 0
                            && (replies.in_progress() || arbiter.can_interrupt(frames, repeats))
                            && retry_late_tx!(
                                submitted,
                                replies.submit_next(
                                    &mut carrier,
                                    &snapshot,
                                    submitted,
                                    &feedback,
                                    reply_now_ms
                                )
                            )
                            .is_some()
                        {
                            arbiter.other_transmitted();
                            submitted += 1;
                            continue;
                        }
                    }
                    if !idle.in_progress()
                        && !replies.in_progress()
                        && !voice.control_in_progress()
                        && let Some(information) = sms.non_arq_frame(superframe, phase).copied()
                        && (sms.in_progress()
                            || arbiter.can_interrupt(
                                sms.frame_count(),
                                repeats + usize::from(sms.adds_repeat()),
                            ))
                    {
                        let control = snapshot
                            .encode_slot(slot, &feedback, Some(&information))?
                            .expect("SMS SPACH selected");
                        retry_late_tx!(
                            submitted,
                            carrier
                                .submit_slot(
                                    submitted,
                                    &control.bits,
                                    Completion::Sms(sms.completion())
                                )
                                .map_err(|rejected| rejected.error)
                        );
                        sms.submitted_at(now_ms)?;
                        arbiter.other_transmitted();
                        submitted += 1;
                        continue;
                    }
                    if !atomic
                        && let Some(information) = idle.frame(superframe, phase).copied()
                        && arbiter.can_interrupt(idle.frame_count(), repeats + idle.adds_repeats())
                    {
                        let control = snapshot
                            .encode_slot(slot, &feedback, Some(&information))?
                            .expect("idle query SPACH selected");
                        let completion = idle
                            .completion()
                            .map_or(Completion::Idle, Completion::Query);
                        retry_late_tx!(
                            submitted,
                            carrier
                                .submit_slot(submitted, &control.bits, completion)
                                .map_err(|rejected| rejected.error)
                        );
                        idle.submitted()?;
                        arbiter.other_transmitted();
                        submitted += 1;
                        continue;
                    }
                    if !atomic {
                        submit_arq!();
                    }
                }
                let bits = match snapshot.encode_slot(slot, &feedback, None)? {
                    Some(control) => control.bits,
                    None => traffic::encode_forward(
                        SyncWord::ALL[slot as usize % SyncWord::ALL.len()],
                        &[false; SACCH_BITS],
                        &[false; DATA_BITS],
                        // A stopped handset must not retain DVCC continuity from unused-slot filler.
                        active_dvcc ^ UNASSIGNED_COLOR_MASK,
                        0,
                    )?,
                };
                retry_late_tx!(
                    submitted,
                    carrier
                        .submit_slot(submitted, &bits, Completion::Idle)
                        .map_err(|rejected| rejected.error)
                );
                submitted += 1;
            }
            timing.tx_refill_us = diagnostics::micros(phase_started.elapsed());
            phase_started = Instant::now();
            if let Some(interface) = interface.as_mut()
                && now_ms >= next_publication_ms
            {
                interface.publish(
                    &sms,
                    &voice,
                    &idle,
                    observation,
                    crate::operations::RuntimeStatus {
                        position: clock.at(submitted / PHYSICAL_SLOTS),
                        now_ms,
                        submitted,
                        completed_registrations,
                        skipped_slots,
                        rx_discontinuities,
                        queue_depth: carrier.pending_slots(),
                        access_bursts: access_burst_count,
                        traffic_bursts: traffic_burst_count,
                        rx_power_dbfs,
                        last_frequency_offset_hz,
                        last_sync_correlation,
                    },
                );
                next_publication_ms =
                    now_ms.saturating_add(crate::operations::SNAPSHOT_INTERVAL_MS);
            }
            timing.publication_us = diagnostics::micros(phase_started.elapsed());
            phase_started = Instant::now();
            received = carrier.read(&mut iq)?;
            timing.rx_read_us = diagnostics::micros(phase_started.elapsed());
            phase_started = Instant::now();
            if matches!(received, ReadOutcome::Timeout { .. }) {
                std::thread::sleep(Duration::from_micros(IDLE_WAIT_US));
            }
            timing.idle_wait_us = diagnostics::micros(phase_started.elapsed());
            tx_diagnostics.finish(loop_started, timing, carrier.pending_slots(), skipped_slots);
        }
        Ok(())
    })();
    if let Err(error) = &result {
        tracing::error!(%error, "serving loop failed, stopping the cell");
    }
    let stopped = carrier.stop();
    backlog.shutdown(
        interface
            .as_ref()
            .map(|interface| interface.hub.protocol.as_ref()),
    );
    arq.cancel_all();
    drain_arq_events(
        &mut arq,
        interface
            .as_ref()
            .map(|interface| interface.hub.protocol.as_ref()),
        &mut completed_registrations,
        &mut sms,
        protocol_clock.milliseconds(last_observation.hardware_tick)?,
    )?;
    idle.shutdown();
    if stopped.is_ok() {
        voice.shutdown(protocol_clock.milliseconds(last_observation.hardware_tick)?)?;
    }
    let sms_stopped = sms.shutdown(protocol_clock.milliseconds(last_observation.hardware_tick)?);
    if let Err(error) = sms_stopped {
        tracing::error!(%error,"SMS shutdown failed");
    }
    if carrier.stop_acknowledged() {
        sms.settle_reply_fences(
            &carrier,
            &voice,
            protocol_clock.milliseconds(last_observation.hardware_tick)?,
        )?;
        sms.settle_auxiliary_fences(
            &carrier,
            &voice,
            protocol_clock.milliseconds(last_observation.hardware_tick)?,
        )?;
    }
    let final_ms = protocol_clock.milliseconds(last_observation.hardware_tick)?;
    sms.settle_incoming_responses(final_ms, unix_ms_at(protocol_unix_origin_ms, final_ms)?)?;
    let store = sms.core_mut().store_mut();
    match store.drain(STORE_DRAIN_TIMEOUT) {
        Ok(true) => {}
        Ok(false) => tracing::error!(
            undurable_changes = store.undurable_changes(),
            "state changes were not all written to the database file before shutdown"
        ),
        Err(error) => tracing::error!(%error, "state could not be flushed at shutdown"),
    }
    sms_hub.request_stop();
    store_requests.answer_durable(store);
    drop(store_requests);
    drop(sms_hub);
    tracing::info!(
        submitted_slots = submitted,
        completed_registrations,
        stop_acknowledged = carrier.stop_acknowledged(),
        "cell stopped"
    );
    if let Some(interface) = interface.as_mut() {
        interface.publish(
            &sms,
            &voice,
            &idle,
            last_observation,
            crate::operations::RuntimeStatus {
                position: clock.at(submitted / PHYSICAL_SLOTS),
                now_ms: protocol_clock.milliseconds(last_observation.hardware_tick)?,
                submitted,
                completed_registrations,
                skipped_slots,
                rx_discontinuities,
                queue_depth: carrier.pending_slots(),
                access_bursts: access_burst_count,
                traffic_bursts: traffic_burst_count,
                rx_power_dbfs,
                last_frequency_offset_hz,
                last_sync_correlation,
            },
        );
        interface.stopped(
            carrier.stop_acknowledged(),
            result.as_ref().err().map(|error| error.to_string()),
        );
    }
    match (result, stopped) {
        (Err(error), Err(stop)) => Err(format!("{error}. Shutdown also failed: {stop}").into()),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error.into()),
        _ => Ok(Summary {
            submitted_slots: submitted,
            completed_registrations,
            stop_acknowledged: carrier.stop_acknowledged(),
        }),
    }
}

#[cfg(test)]
mod tests;
