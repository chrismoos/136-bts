pub mod access_rx;
pub(crate) mod buffered_receiver;
pub mod carrier;
pub mod duplex;
pub mod forward_tx;
pub(crate) mod receive_window;
pub mod receiver;
pub mod timed_tx;
pub mod tx_timeline;

use crate::config::{Config, ConfigError};
use sdr_hal::{Radio, RadioRx, RadioTx, error::Error as HalError};
use std::{error::Error, fmt};

const CHANNEL_BANDWIDTH_HZ: usize = 30_000;
// The HAL rounds timestamp clocks to integral Hz, which also affects clock-derived rate readback.
const CLOCK_ROUNDING_ERROR_HZ: f64 = 0.5;
#[derive(Debug)]
pub enum RadioError {
    Config(ConfigError),
    Hal(Box<HalError>),
    AppliedTxRate {
        requested_hz: usize,
        applied_hz: f64,
    },
    FractionalTxRate {
        requested_hz: usize,
        applied_hz: f64,
    },
    AppliedRxRate {
        requested_hz: usize,
        applied_hz: Option<f64>,
    },
    TimedTx,
    ClockRate,
    MissingRx,
    TxStream(timed_tx::Error),
    RxStream(receiver::Error),
    StreamClock {
        expected_tx_hz: u64,
        actual_tx_hz: u64,
        expected_rx_hz: u64,
        actual_rx_hz: u64,
    },
    Stop {
        tx: Option<Box<HalError>>,
        rx: Option<Box<HalError>>,
    },
}
impl fmt::Display for RadioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "radio preparation failed: {self:?}")
    }
}
impl Error for RadioError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadioSettings {
    pub applied_tx_sample_rate_hz: usize,
    pub applied_rx_sample_rate_hz: usize,
    pub tx_tick_rate_hz: u64,
    pub rx_tick_rate_hz: u64,
}
pub struct PreparedRadio {
    tx: Option<Box<dyn RadioTx>>,
    rx: Option<Box<dyn RadioRx>>,
    settings: RadioSettings,
    stop_acknowledged: bool,
}
pub struct RadioStreams {
    pub tx: timed_tx::TimedTx,
    pub rx: receiver::Receiver,
    pub settings: RadioSettings,
}
impl fmt::Debug for PreparedRadio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedRadio")
            .field("settings", &self.settings)
            .field("stop_acknowledged", &self.stop_acknowledged)
            .finish_non_exhaustive()
    }
}
impl PreparedRadio {
    pub fn open(config: &Config) -> Result<Self, RadioError> {
        config.prepare().map_err(RadioError::Config)?;
        let mut settings = config.radio.clone();
        settings.tx_sample_rate_hz = config.sample_rate_hz();
        settings.tx_bandwidth_hz = Some(settings.tx_bandwidth_hz.unwrap_or(CHANNEL_BANDWIDTH_HZ));
        settings.rx_sample_rate_hz = Some(config.sample_rate_hz());
        settings.rx_bandwidth_hz = Some(settings.rx_bandwidth_hz.unwrap_or(CHANNEL_BANDWIDTH_HZ));
        let radio = settings
            .open()
            .map_err(|error| RadioError::Hal(Box::new(error)))?;
        validate_rx_rate(
            radio.rx_sample_rate_hz(),
            config.sample_rate_hz(),
            radio.tick_rate(),
        )?;
        Self::configure(config, radio)
    }
    pub fn from_radio(config: &Config, radio: Box<dyn Radio>) -> Result<Self, RadioError> {
        config.prepare().map_err(RadioError::Config)?;
        validate_rx_rate(
            radio.rx_sample_rate_hz(),
            config.sample_rate_hz(),
            radio.tick_rate(),
        )?;
        Self::configure(config, radio)
    }
    fn configure(config: &Config, mut radio: Box<dyn Radio>) -> Result<Self, RadioError> {
        let prepared = config.prepare().map_err(RadioError::Config)?;
        let requested_hz = config.sample_rate_hz();
        let applied_hz = radio
            .set_tx_sample_rate(requested_hz as f64)
            .map_err(|error| RadioError::Hal(Box::new(error)))?;
        if radio.tick_rate() == 0 {
            return Err(RadioError::ClockRate);
        }
        if !rate_matches_clock(applied_hz, requested_hz, radio.tick_rate()) {
            return Err(RadioError::AppliedTxRate {
                requested_hz,
                applied_hz,
            });
        }
        let actual_tx_hz = radio.tx_sample_rate_hz();
        if !rate_matches_clock(actual_tx_hz, requested_hz, radio.tick_rate()) {
            return Err(RadioError::FractionalTxRate {
                requested_hz,
                applied_hz: actual_tx_hz,
            });
        }
        if !radio.supports_timed_tx() {
            return Err(RadioError::TimedTx);
        }
        tracing::info!(
            nominal_tx_sample_rate_hz = requested_hz,
            nominal_rx_sample_rate_hz = requested_hz,
            tx_sample_rate_readback_hz = actual_tx_hz,
            rx_sample_rate_readback_hz = ?radio.rx_sample_rate_hz(),
            timestamp_clock_hz = radio.tick_rate(),
            "radio sample rates validated"
        );
        radio
            .set_tx_bandwidth(config.radio.tx_bandwidth_hz.unwrap_or(CHANNEL_BANDWIDTH_HZ))
            .map_err(|error| RadioError::Hal(Box::new(error)))?;
        let (tx, rx) = radio
            .split()
            .map_err(|error| RadioError::Hal(Box::new(error)))?;
        let tx_tick_rate_hz = tx.tick_rate();
        let rx_tick_rate_hz = rx.as_ref().map_or(0, |rx| rx.tick_rate());
        let mut session = Self {
            tx: Some(tx),
            rx,
            settings: RadioSettings {
                applied_tx_sample_rate_hz: requested_hz,
                applied_rx_sample_rate_hz: requested_hz,
                tx_tick_rate_hz,
                rx_tick_rate_hz,
            },
            stop_acknowledged: false,
        };
        session.stop()?;
        let Some(rx) = session.rx.as_mut() else {
            return Err(RadioError::MissingRx);
        };
        if !session
            .tx
            .as_ref()
            .expect("TX installed before radio configuration")
            .supports_timed_tx()
        {
            return Err(RadioError::TimedTx);
        }
        if tx_tick_rate_hz == 0 || rx_tick_rate_hz == 0 {
            return Err(RadioError::ClockRate);
        }
        session
            .tx
            .as_mut()
            .expect("TX installed before radio configuration")
            .set_tx_frequency_hz(prepared.frequencies.forward_hz as f64)
            .map_err(|error| RadioError::Hal(Box::new(error)))?;
        rx.set_rx_frequency(prepared.frequencies.reverse_hz as f64)
            .map_err(|error| RadioError::Hal(Box::new(error)))?;
        Ok(session)
    }
    pub fn settings(&self) -> RadioSettings {
        self.settings
    }
    /// Both stream owners remain inactive. On construction failure, each retained or transferred half requests stop.
    pub fn into_streams(
        mut self,
        tx_limits: timed_tx::Limits,
        rx_limits: receiver::Limits,
    ) -> Result<RadioStreams, RadioError> {
        self.stop()?;
        if self.rx.is_none() {
            return Err(RadioError::MissingRx);
        }
        let actual_tx_hz = self
            .tx
            .as_ref()
            .expect("TX retained until handoff")
            .tick_rate();
        let actual_rx_hz = self
            .rx
            .as_ref()
            .expect("RX presence checked above")
            .tick_rate();
        if actual_tx_hz != self.settings.tx_tick_rate_hz
            || actual_rx_hz != self.settings.rx_tick_rate_hz
        {
            return Err(RadioError::StreamClock {
                expected_tx_hz: self.settings.tx_tick_rate_hz,
                actual_tx_hz,
                expected_rx_hz: self.settings.rx_tick_rate_hz,
                actual_rx_hz,
            });
        }
        let tx = timed_tx::TimedTx::try_new(
            self.tx
                .take()
                .expect("prepared radio retains TX until handoff"),
            self.settings.applied_tx_sample_rate_hz as u64,
            tx_limits,
        )
        .map_err(RadioError::TxStream)?;
        let rx = receiver::Receiver::try_new(
            self.rx.take().expect("RX presence checked before handoff"),
            self.settings.applied_rx_sample_rate_hz as u64,
            rx_limits,
        )
        .map_err(RadioError::RxStream)?;
        Ok(RadioStreams {
            tx,
            rx,
            settings: self.settings,
        })
    }
    /// Successful disable requests do not constitute a hardware TX-state query.
    pub fn stop_acknowledged(&self) -> bool {
        self.stop_acknowledged
    }
    pub fn stop(&mut self) -> Result<(), RadioError> {
        let tx = self
            .tx
            .as_mut()
            .and_then(|tx| tx.stop().err().map(Box::new));
        let rx = self
            .rx
            .as_mut()
            .and_then(|rx| rx.stop().err().map(Box::new));
        self.stop_acknowledged = tx.is_none() && rx.is_none();
        if self.stop_acknowledged {
            Ok(())
        } else {
            Err(RadioError::Stop { tx, rx })
        }
    }
}
fn rate_matches_clock(applied_hz: f64, requested_hz: usize, tick_rate_hz: u64) -> bool {
    applied_hz.is_finite()
        && applied_hz > 0.0
        && requested_hz > 0
        && tick_rate_hz > 0
        && (applied_hz - requested_hz as f64).abs()
            <= requested_hz as f64 * CLOCK_ROUNDING_ERROR_HZ / tick_rate_hz as f64
}
fn validate_rx_rate(
    applied_hz: Option<f64>,
    requested_hz: usize,
    tick_rate_hz: u64,
) -> Result<(), RadioError> {
    if !applied_hz.is_some_and(|rate| rate_matches_clock(rate, requested_hz, tick_rate_hz)) {
        return Err(RadioError::AppliedRxRate {
            requested_hz,
            applied_hz,
        });
    }
    Ok(())
}
impl Drop for PreparedRadio {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("radio cleanup failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdr_hal::NoopRadio;
    fn config() -> Config {
        Config::from_yaml(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/two-phones.yaml"
        )))
        .unwrap()
    }
    fn config_with_sample_rate(rate_hz: usize) -> String {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/two-phones.yaml"
        ))
        .replace(
            "kind: noop",
            &format!("kind: noop\n  tx_sample_rate_hz: {rate_hz}"),
        )
    }
    #[test]
    fn configured_radio_rate_sets_the_modem_rate() {
        let config = Config::from_yaml(&config_with_sample_rate(777_600)).unwrap();
        let mut session = PreparedRadio::open(&config).unwrap();
        assert_eq!(session.settings().applied_tx_sample_rate_hz, 777_600);
        assert_eq!(session.settings().applied_rx_sample_rate_hz, 777_600);
        session.stop().unwrap();
        assert!(matches!(
            Config::from_yaml(&config_with_sample_rate(390_000)),
            Err(crate::config::ConfigError::Field("radio.tx_sample_rate_hz"))
        ));
    }
    #[test]
    fn noop_factory_prepares_both_streams_and_acknowledges_explicit_stop() {
        let config = config();
        let mut session = PreparedRadio::open(&config).unwrap();
        assert_eq!(
            session.settings(),
            RadioSettings {
                applied_tx_sample_rate_hz: crate::config::SAMPLE_RATE_HZ,
                applied_rx_sample_rate_hz: crate::config::SAMPLE_RATE_HZ,
                tx_tick_rate_hz: 1_000_000_000,
                rx_tick_rate_hz: 1_000_000_000,
            }
        );
        assert!(session.stop_acknowledged());
        session.stop().unwrap();
        assert!(session.stop_acknowledged());
    }
    #[test]
    fn rx_readback_rejects_wrong_rates_and_missing_or_nonfinite_values() {
        let requested_hz = crate::config::SAMPLE_RATE_HZ;
        validate_rx_rate(Some(requested_hz as f64), requested_hz, 1_000_000_000).unwrap();
        for applied in [
            None,
            Some(requested_hz as f64 + 0.25),
            Some(requested_hz as f64 - 1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(0.0),
        ] {
            assert!(matches!(
                validate_rx_rate(applied, requested_hz, 1_000_000_000),
                Err(RadioError::AppliedRxRate { .. })
            ));
        }
    }
    #[test]
    fn uhd_readback_within_half_hz_clock_rounding_is_accepted_for_rx_and_tx() {
        const RATE_HZ: usize = 194400;
        const CLOCK_HZ: u64 = 24883200;
        const READBACK_HZ: f64 = 194400.0019746668;
        validate_rx_rate(Some(READBACK_HZ), RATE_HZ, CLOCK_HZ).unwrap();
        assert!(rate_matches_clock(READBACK_HZ, RATE_HZ, CLOCK_HZ));
        let bound = RATE_HZ as f64 * CLOCK_ROUNDING_ERROR_HZ / CLOCK_HZ as f64;
        for sign in [-1.0, 1.0] {
            assert!(rate_matches_clock(
                RATE_HZ as f64 + sign * bound,
                RATE_HZ,
                CLOCK_HZ
            ));
            let outside = RATE_HZ as f64 + sign * bound * 2.0;
            assert!(!rate_matches_clock(outside, RATE_HZ, CLOCK_HZ));
            assert!(validate_rx_rate(Some(outside), RATE_HZ, CLOCK_HZ).is_err());
        }
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
            assert!(!rate_matches_clock(bad, RATE_HZ, CLOCK_HZ));
        }
        assert!(!rate_matches_clock(READBACK_HZ, RATE_HZ, 0));
    }
    #[test]
    fn tx_preparation_accepts_the_observed_uhd_clock_rounding_error() {
        const CLOCK_HZ: u64 = 24883200;
        const READBACK_ERROR_HZ: f64 = 0.0019746668;
        let mut device = Device::new();
        device.tick_rate_hz = CLOCK_HZ;
        device.tx_fraction_hz = READBACK_ERROR_HZ;
        device
            .noop
            .setup_rx(0, "", 388800.0, 30000.0, None)
            .unwrap();
        let prepared = PreparedRadio::from_radio(&config(), Box::new(device)).unwrap();
        assert!(prepared.stop_acknowledged());
        assert_eq!(prepared.settings().applied_tx_sample_rate_hz, 388800);
    }
    struct Device {
        noop: NoopRadio,
        tx_rate_offset_hz: usize,
        tx_setter_fraction_hz: f64,
        tx_fraction_hz: f64,
        bandwidth: std::sync::Arc<std::sync::Mutex<Option<usize>>>,
        timed: bool,
        tick_rate_hz: u64,
    }
    impl Device {
        fn new() -> Self {
            Self {
                noop: NoopRadio::new(388_800),
                tx_rate_offset_hz: 0,
                tx_setter_fraction_hz: 0.0,
                tx_fraction_hz: 0.0,
                bandwidth: std::sync::Arc::new(std::sync::Mutex::new(None)),
                timed: true,
                tick_rate_hz: 1_000_000_000,
            }
        }
    }
    impl Radio for Device {
        fn tx_sample_rate_hz(&self) -> f64 {
            self.noop.tx_sample_rate_hz() + self.tx_fraction_hz
        }
        fn rx_sample_rate_hz(&self) -> Option<f64> {
            self.noop.rx_sample_rate_hz()
        }

        fn supports_timed_tx(&self) -> bool {
            self.timed
        }
        fn tick_rate(&self) -> u64 {
            self.tick_rate_hz
        }
        fn set_tx_frequency(&mut self, center_frequency: usize) -> Result<(), HalError> {
            self.noop.set_tx_frequency(center_frequency)
        }
        fn set_tx_sample_rate(&mut self, rate: f64) -> Result<f64, HalError> {
            self.noop.set_tx_sample_rate(
                rate + self.tx_rate_offset_hz as f64 + self.tx_setter_fraction_hz,
            )
        }
        fn set_tx_bandwidth(&mut self, bandwidth: usize) -> Result<(), HalError> {
            *self.bandwidth.lock().unwrap() = Some(bandwidth);
            self.noop.set_tx_bandwidth(bandwidth)
        }
        fn split(
            self: Box<Self>,
        ) -> Result<(Box<dyn RadioTx>, Option<Box<dyn RadioRx>>), HalError> {
            Box::new(self.noop).split()
        }
    }
    #[test]
    fn configured_analog_bandwidth_is_not_replaced_by_channel_spacing() {
        const ANALOG_BANDWIDTH_HZ: usize = 200000;
        let mut config = config();
        config.radio.tx_bandwidth_hz = Some(ANALOG_BANDWIDTH_HZ);
        config.radio.rx_bandwidth_hz = Some(ANALOG_BANDWIDTH_HZ);
        let mut device = Device::new();
        device
            .noop
            .setup_rx(0, "", 388800.0, ANALOG_BANDWIDTH_HZ as f64, None)
            .unwrap();
        let bandwidth = device.bandwidth.clone();
        let prepared = PreparedRadio::from_radio(&config, Box::new(device)).unwrap();
        assert_eq!(*bandwidth.lock().unwrap(), Some(ANALOG_BANDWIDTH_HZ));
        assert!(prepared.stop_acknowledged());
    }
    #[test]
    fn synthesizer_rate_error_within_one_clock_tick_is_accepted() {
        let mut device = Device::new();
        device.tick_rate_hz = 24_883_200;
        device.tx_setter_fraction_hz = 0.0039493336;
        device
            .noop
            .setup_rx(0, "", 388800.0, CHANNEL_BANDWIDTH_HZ as f64, None)
            .unwrap();
        let prepared = PreparedRadio::from_radio(&config(), Box::new(device)).unwrap();
        assert_eq!(prepared.settings().applied_tx_sample_rate_hz, 388800);
    }
    #[test]
    fn rounded_tx_rate_is_rejected_before_stream_split() {
        let config = config();
        let mut device = Device::new();
        device.tx_rate_offset_hz = 1;
        assert!(matches!(
            PreparedRadio::configure(&config, Box::new(device)),
            Err(RadioError::AppliedTxRate { .. })
        ));
    }
    #[test]
    fn fractional_tx_readback_is_rejected_even_when_legacy_setter_rounds_to_requested_rate() {
        let mut device = Device::new();
        device.tx_fraction_hz = -0.25;
        assert!(matches!(
            PreparedRadio::configure(&config(), Box::new(device)),
            Err(RadioError::FractionalTxRate { .. })
        ));
    }
    #[test]
    fn untimed_device_zero_clock_and_missing_rx_are_rejected() {
        let config = config();
        let mut device = Device::new();
        device.timed = false;
        assert!(matches!(
            PreparedRadio::configure(&config, Box::new(device)),
            Err(RadioError::TimedTx)
        ));
        let mut device = Device::new();
        device.tick_rate_hz = 0;
        assert!(matches!(
            PreparedRadio::configure(&config, Box::new(device)),
            Err(RadioError::ClockRate)
        ));
        assert!(matches!(
            PreparedRadio::configure(&config, Box::new(Device::new())),
            Err(RadioError::MissingRx)
        ));
    }
    #[test]
    fn validated_noop_factory_hands_off_both_inactive_owners_with_exact_settings() {
        let prepared = PreparedRadio::open(&config()).unwrap();
        let expected = prepared.settings();
        let streams = prepared
            .into_streams(
                timed_tx::Limits {
                    max_queued_blocks: 4,
                    max_block_samples: 1296,
                },
                receiver::Limits {
                    max_block_samples: 256,
                    timeout_us: 1000,
                },
            )
            .unwrap();
        assert_eq!(streams.settings, expected);
        assert_eq!(streams.tx.state(), timed_tx::State::Prepared);
        assert_eq!(streams.rx.state(), receiver::State::Prepared);
        assert!(streams.tx.stop_acknowledged());
        assert!(streams.rx.stop_acknowledged());
    }
    #[derive(Default)]
    struct Trace {
        tx_enabled: bool,
        rx_active: bool,
        tx_stops: usize,
        backend_tx_stops: usize,
        rx_stops: usize,
        tx_enables: usize,
        rx_activations: usize,
    }
    struct Tx(std::sync::Arc<std::sync::Mutex<Trace>>);
    impl RadioTx for Tx {
        fn tx_sample_rate_hz(&self) -> f64 {
            388_800.0
        }
        fn supports_timed_tx(&self) -> bool {
            true
        }
        fn tick_rate(&self) -> u64 {
            1000000
        }
        fn get_hardware_time(&self) -> Result<u64, HalError> {
            Ok(0)
        }
        fn transmit(&mut self, _samples: &[num_complex::Complex32]) -> Result<(), HalError> {
            Err("no writes in handoff test".into())
        }
        fn enable_transmit(&mut self, enable: bool) -> Result<(), HalError> {
            let mut trace = self.0.lock().unwrap();
            trace.tx_enabled = enable;
            if enable {
                trace.tx_enables += 1;
            } else {
                trace.tx_stops += 1;
            }
            Ok(())
        }
        fn stop(&mut self) -> Result<(), HalError> {
            self.0.lock().unwrap().backend_tx_stops += 1;
            self.enable_transmit(false)
        }
    }
    struct Rx(std::sync::Arc<std::sync::Mutex<Trace>>);
    impl RadioRx for Rx {
        fn rx_sample_rate_hz(&self) -> f64 {
            388_800.0
        }
        fn tick_rate(&self) -> u64 {
            1000000
        }
        fn get_hardware_time(&self) -> Result<u64, HalError> {
            Ok(0)
        }
        fn rx_read(
            &mut self,
            _buffer: &mut [num_complex::Complex32],
            _timeout_us: i64,
        ) -> Result<sdr_hal::RxReadResult, HalError> {
            Err("no reads in handoff test".into())
        }
        fn rx_activate(&mut self, _tick: Option<u64>) -> Result<(), HalError> {
            let mut trace = self.0.lock().unwrap();
            trace.rx_active = true;
            trace.rx_activations += 1;
            Ok(())
        }
        fn stop(&mut self) -> Result<(), HalError> {
            let mut trace = self.0.lock().unwrap();
            trace.rx_active = false;
            trace.rx_stops += 1;
            Ok(())
        }
    }
    fn traced_prepared() -> (PreparedRadio, std::sync::Arc<std::sync::Mutex<Trace>>) {
        let trace = std::sync::Arc::new(std::sync::Mutex::new(Trace::default()));
        let radio = PreparedRadio {
            tx: Some(Box::new(Tx(trace.clone()))),
            rx: Some(Box::new(Rx(trace.clone()))),
            settings: RadioSettings {
                applied_tx_sample_rate_hz: 194400,
                applied_rx_sample_rate_hz: 194400,
                tx_tick_rate_hz: 1000000,
                rx_tick_rate_hz: 1000000,
            },
            stop_acknowledged: true,
        };
        (radio, trace)
    }
    #[test]
    fn prepared_guard_dispatches_backend_stop_for_both_streams() {
        let (mut radio, trace) = traced_prepared();
        radio.stop().unwrap();
        assert!(radio.stop_acknowledged());
        let trace = trace.lock().unwrap();
        assert_eq!(trace.backend_tx_stops, 1);
        assert_eq!(trace.rx_stops, 1);
    }
    #[test]
    fn failing_either_owner_construction_keeps_both_halves_stopped_and_unactivated() {
        for fail_tx in [true, false] {
            let (radio, trace) = traced_prepared();
            let result = radio.into_streams(
                timed_tx::Limits {
                    max_queued_blocks: if fail_tx { 0 } else { 4 },
                    max_block_samples: 1296,
                },
                receiver::Limits {
                    max_block_samples: 256,
                    timeout_us: if fail_tx { 1000 } else { 0 },
                },
            );
            if fail_tx {
                assert!(matches!(result, Err(RadioError::TxStream(_))));
            } else {
                assert!(matches!(result, Err(RadioError::RxStream(_))));
            }
            let trace = trace.lock().unwrap();
            assert!(!trace.tx_enabled);
            assert!(!trace.rx_active);
            assert_eq!(trace.tx_enables, 0);
            assert_eq!(trace.rx_activations, 0);
            assert!(trace.tx_stops > 0);
            assert!(trace.rx_stops > 0);
        }
    }
    #[test]
    fn transferred_owners_survive_the_prepared_guard_and_request_stop_when_active_owners_drop() {
        let (radio, trace) = traced_prepared();
        let mut streams = radio
            .into_streams(
                timed_tx::Limits {
                    max_queued_blocks: 4,
                    max_block_samples: 1296,
                },
                receiver::Limits {
                    max_block_samples: 256,
                    timeout_us: 1000,
                },
            )
            .unwrap();
        streams.tx.start(1000, 100000).unwrap();
        streams.rx.start(None).unwrap();
        {
            let trace = trace.lock().unwrap();
            assert!(trace.tx_enabled);
            assert!(trace.rx_active);
        }
        drop(streams);
        let trace = trace.lock().unwrap();
        assert!(!trace.tx_enabled);
        assert!(!trace.rx_active);
        assert_eq!(trace.tx_enables, 1);
        assert_eq!(trace.rx_activations, 1);
    }
    #[test]
    fn clock_changes_since_preparation_reject_handoff_before_owner_construction() {
        let (mut radio, trace) = traced_prepared();
        radio.settings.rx_tick_rate_hz = 2000000;
        let result = radio.into_streams(
            timed_tx::Limits {
                max_queued_blocks: 4,
                max_block_samples: 1296,
            },
            receiver::Limits {
                max_block_samples: 256,
                timeout_us: 1000,
            },
        );
        assert!(matches!(
            result,
            Err(RadioError::StreamClock {
                expected_rx_hz: 2000000,
                actual_rx_hz: 1000000,
                ..
            })
        ));
        let trace = trace.lock().unwrap();
        assert!(!trace.tx_enabled);
        assert!(!trace.rx_active);
        assert_eq!(trace.tx_enables, 0);
        assert_eq!(trace.rx_activations, 0);
    }
}
