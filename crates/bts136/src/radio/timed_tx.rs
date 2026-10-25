use num_complex::Complex32;
use sdr_hal::{RadioTx, TxRadioHealth, error::Error as HalError};
use std::{
    collections::{TryReserveError, VecDeque},
    error::Error as StdError,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

const NANOS_PER_SECOND: u128 = 1_000_000_000;
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_queued_blocks: usize,
    pub max_block_samples: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Prepared,
    Running,
    Stopped,
    Failed,
}
#[derive(Debug)]
pub enum Failure {
    Limits,
    ClockRate {
        tick_rate_hz: u64,
        sample_rate_hz: u64,
    },
    TimedTx,
    State,
    Window,
    QueueFull,
    SampleCount(usize),
    Sample(usize),
    Range,
    ClockRegression {
        previous_tick: u64,
        current_tick: u64,
    },
    ClockRateChanged {
        previous_hz: u64,
        current_hz: u64,
    },
    Late {
        start_tick: u64,
        current_tick: u64,
    },
    Deadline {
        end_tick: u64,
        stop_tick: u64,
    },
    OutputCapacity,
    Health {
        previous: TxRadioHealth,
        current: TxRadioHealth,
    },
    Hal(Box<HalError>),
    Allocation(TryReserveError),
}
#[derive(Debug)]
pub struct Error {
    pub failure: Failure,
    pub stop_error: Option<Box<HalError>>,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "timed TX failed: {:?}", self.failure)?;
        if let Some(error) = &self.stop_error {
            write!(f, ". Stop request failed: {error}")?;
        }
        Ok(())
    }
}
impl StdError for Error {}
#[derive(Debug, Clone)]
pub struct Receipt {
    owner: Arc<()>,
    sequence: u64,
    start_tick: u64,
    end_tick: u64,
    sample_count: usize,
}
impl Receipt {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn start_tick(&self) -> u64 {
        self.start_tick
    }
    pub fn end_tick(&self) -> u64 {
        self.end_tick
    }
    pub fn sample_count(&self) -> usize {
        self.sample_count
    }
}
impl PartialEq for Receipt {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence && Arc::ptr_eq(&self.owner, &other.owner)
    }
}
impl Eq for Receipt {}
#[derive(Debug, Clone, Copy)]
pub struct Observation {
    pub hardware_tick: u64,
    pub health: TxRadioHealth,
    pub state: State,
}

pub struct TimedTx {
    tx: Box<dyn RadioTx>,
    limits: Limits,
    sample_rate_hz: u64,
    tick_rate_hz: u64,
    state: State,
    stop_acknowledged: bool,
    owner: Arc<()>,
    queue: VecDeque<Receipt>,
    submitted_samples: u64,
    next_sequence: u64,
    epoch_tick: u64,
    stop_tick: u64,
    last_tick: Option<u64>,
    last_device_tick: Option<u64>,
    clock_anchor: Option<(u64, Instant)>,
    hardware_time_refresh: Duration,
    last_health: Option<TxRadioHealth>,
    last_observation_host: Option<Instant>,
    last_write_host: Option<Instant>,
    last_write_us: u64,
    max_write_us: u64,
    last_slow_write_warning: Option<Instant>,
    burst_open: bool,
}
impl TimedTx {
    /// Sample rate must be the applied final IQ rate. Construction requests TX stop and never enables it.
    pub fn try_new(
        tx: Box<dyn RadioTx>,
        sample_rate_hz: u64,
        limits: Limits,
    ) -> Result<Self, Error> {
        let tick_rate_hz = tx.tick_rate();
        let mut stream = Self {
            tx,
            limits,
            sample_rate_hz,
            tick_rate_hz,
            state: State::Prepared,
            stop_acknowledged: false,
            owner: Arc::new(()),
            queue: VecDeque::new(),
            submitted_samples: 0,
            next_sequence: 0,
            epoch_tick: 0,
            stop_tick: 0,
            last_tick: None,
            last_device_tick: None,
            clock_anchor: None,
            hardware_time_refresh: Duration::ZERO,
            last_health: None,
            last_observation_host: None,
            last_write_host: None,
            last_write_us: 0,
            max_write_us: 0,
            last_slow_write_warning: None,
            burst_open: false,
        };
        stream
            .request_stop()
            .map_err(|error| stream.fail(Failure::Hal(Box::new(error))))?;
        if limits.max_queued_blocks == 0 || limits.max_block_samples == 0 {
            return Err(stream.fail(Failure::Limits));
        }
        if sample_rate_hz == 0 || tick_rate_hz < sample_rate_hz {
            return Err(stream.fail(Failure::ClockRate {
                tick_rate_hz,
                sample_rate_hz,
            }));
        }
        if !stream.tx.supports_timed_tx() {
            return Err(stream.fail(Failure::TimedTx));
        }
        stream
            .queue
            .try_reserve_exact(limits.max_queued_blocks)
            .map_err(|error| stream.fail(Failure::Allocation(error)))?;
        stream
            .tx
            .prepare_transmit(limits.max_block_samples)
            .map_err(|error| stream.fail(Failure::Hal(Box::new(error))))?;
        Ok(stream)
    }
    /// Reads the device clock at most once per `refresh` and extrapolates from the host
    /// monotonic clock in between. Zero reads the device on every observation.
    pub fn set_hardware_time_refresh(&mut self, refresh: Duration) {
        self.hardware_time_refresh = refresh;
    }
    pub(crate) fn rates(&self) -> (u64, u64) {
        (self.sample_rate_hz, self.tick_rate_hz)
    }
    pub(crate) fn queue_full(&self) -> bool {
        self.queue.len() == self.limits.max_queued_blocks
    }
    pub(crate) fn max_block_samples(&self) -> usize {
        self.limits.max_block_samples
    }
    pub fn state(&self) -> State {
        self.state
    }
    /// A successful disable request is not an RF-state query.
    pub fn stop_acknowledged(&self) -> bool {
        self.stop_acknowledged
    }
    pub fn pending_blocks(&self) -> usize {
        self.queue.len()
    }
    pub fn prepared_observation(&mut self) -> Result<Observation, Error> {
        if self.state != State::Prepared {
            return Err(self.local(Failure::State));
        }
        self.observe(true).map_err(|failure| self.fail(failure))
    }
    /// Epoch identifies the first IQ sample, including any caller-provided pre-roll and pulse-delay compensation.
    pub fn start(&mut self, epoch_tick: u64, stop_tick: u64) -> Result<(), Error> {
        if self.state != State::Prepared {
            return Err(self.local(Failure::State));
        }
        let observed = self.observe(true).map_err(|failure| self.fail(failure))?;
        if epoch_tick <= observed.hardware_tick || stop_tick <= epoch_tick {
            return Err(self.local(Failure::Window));
        }
        self.tx
            .enable_transmit(true)
            .map_err(|error| self.fail(Failure::Hal(Box::new(error))))?;
        self.stop_acknowledged = false;
        self.epoch_tick = epoch_tick;
        self.stop_tick = stop_tick;
        self.state = State::Running;
        Ok(())
    }
    pub fn submit(&mut self, samples: &[Complex32]) -> Result<Receipt, Error> {
        if self.state != State::Running {
            return Err(self.local(Failure::State));
        }
        let observed = self.observe(false).map_err(|failure| self.fail(failure))?;
        if observed.hardware_tick >= self.stop_tick {
            self.stop()?;
            return Err(self.local(Failure::State));
        }
        if samples.is_empty() || samples.len() > self.limits.max_block_samples {
            return Err(self.local(Failure::SampleCount(samples.len())));
        }
        if self.queue.len() == self.limits.max_queued_blocks {
            return Err(self.local(Failure::QueueFull));
        }
        if let Some(index) = samples
            .iter()
            .position(|sample| !sample.re.is_finite() || !sample.im.is_finite())
        {
            return Err(self.local(Failure::Sample(index)));
        }
        let next_samples = self
            .submitted_samples
            .checked_add(samples.len() as u64)
            .ok_or_else(|| self.local(Failure::Range))?;
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| self.local(Failure::Range))?;
        let start_tick = self
            .tick_at(self.submitted_samples)
            .ok_or_else(|| self.local(Failure::Range))?;
        let end_tick = self
            .tick_at(next_samples)
            .ok_or_else(|| self.local(Failure::Range))?;
        if end_tick > self.stop_tick {
            return Err(self.local(Failure::Deadline {
                end_tick,
                stop_tick: self.stop_tick,
            }));
        }
        if start_tick <= observed.hardware_tick {
            return Err(self.local(Failure::Late {
                start_tick,
                current_tick: observed.hardware_tick,
            }));
        }
        let write_started = Instant::now();
        let write_result = self.tx.write_all(
            samples,
            sdr_hal::TxMetadata {
                schedule: sdr_hal::Schedule::At(sdr_hal::DeviceTime::from_ticks(start_tick)),
                end_of_burst: false,
            },
        );
        self.last_write_us = elapsed_us(write_started);
        self.max_write_us = self.max_write_us.max(self.last_write_us);
        let since_previous_write_us = self.last_write_host.map(elapsed_us);
        if self.last_write_us >= SLOW_WRITE_US
            && self
                .last_slow_write_warning
                .is_none_or(|last| last.elapsed() >= WARNING_INTERVAL)
        {
            tracing::warn!(
                write_us = self.last_write_us,
                ?since_previous_write_us,
                samples = samples.len(),
                start_tick,
                observed_tick = observed.hardware_tick,
                lead_us = relative_us(start_tick, observed.hardware_tick, self.tick_rate_hz),
                pending_blocks_host = self.queue.len(),
                "Radio TX write stalled"
            );
            self.last_slow_write_warning = Some(Instant::now());
        }
        if let Err(error) = write_result {
            if error.kind() == sdr_hal::error::ErrorKind::Late
                && error.accepted_samples() == Some(0)
            {
                let observed = self.observe(true).map_err(|failure| self.fail(failure))?;
                return Err(self.local(Failure::Late {
                    start_tick,
                    current_tick: observed.hardware_tick,
                }));
            }
            return Err(self.fail(Failure::Hal(Box::new(error))));
        }
        self.last_write_host = Some(Instant::now());
        self.burst_open = true;
        let receipt = Receipt {
            owner: self.owner.clone(),
            sequence: self.next_sequence,
            start_tick,
            end_tick,
            sample_count: samples.len(),
        };
        self.queue.push_back(receipt.clone());
        self.submitted_samples = next_samples;
        self.next_sequence = next_sequence;
        Ok(receipt)
    }
    #[cfg(test)]
    pub(crate) fn skip_expired_block(
        &mut self,
        sample_count: usize,
        lead_ticks: u64,
    ) -> Result<bool, Error> {
        self.skip_expired_blocks(sample_count, lead_ticks, 1)
            .map(|count| count != 0)
    }
    pub(crate) fn skip_expired_blocks(
        &mut self,
        sample_count: usize,
        lead_ticks: u64,
        maximum_blocks: u64,
    ) -> Result<u64, Error> {
        if self.state != State::Running
            || sample_count == 0
            || sample_count > self.limits.max_block_samples
            || maximum_blocks == 0
        {
            return Err(self.local(Failure::State));
        }
        let observed = self.observe(false).map_err(|failure| self.fail(failure))?;
        let start_tick = self
            .tick_at(self.submitted_samples)
            .ok_or_else(|| self.local(Failure::Range))?;
        let threshold = observed.hardware_tick.saturating_add(lead_ticks);
        let mut low = 0;
        let mut high = maximum_blocks;
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            let sample = (middle - 1)
                .checked_mul(sample_count as u64)
                .and_then(|offset| self.submitted_samples.checked_add(offset))
                .ok_or_else(|| self.local(Failure::Range))?;
            if self
                .tick_at(sample)
                .ok_or_else(|| self.local(Failure::Range))?
                <= threshold
            {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        if low == 0 {
            return Ok(0);
        }
        let next_samples = low
            .checked_mul(sample_count as u64)
            .and_then(|offset| self.submitted_samples.checked_add(offset))
            .ok_or_else(|| self.local(Failure::Range))?;
        let end_tick = self
            .tick_at(next_samples)
            .ok_or_else(|| self.local(Failure::Range))?;
        if end_tick > self.stop_tick {
            return Err(self.local(Failure::Deadline {
                end_tick,
                stop_tick: self.stop_tick,
            }));
        }
        if self.burst_open
            && self
                .tx
                .capabilities()
                .supports(sdr_hal::TxCapability::BurstEnd)
        {
            // A gap must end the native burst or continuous transmit engines can ignore the next timestamp.
            let began = Instant::now();
            self.tx
                .end_burst()
                .map_err(|error| self.fail(Failure::Hal(Box::new(error))))?;
            self.burst_open = false;
            tracing::warn!(
                start_tick,
                burst_end_us = elapsed_us(began),
                "TX burst boundary queued before skipped slot"
            );
        }
        tracing::warn!(start_tick, skipped_blocks=low, current_tick=observed.hardware_tick, lead_ticks,
            start_lead_us=relative_us(start_tick, observed.hardware_tick, self.tick_rate_hz),
            guard_us=relative_us(lead_ticks, 0, self.tick_rate_hz),
            pending_blocks_host=self.queue.len(), last_write_us=self.last_write_us,
            since_last_write_us=?self.last_write_host.map(elapsed_us),
            "TX slot crossed submission guard");
        self.submitted_samples = next_samples;
        Ok(low)
    }
    /// Poll regularly to enforce the stop deadline. Elapsed receipts describe clock observations, not independently verified RF delivery.
    pub fn poll(&mut self, elapsed: &mut Vec<Receipt>) -> Result<Observation, Error> {
        if self.state != State::Running {
            return Err(self.local(Failure::State));
        }
        let mut observed = self.observe(false).map_err(|failure| self.fail(failure))?;
        if observed.hardware_tick >= self.stop_tick {
            self.request_stop()
                .map_err(|error| self.fail(Failure::Hal(Box::new(error))))?;
            self.state = State::Stopped;
        }
        let count = self
            .queue
            .iter()
            .take_while(|block| block.end_tick <= observed.hardware_tick)
            .count();
        if elapsed.capacity() - elapsed.len() < count {
            if self.state == State::Stopped {
                self.queue.clear();
            }
            return Err(self.local(Failure::OutputCapacity));
        }
        for _ in 0..count {
            elapsed.push(
                self.queue
                    .pop_front()
                    .expect("elapsed blocks counted above"),
            );
        }
        observed.state = self.state;
        Ok(observed)
    }
    pub fn stop(&mut self) -> Result<(), Error> {
        self.queue.clear();
        self.request_stop()
            .map_err(|error| self.fail(Failure::Hal(Box::new(error))))?;
        self.state = State::Stopped;
        Ok(())
    }
    fn tick_at(&self, sample: u64) -> Option<u64> {
        let offset = (u128::from(sample) * u128::from(self.tick_rate_hz))
            .div_ceil(u128::from(self.sample_rate_hz));
        self.epoch_tick.checked_add(u64::try_from(offset).ok()?)
    }
    fn device_tick(&mut self, force: bool) -> Result<(u64, Option<u64>), Failure> {
        let now = Instant::now();
        if !force
            && !self.hardware_time_refresh.is_zero()
            && let Some((tick, at)) = self.clock_anchor
            && now.duration_since(at) < self.hardware_time_refresh
        {
            let elapsed = now.duration_since(at).as_nanos() * u128::from(self.tick_rate_hz)
                / NANOS_PER_SECOND;
            let elapsed = u64::try_from(elapsed).map_err(|_| Failure::Range)?;
            return Ok((tick.saturating_add(elapsed), None));
        }
        let tick = self
            .tx
            .get_hardware_time()
            .map_err(|error| Failure::Hal(Box::new(error)))?;
        if let Some(previous) = self.last_device_tick
            && tick < previous
        {
            return Err(Failure::ClockRegression {
                previous_tick: previous,
                current_tick: tick,
            });
        }
        self.last_device_tick = Some(tick);
        // Anchoring at the start of the read makes extrapolation run ahead of the device, so
        // a cached reading can only overstate how late a slot is.
        self.clock_anchor = Some((tick, now));
        Ok((tick, Some(elapsed_us(now))))
    }
    fn observe(&mut self, force: bool) -> Result<Observation, Failure> {
        let rate = self.tx.tick_rate();
        if rate != self.tick_rate_hz {
            return Err(Failure::ClockRateChanged {
                previous_hz: self.tick_rate_hz,
                current_hz: rate,
            });
        }
        let observation_gap_us = self.last_observation_host.map(elapsed_us);
        let (tick, hardware_time_us) = self.device_tick(force)?;
        let tick = tick.max(self.last_tick.unwrap_or(0));
        let health_started = Instant::now();
        let health = self
            .tx
            .tx_health()
            .map_err(|error| Failure::Hal(Box::new(error)))?;
        let health_poll_us = elapsed_us(health_started);
        self.last_observation_host = Some(Instant::now());
        if let Some(previous) = self.last_health
            && unhealthy(previous, health)
        {
            return Err(Failure::Health {
                previous,
                current: health,
            });
        }
        if let Some(previous) = self.last_health
            && health.underflows > previous.underflows
        {
            tracing::warn!(
                new_underflows = health.underflows - previous.underflows,
                total_underflows = health.underflows,
                hardware_tick = tick,
                ?observation_gap_us, ?hardware_time_us, health_poll_us,
                pending_blocks_host = self.queue.len(),
                queued_end_tick = ?self.queue.back().map(|receipt| receipt.end_tick),
                queued_ahead_us = ?self.queue.back().map(|receipt| relative_us(receipt.end_tick, tick, self.tick_rate_hz)),
                last_write_us = self.last_write_us, max_write_us = self.max_write_us,
                since_last_write_us = ?self.last_write_host.map(elapsed_us),
                "TX underflow. Continuing the scheduled stream"
            );
        }
        if let Some(previous) = self.last_health
            && health.late_packets > previous.late_packets
        {
            tracing::warn!(
                new_late_packets = health.late_packets - previous.late_packets,
                total_late_packets = health.late_packets,
                "Radio dropped late TX packets. Continuing the scheduled stream"
            );
        }
        self.last_tick = Some(tick);
        self.last_health = Some(health);
        Ok(Observation {
            hardware_tick: tick,
            health,
            state: self.state,
        })
    }
    fn request_stop(&mut self) -> Result<(), HalError> {
        self.stop_acknowledged = false;
        self.tx.stop()?;
        self.stop_acknowledged = true;
        Ok(())
    }
    fn fail(&mut self, failure: Failure) -> Error {
        self.queue.clear();
        let stop_error = self.request_stop().err().map(Box::new);
        self.state = State::Failed;
        Error {
            failure,
            stop_error,
        }
    }
    fn local(&self, failure: Failure) -> Error {
        Error {
            failure,
            stop_error: None,
        }
    }
}
fn unhealthy(previous: TxRadioHealth, current: TxRadioHealth) -> bool {
    current.underflows < previous.underflows
        || current.late_packets < previous.late_packets
        || current.sequence_errors != previous.sequence_errors
        || current.dropped_packets != previous.dropped_packets
        || current.unknown_events != previous.unknown_events
        || current.burst_acks < previous.burst_acks
}
impl Drop for TimedTx {
    fn drop(&mut self) {
        if !self.stop_acknowledged
            && let Err(error) = self.request_stop()
        {
            eprintln!("timed TX cleanup failed: {error:?}");
        }
    }
}
#[cfg(test)]
mod tests;

const MICROSECONDS_PER_SECOND: i128 = 1_000_000;
const SLOW_WRITE_US: u64 = 2_000;
const WARNING_INTERVAL: Duration = Duration::from_secs(1);
fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
fn relative_us(tick: u64, reference: u64, rate: u64) -> i128 {
    (i128::from(tick) - i128::from(reference)) * MICROSECONDS_PER_SECOND / i128::from(rate)
}
