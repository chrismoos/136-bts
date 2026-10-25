use super::*;
use crate::{
    l3::registration::{AccessMessage, RegistrationResponse, RegistrationType},
    link::{
        rach::{self, MobileIdentity},
        spach::SpachReceiver,
    },
    phy::{
        acquisition::decode_forward_control,
        control::LogicalChannel,
        modulation::{Dibit, DifferentialEncoder},
        pulse::RootRaisedCosine,
    },
};
use sdr_hal::{Radio, RadioRx, RadioTx, RxReadResult, error::Error as HalError};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
// Concurrent paced cells can miss real-clock response deadlines on shared hosts.
static PACED_TEST_LOCK: Mutex<()> = Mutex::new(());
const RATE: usize = 388800;
const TICKS: u64 = 1_000_000_000;
const SAMPLES_PER_SYMBOL: usize = 16;
const GUARD_SYMBOLS: usize = 3;
const RAMP_SYMBOLS: usize = 3;
const FILTER_SPAN: usize = 8;
const ACCESS_BLOCK: u64 = 10;
const ACCESS_PATH_BLOCKS: u64 = 6;
const REVERSE_LEAD_SYMBOLS: usize = 207;
const STOP_SLOTS: usize = 192;
mod arq_loop;
mod broadcast_recovery;
mod receive_window;
mod registration_reject;
mod segmented_sms;
mod sms_loop;
mod voice_loop;
struct Backend {
    sample: usize,
    tx_observation_offset_ticks: u64,
    epoch: Option<usize>,
    tx: Vec<Complex32>,
    slot_count: usize,
    stop_slots: usize,
    underflow_after_slot: Option<usize>,
    stall_after_slot: Option<usize>,
    realtime_epoch: Option<Instant>,
    handset_segmented: Option<segmented_sms::Handsets>,
    handset_sms: Option<sms_loop::Handset>,
    handset_voice: Option<voice_loop::Handsets>,
    handset_idle: Option<idle_loop::Handsets>,
    handset_arq: Option<arq_loop::Handsets>,
    traffic_packets: Vec<(u64, isize, Vec<Complex32>)>,
    packets: Vec<(u64, Vec<Complex32>)>,
    tx_active: bool,
    rx_active: bool,
    shutdown: Arc<AtomicBool>,
}
struct Device(Arc<Mutex<Backend>>);
struct Tx(Arc<Mutex<Backend>>);
struct Rx(Arc<Mutex<Backend>>);
impl Radio for Device {
    fn tx_sample_rate_hz(&self) -> f64 {
        RATE as f64
    }
    fn rx_sample_rate_hz(&self) -> Option<f64> {
        Some(RATE as f64)
    }
    fn supports_timed_tx(&self) -> bool {
        true
    }
    fn tick_rate(&self) -> u64 {
        TICKS
    }
    fn set_tx_frequency(&mut self, _: usize) -> Result<(), HalError> {
        Ok(())
    }
    fn set_tx_sample_rate(&mut self, rate: f64) -> Result<f64, HalError> {
        Ok(rate)
    }
    fn set_tx_bandwidth(&mut self, _: usize) -> Result<(), HalError> {
        Ok(())
    }
    fn split(self: Box<Self>) -> Result<(Box<dyn RadioTx>, Option<Box<dyn RadioRx>>), HalError> {
        Ok((Box::new(Tx(self.0.clone())), Some(Box::new(Rx(self.0)))))
    }
}
impl RadioTx for Tx {
    fn tx_sample_rate_hz(&self) -> f64 {
        RATE as f64
    }
    fn supports_timed_tx(&self) -> bool {
        true
    }
    fn tick_rate(&self) -> u64 {
        TICKS
    }
    fn get_hardware_time(&self) -> Result<u64, HalError> {
        let state = self.0.lock().unwrap();
        Ok((state.sample as u64 * TICKS).div_ceil(RATE as u64) + state.tx_observation_offset_ticks)
    }
    fn set_tx_frequency_hz(&mut self, _: f64) -> Result<(), HalError> {
        Ok(())
    }
    fn transmit(&mut self, _: &[Complex32]) -> Result<(), HalError> {
        Err("timed writes required".into())
    }
    fn transmit_at(&mut self, samples: &[Complex32], tick: Option<u64>) -> Result<(), HalError> {
        let mut state = self.0.lock().unwrap();
        if state.epoch.is_none() {
            state.epoch = Some(
                (tick.unwrap() * RATE as u64 / TICKS) as usize + FILTER_SPAN * SAMPLES_PER_SYMBOL,
            );
        }
        let start_sample = (tick.unwrap() as f64 * RATE as f64 / TICKS as f64).round() as usize;
        let offset = start_sample - (state.epoch.unwrap() - FILTER_SPAN * SAMPLES_PER_SYMBOL);
        if offset < state.tx.len() {
            return Err("overlapping scheduled TX writes".into());
        }
        let additional = offset + samples.len() - state.tx.len();
        state
            .tx
            .try_reserve(additional)
            .map_err(|error| HalError::from(error.to_string()))?;
        state.tx.resize(offset, Complex32::new(0.0, 0.0));
        state.tx.extend_from_slice(samples);
        state.slot_count = offset / (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL) + 1;
        if state
            .stall_after_slot
            .is_some_and(|slot| state.slot_count >= slot)
        {
            state.stall_after_slot = None;
            state.sample += 4 * SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL;
        }
        if let Some(mut handsets) = state.handset_arq.take() {
            let observed = handsets.observe(&mut state);
            state.handset_arq = Some(handsets);
            observed?;
        }
        if let Some(mut handset) = state.handset_segmented.take() {
            let observed = handset.observe(&mut state);
            state.handset_segmented = Some(handset);
            observed?;
        }
        if let Some(mut handset) = state.handset_sms.take() {
            let observed = handset.observe(&mut state);
            state.handset_sms = Some(handset);
            observed?;
        }
        if let Some(mut handsets) = state.handset_voice.take() {
            let observed = handsets.observe(&mut state);
            state.handset_voice = Some(handsets);
            observed?;
        }
        if let Some(mut handsets) = state.handset_idle.take() {
            let observed = handsets.observe(&mut state);
            state.handset_idle = Some(handsets);
            observed?;
        }
        if state.slot_count >= state.stop_slots {
            state.shutdown.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
    fn tx_health(&mut self) -> Result<sdr_hal::TxRadioHealth, HalError> {
        let state = self.0.lock().unwrap();
        Ok(sdr_hal::TxRadioHealth {
            underflows: u64::from(
                state
                    .underflow_after_slot
                    .is_some_and(|slot| state.slot_count >= slot),
            ),
            ..Default::default()
        })
    }
    fn enable_transmit(&mut self, enable: bool) -> Result<(), HalError> {
        self.0.lock().unwrap().tx_active = enable;
        Ok(())
    }
}
impl RadioRx for Rx {
    fn rx_sample_rate_hz(&self) -> f64 {
        RATE as f64
    }
    fn tick_rate(&self) -> u64 {
        TICKS
    }
    fn get_hardware_time(&self) -> Result<u64, HalError> {
        Ok((self.0.lock().unwrap().sample as u64 * TICKS).div_ceil(RATE as u64))
    }
    fn set_rx_frequency(&mut self, _: f64) -> Result<(), HalError> {
        Ok(())
    }
    fn rx_activate(&mut self, _: Option<u64>) -> Result<(), HalError> {
        self.0.lock().unwrap().rx_active = true;
        Ok(())
    }
    fn stop(&mut self) -> Result<(), HalError> {
        self.0.lock().unwrap().rx_active = false;
        Ok(())
    }
    fn rx_read(&mut self, buffer: &mut [Complex32], _: i64) -> Result<RxReadResult, HalError> {
        let realtime = {
            let state = self.0.lock().unwrap();
            state.realtime_epoch.map(|epoch| (epoch, state.sample))
        };
        if let Some((epoch, sample)) = realtime {
            let target = Duration::from_secs_f64(sample as f64 / RATE as f64);
            if let Some(wait) = target.checked_sub(epoch.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        let mut state = self.0.lock().unwrap();
        let first = state.sample;
        buffer.fill(Complex32::new(0.0, 0.0));
        if let Some(epoch) = state.epoch {
            for (physical_slot, advance_samples, packet) in state
                .packets
                .iter()
                .map(|(block, packet)| (block * CONTROL_SLOT_PERIOD, 0, packet))
                .chain(
                    state
                        .traffic_packets
                        .iter()
                        .map(|(slot, advance_samples, packet)| (*slot, *advance_samples, packet)),
                )
            {
                let start = epoch + physical_slot as usize * SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL
                    - REVERSE_LEAD_SYMBOLS * SAMPLES_PER_SYMBOL
                    - FILTER_SPAN * SAMPLES_PER_SYMBOL;
                let start = start
                    .checked_add_signed(-advance_samples)
                    .ok_or_else(|| HalError::from("simulated sample position overflow"))?;
                mix_overlap(buffer, first, packet, start);
            }
        }
        state.sample += buffer.len();
        Ok(RxReadResult {
            samples_read: buffer.len(),
            time_ticks: (first as u64 * TICKS).div_ceil(RATE as u64),
            overflow: false,
        })
    }
}
fn mix_overlap(buffer: &mut [Complex32], first: usize, packet: &[Complex32], start: usize) {
    let overlap_start = first.max(start);
    let overlap_end = first
        .saturating_add(buffer.len())
        .min(start.saturating_add(packet.len()));
    if overlap_start >= overlap_end {
        return;
    }
    for (value, sample) in buffer[overlap_start - first..overlap_end - first]
        .iter_mut()
        .zip(&packet[overlap_start - start..overlap_end - start])
    {
        *value += *sample;
    }
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn simulated_rx_only_mixes_intersecting_samples_and_preserves_superposition() {
    let packet = [
        Complex32::new(1.0, -1.0),
        Complex32::new(2.0, -2.0),
        Complex32::new(3.0, -3.0),
    ];
    for (first, start, expected) in [
        (10, 8, [3.0, 0.0, 0.0, 0.0]),
        (10, 10, [1.0, 2.0, 3.0, 0.0]),
        (10, 12, [0.0, 0.0, 1.0, 2.0]),
        (10, 14, [0.0; 4]),
        (10, 7, [0.0; 4]),
    ] {
        let mut buffer = [Complex32::new(4.0, 5.0); 4];
        mix_overlap(&mut buffer, first, &packet, start);
        assert_eq!(
            buffer,
            expected.map(|sample| Complex32::new(4.0 + sample, 5.0 - sample))
        );
    }
}
fn packet() -> Vec<Complex32> {
    packet_version(ProtocolVersion::Is136RevA)
}
fn packet_version(protocol_version: ProtocolVersion) -> Vec<Complex32> {
    let message = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 15,
        protocol_version,
    }
    .encode()
    .unwrap();
    let mut packets = access_packets(MobileIdentity::Min34(1), &message);
    assert_eq!(packets.len(), 1);
    packets.remove(0).1
}
fn access_packets(identity: MobileIdentity, message: &[u8]) -> Vec<(u64, Vec<Complex32>)> {
    rach::encode(BurstFormat::Normal, identity, &[message])
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, frame)| (index as u64 * ACCESS_PATH_BLOCKS, burst_packet(frame)))
        .collect()
}
fn burst_packet(frame: &[bool]) -> Vec<Complex32> {
    let bits = crate::phy::rach::encode(BurstFormat::Normal, SyncWord::One, frame, 1).unwrap();
    reverse_waveform(&bits)
}
fn reverse_waveform(bits: &[bool; SLOT_BITS]) -> Vec<Complex32> {
    reverse_waveform_active(bits, SYMBOLS_PER_SLOT)
}
fn reverse_waveform_active(bits: &[bool; SLOT_BITS], active_end: usize) -> Vec<Complex32> {
    let mut symbols = vec![Symbol { i: 0.0, q: 0.0 }; SYMBOLS_PER_SLOT];
    let mut differential = DifferentialEncoder::default();
    for ramp in 0..RAMP_SYMBOLS {
        symbols[GUARD_SYMBOLS + ramp] = Symbol {
            i: (ramp + 1) as f64 / RAMP_SYMBOLS as f64,
            q: 0.0,
        };
    }
    for symbol in GUARD_SYMBOLS + RAMP_SYMBOLS..active_end {
        symbols[symbol] =
            differential.encode(Dibit::from_bits([bits[symbol * 2], bits[symbol * 2 + 1]]));
    }
    if active_end < SYMBOLS_PER_SLOT {
        let previous = symbols[active_end - 1];
        for ramp in 0..RAMP_SYMBOLS {
            let amplitude = (RAMP_SYMBOLS - 1 - ramp) as f64 / RAMP_SYMBOLS as f64;
            symbols[active_end + ramp] = Symbol {
                i: previous.i * amplitude,
                q: previous.q * amplitude,
            };
        }
    }
    let filter = RootRaisedCosine::try_new(RATE as f64, FILTER_SPAN).unwrap();
    let mut impulses = vec![
        Symbol { i: 0.0, q: 0.0 };
        symbols.len() * SAMPLES_PER_SYMBOL + filter.delay_samples() * 2
    ];
    for (index, symbol) in symbols.into_iter().enumerate() {
        impulses[index * SAMPLES_PER_SYMBOL] = symbol;
    }
    filter
        .filter(&impulses)
        .into_iter()
        .map(|symbol| Complex32::new(symbol.i as f32, symbol.q as f32))
        .collect()
}
struct Database(PathBuf);
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn generated_registration_iq_crosses_serving_loop_and_returns_an_arch_accept() {
    registration_loop(None, false, ProtocolVersion::Is136RevA, None);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn generated_maca_companion_iq_preserves_registration_and_decoded_report() {
    let report = crate::l3::maca::MacaReport {
        measurements: crate::l3::maca::ReportMeasurements::Extended(
            crate::l3::maca::Measurements {
                current: Some(vec![20, 21]),
                other: vec![crate::l3::maca::OtherMeasurements {
                    rss: vec![Some(22), None],
                }],
                ..Default::default()
            },
        ),
        ..Default::default()
    }
    .encode()
    .unwrap();
    registration_companion_loop(
        None,
        false,
        ProtocolVersion::Pv2,
        None,
        arq_loop::Mode::Normal,
        Some(report),
        false,
    );
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registration_loop_continues_after_a_startup_tx_underflow() {
    const UNDERFLOW_SLOT: usize = 22;
    registration_loop(
        Some(UNDERFLOW_SLOT),
        false,
        ProtocolVersion::Is136RevA,
        None,
    );
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registration_loop_recovers_missed_slots_after_underflow_and_host_stall() {
    const UNDERFLOW_SLOT: usize = 22;
    registration_loop(Some(UNDERFLOW_SLOT), true, ProtocolVersion::Is136RevA, None);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn later_protocol_handset_registers_through_iq_without_upgrading_the_cell() {
    registration_loop(None, false, ProtocolVersion::Pv7, None);
}
fn registration_loop(
    underflow_after_slot: Option<usize>,
    host_stall: bool,
    protocol_version: ProtocolVersion,
    capability_report: Option<crate::l3::capability_report::CapabilityReport>,
) {
    registration_arq_loop(
        underflow_after_slot,
        host_stall,
        protocol_version,
        capability_report,
        arq_loop::Mode::Normal,
    );
}
fn registration_arq_loop(
    underflow_after_slot: Option<usize>,
    host_stall: bool,
    protocol_version: ProtocolVersion,
    capability_report: Option<crate::l3::capability_report::CapabilityReport>,
    arq_mode: arq_loop::Mode,
) {
    registration_companion_loop(
        underflow_after_slot,
        host_stall,
        protocol_version,
        capability_report,
        arq_mode,
        None,
        false,
    );
}
fn registration_companion_loop(
    underflow_after_slot: Option<usize>,
    host_stall: bool,
    protocol_version: ProtocolVersion,
    capability_report: Option<crate::l3::capability_report::CapabilityReport>,
    arq_mode: arq_loop::Mode,
    maca_report: Option<Vec<u8>>,
    esn_provisioned: bool,
) {
    let _paced_guard = capability_report.as_ref().map(|_| {
        PACED_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    });

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = Database(std::env::temp_dir().join(format!(
        "136bts-serving-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = db.0.clone();
    let mut expected_number = config.subscribers[0].number.clone();
    if esn_provisioned {
        let mut store = crate::core::store::StateStore::open(&db.0).unwrap();
        let provisioning: Vec<_> = config
            .prepare()
            .unwrap()
            .registry
            .subscribers()
            .iter()
            .skip(1)
            .map(|subscriber| subscriber.provisioning.clone())
            .collect();
        store.seed_once(&provisioning).unwrap();
        let mut core = crate::core::messaging::LocalMessaging::from_store(store).unwrap();
        core.save_device(0x10203040, config.subscribers[0].number.clone(), None)
            .unwrap();
        config.subscribers.clear();
    }

    if maca_report.is_some() {
        config.maca = Some(crate::config::MacaConfig {
            current_channels: vec![992],
            other_hyperbands: vec![crate::config::OtherChannels {
                band: crate::config::MeasurementBand::Pcs1900,
                channels: vec![100, 101],
            }],
            ..Default::default()
        });
    }
    if arq_mode == arq_loop::Mode::DropContinue {
        config.subscribers[0].number = "12345678901234567890123456789012".into();
        expected_number = config.subscribers[0].number.clone();
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let mut packets = if let Some(report) = &capability_report {
        let primary = crate::l3::registration::RegistrationRequest {
            registration_type: RegistrationType::PowerUp,
            scm: 15,
            protocol_version,
            options: crate::l3::registration::RegistrationOptions {
                pfc: Some(7),
                ignored_bits: report.options.ignored_bits.clone(),
                sid_report: Some(42),
                number_request: Some(1),
                ..Default::default()
            },
        }
        .encode()
        .unwrap();
        let serial = crate::l3::registration::SerialNumber {
            esn: 0x10203040,
            esnx: Some(0xabcdef),
            s_esn: Some(0x50607080),
            ignored_bits: report.options.ignored_bits.clone(),
            unparsed_bits: vec![],
        }
        .encode()
        .unwrap();
        let bytes = report.encode().unwrap();
        rach::encode(
            BurstFormat::Normal,
            MobileIdentity::Min34(1),
            &[&primary, &serial, &bytes],
        )
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            (
                ACCESS_BLOCK + index as u64 * ACCESS_PATH_BLOCKS,
                burst_packet(frame),
            )
        })
        .collect()
    } else if let Some(report) = &maca_report {
        let primary = AccessMessage::Registration {
            registration_type: RegistrationType::PowerUp,
            scm: 15,
            protocol_version,
        }
        .encode()
        .unwrap();
        rach::encode(
            BurstFormat::Normal,
            MobileIdentity::Min34(1),
            &[&primary, report],
        )
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            (
                ACCESS_BLOCK + index as u64 * ACCESS_PATH_BLOCKS,
                burst_packet(frame),
            )
        })
        .collect()
    } else {
        vec![(ACCESS_BLOCK, packet_version(protocol_version))]
    };
    if arq_mode == arq_loop::Mode::Concurrent {
        let primary = crate::l3::registration::RegistrationRequest {
            registration_type: RegistrationType::PowerUp,
            scm: 15,
            protocol_version,
            options: crate::l3::registration::RegistrationOptions {
                pfc: Some(7),
                sid_report: Some(42),
                ignored_bits: capability_report
                    .as_ref()
                    .unwrap()
                    .options
                    .ignored_bits
                    .clone(),
                number_request: Some(1),
                ..Default::default()
            },
        }
        .encode()
        .unwrap();
        let serial = crate::l3::registration::SerialNumber {
            esn: 0x10203041,
            esnx: Some(0xabcdef),
            s_esn: Some(0x50607081),
            ignored_bits: capability_report
                .as_ref()
                .unwrap()
                .options
                .ignored_bits
                .clone(),
            unparsed_bits: vec![],
        }
        .encode()
        .unwrap();
        let report = capability_report.as_ref().unwrap().encode().unwrap();
        for (index, bits) in rach::encode(
            BurstFormat::Normal,
            MobileIdentity::Min34(2),
            &[&primary, &serial, &report],
        )
        .unwrap()
        .iter()
        .enumerate()
        {
            packets.push((
                ACCESS_BLOCK + 1 + index as u64 * ACCESS_PATH_BLOCKS,
                burst_packet(bits),
            ));
        }
    }
    if capability_report.is_some() && arq_mode == arq_loop::Mode::Normal {
        let bits = rach::encode_arq_status(
            BurstFormat::Normal,
            &rach::ArqStatus {
                partial_echo: 0,
                received_frames: [true; 26],
            },
        )
        .unwrap();
        packets.push((ACCESS_BLOCK + 2, burst_packet(&bits)));
    }
    if capability_report.is_some() && arq_mode != arq_loop::Mode::Normal {
        let quiet_blocks = arq_reuse_policy(&config).unwrap().quiet_blocks();
        for (block, _) in &mut packets {
            *block += quiet_blocks;
        }
    }
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        tx_observation_offset_ticks: 0,
        epoch: None,
        tx: Vec::new(),
        slot_count: 0,
        stop_slots: STOP_SLOTS
            + if capability_report.is_some() {
                arq_reuse_policy(&config).unwrap().quiet_blocks() as usize
                    * CONTROL_SLOT_PERIOD as usize
            } else {
                0
            }
            + match arq_mode {
                arq_loop::Mode::Normal => 0,
                arq_loop::Mode::DropContinue | arq_loop::Mode::Concurrent => {
                    32 * CONTROL_SLOT_PERIOD as usize
                }
                arq_loop::Mode::LoseStatus => 64 * CONTROL_SLOT_PERIOD as usize,
                arq_loop::Mode::Silent => {
                    (super::super::timers::SPACH_MS / 20) as usize * CONTROL_SLOT_PERIOD as usize
                }
            },
        underflow_after_slot,
        stall_after_slot: if host_stall {
            underflow_after_slot
        } else {
            None
        },
        realtime_epoch: (arq_mode == arq_loop::Mode::Silent).then(Instant::now),
        handset_segmented: None,
        handset_sms: None,
        handset_voice: None,
        handset_arq: capability_report
            .as_ref()
            .map(|_| arq_loop::Handsets::new(arq_mode)),
        handset_idle: None,
        traffic_packets: Vec::new(),
        packets,
        tx_active: false,
        rx_active: false,
        shutdown: shutdown.clone(),
    }));
    let prepared = PreparedRadio::from_radio(&config, Box::new(Device(state.clone()))).unwrap();
    let (hub, interface) = crate::operations::Hub::new(&config);
    let summary = run_prepared_with_interface(
        &config,
        prepared,
        &shutdown,
        Some(if arq_mode == arq_loop::Mode::Silent {
            Duration::from_millis(
                super::super::timers::SPACH_MS
                    + 4 * super::super::timers::SUPERFRAME_MS
                    + arq_reuse_policy(&config).unwrap().quiet_ms,
            )
        } else {
            Duration::from_secs(5)
                + if capability_report.is_some() {
                    Duration::from_millis(arq_reuse_policy(&config).unwrap().quiet_ms)
                } else {
                    Duration::ZERO
                }
        }),
        Some(interface),
    )
    .unwrap();
    let operations = hub.snapshot();
    if maca_report.is_some() {
        for (name, value) in [
            ("MACA Cellular800 channel 992 RSS", Some("-71 dBm")),
            ("MACA Pcs1900 channel 100 RSS", Some("-69 dBm")),
            ("MACA Pcs1900 channel 101 RSS", None),
        ] {
            assert!(
                operations
                    .subscribers
                    .iter()
                    .flat_map(|subscriber| &subscriber.measurements)
                    .any(|reading| reading.name == name && reading.value.as_deref() == value),
                "{name}"
            );
        }
        assert!(
            operations
                .subscribers
                .iter()
                .flat_map(|subscriber| &subscriber.measurements)
                .any(|reading| reading.name
                    == format!("MACA Cellular800 channel {} RSS", config.cell.channel)
                    && reading.value.as_deref() == Some("-73 dBm"))
        );
        assert!(
            operations
                .air_messages
                .iter()
                .any(|message| message.direction == "Reverse" && message.name == "MACA Report")
        );
    }
    assert_eq!(
        operations.phase,
        crate::operations::proto::CellPhase::Stopped
    );
    assert!(
        operations
            .air_messages
            .iter()
            .any(|message| message.direction == "Reverse" && message.name == "Registration")
    );
    assert_eq!(
        operations.air_messages.iter().any(
            |message| message.direction == "Forward" && message.name == "Registration response"
        ),
        arq_mode != arq_loop::Mode::Silent
    );
    if arq_mode == arq_loop::Mode::Silent {
        assert!(
            operations
                .air_messages
                .iter()
                .any(|message| message.name == "ARQ transaction"
                    && message.status == "Expired without ARQ confirmation")
        );
    }
    assert_eq!(
        operations
            .subscribers
            .iter()
            .filter(|subscriber| subscriber.registered)
            .count(),
        if arq_mode == arq_loop::Mode::Concurrent {
            2
        } else {
            1
        }
    );
    let subscriber = operations
        .subscribers
        .iter()
        .find(|subscriber| subscriber.registered)
        .unwrap();
    assert_eq!(subscriber.identity, "Min34(1)");
    if let Some(report) = &capability_report {
        for (name, expected) in [
            ("Registration requested PFC", "8"),
            ("Assigned PFC", "1"),
            ("Reported previous SID", "42"),
            ("Reported ESNX", "0xABCDEF"),
            ("Reported S-ESN", "0x50607080"),
            ("Directory number requested", "true"),
        ] {
            assert!(
                subscriber
                    .measurements
                    .iter()
                    .any(|metric| metric.name == name && metric.value.as_deref() == Some(expected)),
                "{name}"
            );
        }
        assert_eq!(subscriber.esn, Some(0x10203040));
        for name in ["Registration", "Serial number", "Capability report"] {
            assert!(
                operations.air_messages.iter().any(|message| {
                    message.direction == "Reverse"
                        && message.name == name
                        && message.decoded.contains("Ignored optional bits=12")
                }),
                "{name} did not identify its skipped optional IE"
            );
        }
        assert!(
            operations
                .air_messages
                .iter()
                .any(|message| message.name == "Serial number"
                    && message.direction == "Reverse"
                    && message.decoded.contains("ESNX=0xABCDEF")
                    && message.decoded.contains("S-ESN=0x50607080"))
        );
        for (name, expected) in [
            ("Handset model", "0x34"),
            ("Handset manufacturer code", "0x20"),
            ("Reported voice coder map", "0x1F"),
            ("Reported maximum PFC", "6"),
            ("DADS support", "true"),
            ("ADVICE support", "false"),
            ("Reported positioning map", "0x0F"),
            ("Reported extended bands", "0x01"),
            ("Reported extended character map", "0xF000"),
            ("Reported encryption algorithms", "0:0x0,1:0x1"),
            (
                "Reported voice modes (coder/modulation/interleaving)",
                "1/0/1,2/0/1,3/0/1,5/0/1,6/0/1",
            ),
        ] {
            assert!(
                subscriber
                    .measurements
                    .iter()
                    .any(|metric| metric.name == name && metric.value.as_deref() == Some(expected)),
                "{name}: {:?}",
                subscriber.measurements
            );
        }
        assert!(operations.air_messages.iter().any(|message| {
            message.direction == "Reverse"
                && message.name == "Capability report"
                && message
                    .decoded
                    .contains(&format!("{:?}", report.protocol_version))
                && message.decoded.contains("LanguagesExt=0xF000")
                && message
                    .decoded
                    .contains("Modes(coder/modulation/interleaving)=1/0/1,2/0/1,3/0/1,5/0/1,6/0/1")
        }));
    }

    assert!(
        subscriber
            .measurements
            .iter()
            .any(|reading| reading.name == "Access frequency offset" && reading.value.is_some())
    );
    assert!(
        operations
            .radio
            .iter()
            .any(|reading| reading.name == "Stop acknowledged"
                && reading.value.as_deref() == Some("true"))
    );
    assert_eq!(
        summary.completed_registrations,
        if arq_mode == arq_loop::Mode::Concurrent {
            2
        } else {
            u64::from(arq_mode != arq_loop::Mode::Silent)
        }
    );
    assert!(summary.stop_acknowledged);
    let state = state.lock().unwrap();
    assert!(!state.tx_active);
    assert!(!state.rx_active);
    let samples: Vec<_> = state
        .tx
        .iter()
        .map(|sample| Symbol {
            i: sample.re as f64,
            q: sample.im as f64,
        })
        .collect();
    let controls = decode_forward_control(&samples, RATE as f64, LogicalChannel::Spach, 1).unwrap();
    let mut receiver = SpachReceiver::default();
    let mut arq_receiver =
        crate::link::spach::arq::ArqReassembler::try_new(MobileIdentity::Min34(1)).unwrap();
    let mut accepted = 0;
    for burst in controls {
        let message = if crate::link::spach::arq::decode(&burst.fields.block.information).is_ok() {
            match arq_receiver.push(&burst.fields.block.information).unwrap() {
                crate::link::spach::arq::Reception::Accepted { message, .. } => message,
                _ => None,
            }
        } else {
            receiver
                .push(&burst.fields.block.information)
                .ok()
                .flatten()
        };
        if let Some(message) = message
            && message.channel == crate::link::spach::Channel::Arch
            && message.identity == MobileIdentity::Min34(1)
        {
            assert_eq!(
                crate::l3::registration::RegistrationResponse::decode_prefix(&message.message)
                    .unwrap()
                    .message,
                RegistrationResponse::Accept
            );
            if capability_report.is_some() {
                let accept =
                    crate::l3::registration::AcceptOptions::decode(&message.message).unwrap();
                assert_eq!(accept.pfc, Some(0));
                assert_eq!(
                    accept.directory_address.unwrap().address,
                    crate::l3::address::AddressValue::Tbcd(expected_number.clone())
                );
            }
            accepted += 1;
        }
    }
    if capability_report.is_some() {
        let peer = state.handset_arq.as_ref().unwrap();
        assert!(
            peer.first_begin_block.unwrap() >= arq_reuse_policy(&config).unwrap().quiet_blocks()
        );
        if arq_mode == arq_loop::Mode::Normal {
            assert!(
                operations
                    .air_messages
                    .iter()
                    .any(|message| message.name == "SPACH ARQ status"
                        && message.status == "Unknown or stale PEA"
                        && message.identity.is_empty())
            );
        }
        assert_eq!(
            peer.applications,
            if arq_mode == arq_loop::Mode::Concurrent {
                2
            } else {
                1
            }
        );
        assert!(
            peer.max_interruption < 32,
            "{} intervening SPACH frames",
            peer.max_interruption
        );
        assert!(!peer.messages.is_empty());
        if arq_mode == arq_loop::Mode::Silent {
            assert_eq!(peer.confirmed, 0);
        } else {
            assert!(peer.confirmed > 0);
            assert!(operations.air_messages.iter().any(|message| message.name
                == "SPACH ARQ status"
                && message.identity == "Min34(1)"));
        }
        if arq_mode == arq_loop::Mode::DropContinue {
            assert_eq!(peer.echoes.len(), 1);
            assert!(peer.statuses >= 2);
        }
        if arq_mode == arq_loop::Mode::Concurrent {
            let beginnings: Vec<_> = ["Min34(1)", "Min34(2)"]
                .into_iter()
                .map(|identity| {
                    operations
                        .air_messages
                        .iter()
                        .find(|message| message.name == "ARQ BEGIN" && message.identity == identity)
                        .unwrap()
                        .sequence
                })
                .collect();
            let first_status = operations
                .air_messages
                .iter()
                .filter(|message| {
                    message.name == "SPACH ARQ status"
                        && message.status == "Matched active ARQ transaction"
                })
                .map(|message| message.sequence)
                .min()
                .unwrap();
            assert!(
                beginnings.iter().all(|sequence| *sequence < first_status),
                "transactions did not overlap"
            );
            assert_eq!(peer.echoes.len(), 2);
            assert!(operations.air_messages.iter().any(|message| message.name
                == "SPACH ARQ status"
                && message.identity == "Min34(2)"));
        }
        if arq_mode == arq_loop::Mode::LoseStatus {
            assert!(peer.echoes.len() >= 2);
        }
        assert!(accepted > 0);
    } else {
        assert_eq!(accepted, 1);
    }
    let broadcasts =
        decode_forward_control(&samples, RATE as f64, LogicalChannel::Fbcch, 0).unwrap();
    let mut receiver = crate::link::broadcast::FbcchReceiver::default();
    let mut serial_requests = 0;
    let mut menus = 0;
    let mut system_identities = 0;
    let mut registration_requests = 0;
    let mut maca_broadcasts = 0;
    let mut observed_fbcch_slots = None;
    for burst in broadcasts {
        for message in receiver
            .push(
                burst
                    .fields
                    .block
                    .information
                    .as_slice()
                    .try_into()
                    .unwrap(),
            )
            .unwrap()
        {
            if let Ok(decoded) = crate::l3::broadcast::decode_prefix(&message)
                && let crate::l3::broadcast::MandatoryBroadcast::AccessParameters(parameters) =
                    decoded.message
            {
                assert!(parameters.s);
                serial_requests += 1;
            }
            if let Ok(decoded) = crate::l3::broadcast::decode_prefix(&message)
                && let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) =
                    decoded.message
            {
                observed_fbcch_slots = Some(structure.fbcch_slots);
            }
            if let Ok(message) = crate::l3::maca::broadcast::MacaBroadcast::decode(&message) {
                assert_eq!(
                    message.status,
                    crate::l3::maca::broadcast::MacaStatus::ShortTerm
                );
                assert!(message.triggers.registration);
                maca_broadcasts += 1;
            }
            if let Ok(decoded) = crate::l3::broadcast::decode_prefix(&message)
                && let crate::l3::broadcast::MandatoryBroadcast::RegistrationParameters(parameters) =
                    decoded.message
            {
                assert!(parameters.capability_request);
                registration_requests += 1;
            }
            if let Ok(decoded) = crate::l3::broadcast::decode_prefix(&message)
                && let crate::l3::broadcast::MandatoryBroadcast::SystemIdentity(identity) =
                    decoded.message
            {
                assert_eq!(identity.protocol_version, ProtocolVersion::Pv2);
                system_identities += 1;
            }
            if message.first() == Some(&crate::l3::service_menu::MESSAGE_TYPE) {
                let decoded =
                    crate::l3::service_menu::decode_prefix(&message, ProtocolVersion::Pv2).unwrap();
                assert_eq!(decoded.message.menu_map, 4);
                assert_eq!(decoded.message.voice_coder_map, 0x03);
                assert_eq!(decoded.message.sms_map, 3);
                let flags = decoded.message.pv1.as_ref().unwrap();
                assert!(flags.latin1 && flags.bmp);
                assert!(!flags.latin_hebrew && !flags.async_data && !flags.g3_fax && !flags.dads);
                assert_eq!(decoded.message.sms_broadcast, Some(false));
                menus += 1;
            }
        }
    }
    assert!(menus > 0);
    assert!(system_identities > 0);
    assert!(registration_requests > 0);
    assert!(observed_fbcch_slots.is_some());
    let extended = decode_forward_control(&samples, RATE as f64, LogicalChannel::Ebcch, 1).unwrap();
    let mut cycles = 0;
    for burst in extended {
        if Some(burst.fields.superframe_phase) != observed_fbcch_slots {
            continue;
        }
        let information = burst
            .fields
            .block
            .information
            .as_slice()
            .try_into()
            .unwrap();
        let messages = crate::link::broadcast::decode_ebcch_begin(information)
            .unwrap()
            .1;
        assert_eq!(messages, [vec![0x0d, 0x00], vec![0x0f, 0x40]]);
        cycles += 1;
    }
    assert!(cycles > 0);

    assert!(serial_requests > 0);
    assert_eq!(maca_broadcasts > 0, maca_report.is_some());
    let traffic = crate::phy::acquisition::decode_forward_traffic(&samples, RATE as f64).unwrap();
    let unused: Vec<_> = traffic
        .iter()
        .filter(|burst| {
            matches!(
                burst.fields.fields.sync,
                SyncWord::Two | SyncWord::Three | SyncWord::Five | SyncWord::Six
            )
        })
        .collect();
    assert!(!unused.is_empty());
    assert!(unused.iter().all(|burst|burst.fields.fields.color.dvcc==config.cell.dvcc^UNASSIGNED_COLOR_MASK));
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn generated_sms_submission_iq_is_durably_stored_and_acknowledged() {
    generated_sms_submission_loop(false);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn generated_external_sms_iq_is_accepted_while_hub_is_unresponsive() {
    generated_sms_submission_loop(true);
}
fn generated_sms_submission_loop(external: bool) {
    use crate::l3::{
        address::{AddressInfo, AddressValue},
        relay::{
            Direction, HigherLayerProtocol, RelayMessage,
            addressing::{AddressedRelay, RelayAddresses},
        },
        sms::{Metadata, SmsMessage, UserData},
    };
    const SMS_ACCESS_BLOCK: u64 = 20;
    const TRANSACTION_ID: u8 = 4;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = Database(std::env::temp_dir().join(format!(
        "136bts-serving-sms-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = db.0.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    if external {
        config.sms_hub = crate::sms_hub::SmsHubConfig {
            enabled: true,
            role: crate::sms_hub::BindRole::Client,
            hub_addr: listener.local_addr().unwrap(),
            system_id: "test-site".into(),
            password: "testpass".into(),
            ..Default::default()
        };
    }
    let payload = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 12,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(b"serving loop SMS".to_vec()),
    }
    .encode()
    .unwrap();
    let message = AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: TRANSACTION_ID,
            protocol: HigherLayerProtocol::Sms,
            payload,
        },
        addresses: RelayAddresses {
            destination: Some(AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd(if external { "2000" } else { "1002" }.into()),
            }),
            ..Default::default()
        },
    }
    .encode(Direction::Reverse)
    .unwrap();
    let mut packets = vec![(ACCESS_BLOCK, packet())];
    packets.extend(
        access_packets(MobileIdentity::Min34(1), &message)
            .into_iter()
            .map(|(offset, packet)| (SMS_ACCESS_BLOCK + offset, packet)),
    );
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        tx_observation_offset_ticks: 0,
        epoch: None,
        tx: Vec::new(),
        slot_count: 0,
        stop_slots: STOP_SLOTS,
        underflow_after_slot: None,
        stall_after_slot: None,
        realtime_epoch: None,
        handset_segmented: None,
        handset_sms: None,
        handset_voice: None,
        handset_arq: None,
        handset_idle: None,
        traffic_packets: Vec::new(),
        packets,
        tx_active: false,
        rx_active: false,
        shutdown: shutdown.clone(),
    }));
    let prepared = PreparedRadio::from_radio(&config, Box::new(Device(state.clone()))).unwrap();
    let summary = run_prepared(&config, prepared, &shutdown, Some(Duration::from_secs(5))).unwrap();
    assert_eq!(summary.completed_registrations, 1);
    assert!(summary.stop_acknowledged);
    let store = crate::core::store::StateStore::open(&db.0).unwrap();
    if external {
        assert_eq!(store.hub_queue_depth().unwrap(), 1);
        let messages = store.operation_messages(10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender, "1001");
        assert_eq!(messages[0].recipient, "2000");
        assert!(
            store
                .pending_for(MobileIdentity::Min34(2))
                .unwrap()
                .is_empty()
        );
    } else {
        let queued = store.pending_for(MobileIdentity::Min34(2)).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].sender_number, "1001");
    }
    let state = state.lock().unwrap();
    assert!(!state.tx_active);
    assert!(!state.rx_active);
    let samples: Vec<_> = state
        .tx
        .iter()
        .map(|sample| Symbol {
            i: f64::from(sample.re),
            q: f64::from(sample.im),
        })
        .collect();
    let controls = decode_forward_control(&samples, RATE as f64, LogicalChannel::Spach, 1).unwrap();
    let mut receiver = SpachReceiver::default();
    let mut accepted = 0;
    for burst in controls {
        if let Ok(Some(message)) = receiver.push(&burst.fields.block.information)
            && message.channel == crate::link::spach::Channel::Smsch
            && message.identity == MobileIdentity::Min34(1)
        {
            let response =
                crate::l3::relay::decode_prefix(&message.message, Direction::Forward).unwrap();
            assert_eq!(
                response.message,
                RelayMessage::Accept {
                    transaction_id: TRANSACTION_ID
                }
            );
            accepted += 1;
        }
    }
    assert_eq!(accepted, 1);
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registration_with_capability_report_and_serial_number_crosses_iq_and_reaches_subscriber_details()
{
    registration_loop(
        None,
        false,
        ProtocolVersion::Pv5,
        Some(extended_registration_report()),
    );
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn esn_only_subscriber_learns_mobile_identity_through_generated_registration_iq() {
    registration_companion_loop(
        None,
        false,
        ProtocolVersion::Pv5,
        Some(extended_registration_report()),
        arq_loop::Mode::Normal,
        None,
        true,
    );
}
fn extended_registration_report() -> crate::l3::capability_report::CapabilityReport {
    use crate::l3::capability_report::{CapabilityReport, ReportOptions, SupportedServices};
    CapabilityReport {
        protocol_version: ProtocolVersion::Pv7,
        scm: 15,
        software_vintage: 2,
        firmware_vintage: 3,
        model: 4,
        manufacturer: 0x20,
        max_supported_pfc: 5,
        supported_bands: 3,
        services: SupportedServices {
            ira: true,
            ..Default::default()
        },
        options: ReportOptions {
            complete_model: Some(0x34),
            ignored_bits: vec![
                true, false, false, false, false, false, false, false, false, false, false, false,
            ],
            extensions: crate::l3::capability_report::ReportExtensions {
                dads: Some(true),
                advice: Some(false),
                encryption: Some(crate::l3::capability_report::EncryptionMap {
                    algorithms: [Some(0), Some(1), None, None, None, None, None, None],
                }),
                voice: Some(crate::l3::capability_report::ExtendedVoice {
                    privacy: 0,
                    modes: [1, 2, 3, 5, 6]
                        .into_iter()
                        .map(|coder| crate::l3::capability_report::VoiceMode {
                            coder,
                            modulation: 0,
                            interleaving: 1,
                        })
                        .collect(),
                }),
                positioning: Some(15),
                frequency_bands: Some(1),
                multilingual: Some(0xf000),
            },
            ..Default::default()
        },
    }
}

mod idle_loop;

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn queued_transmit_lead_fits_the_recovered_transaction_guard() {
    let queue_ms =
        (QUEUED_SLOTS as f64 * SYMBOLS_PER_SLOT as f64 * 1000.0 / SYMBOL_RATE_HZ).ceil() as u64;
    let modulator =
        crate::phy::forward::ForwardModulator::try_new(crate::config::SAMPLES_PER_SYMBOL).unwrap();
    let pulse_tail_ms =
        (modulator.tail_samples() as u64 * 1000).div_ceil(crate::config::SAMPLE_RATE_HZ as u64);
    assert!(START_LEAD_MS + queue_ms + pulse_tail_ms < super::super::timers::SUPERFRAME_MS);
}
