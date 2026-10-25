use num_complex::Complex32;
use sdr_hal::{RadioRx, error::Error as HalError};
use std::{error::Error as StdError, fmt};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_block_samples: usize,
    pub timeout_us: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Prepared,
    Active,
    Stopped,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuity {
    First,
    Continuous,
    Gap { missing_samples: u64 },
    Overlap { discarded_samples: u64 },
    UnknownGap,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub samples_read: usize,
    pub time_ticks: u64,
    pub first_sample: u64,
    pub segment: u64,
    pub continuity: Continuity,
    pub hardware_overflow: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    Timeout {
        hardware_overflow: bool,
    },
    Dropped {
        repeated_samples: u64,
        hardware_overflow: bool,
    },
    Block(Block),
}
#[derive(Debug)]
pub enum Failure {
    Worker(std::io::Error),
    WorkerStopped,
    WorkerPanicked,
    Limits,
    ClockRate {
        tick_rate_hz: u64,
        sample_rate_hz: u64,
    },
    ClockRateChanged {
        previous_hz: u64,
        current_hz: u64,
    },
    State,
    Buffer(usize),
    Count {
        reported: usize,
        capacity: usize,
    },
    Sample(usize),
    TimestampRegression {
        previous_tick: u64,
        current_tick: u64,
    },
    Overlap {
        expected_sample: u64,
        received_sample: u64,
    },
    Range,
    Hal(Box<HalError>),
}
#[derive(Debug)]
pub struct Error {
    pub failure: Failure,
    pub stop_error: Option<Box<HalError>>,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RX stream failed: {:?}", self.failure)?;
        if let Some(error) = &self.stop_error {
            write!(f, ". Deactivation failed: {error}")?;
        }
        Ok(())
    }
}
impl StdError for Error {}
pub struct Receiver {
    rx: Box<dyn RadioRx>,
    sample_rate_hz: u64,
    tick_rate_hz: u64,
    limits: Limits,
    state: State,
    stop_acknowledged: bool,
    origin_tick: Option<u64>,
    last_tick: Option<u64>,
    last_sample_count: u64,
    next_sample: u64,
    segment: u64,
    pending_overflow: bool,
    pending_discontinuity: bool,
}
impl Receiver {
    /// Timestamp quantization requires at least two clock ticks per sample. Use the applied final RX sample rate.
    pub fn try_new(
        rx: Box<dyn RadioRx>,
        sample_rate_hz: u64,
        limits: Limits,
    ) -> Result<Self, Error> {
        let tick_rate_hz = rx.tick_rate();
        let mut receiver = Self {
            rx,
            sample_rate_hz,
            tick_rate_hz,
            limits,
            state: State::Prepared,
            stop_acknowledged: false,
            origin_tick: None,
            last_tick: None,
            last_sample_count: 0,
            next_sample: 0,
            segment: 0,
            pending_overflow: false,
            pending_discontinuity: false,
        };
        receiver
            .request_stop()
            .map_err(|error| receiver.fail(Failure::Hal(Box::new(error))))?;
        if limits.max_block_samples == 0 || limits.timeout_us <= 0 {
            return Err(receiver.fail(Failure::Limits));
        }
        if sample_rate_hz == 0 || u128::from(tick_rate_hz) < u128::from(sample_rate_hz) * 2 {
            return Err(receiver.fail(Failure::ClockRate {
                tick_rate_hz,
                sample_rate_hz,
            }));
        }
        Ok(receiver)
    }
    pub(super) fn max_block_samples(&self) -> usize {
        self.limits.max_block_samples
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn origin_tick(&self) -> Option<u64> {
        self.origin_tick
    }
    pub fn stop_acknowledged(&self) -> bool {
        self.stop_acknowledged
    }
    /// Some backends ignore the requested activation time. The first nonempty read establishes the actual sample epoch.
    pub fn start(&mut self, time_ticks: Option<u64>) -> Result<(), Error> {
        if self.state != State::Prepared {
            return Err(self.local(Failure::State));
        }
        self.rx
            .rx_activate(time_ticks)
            .map_err(|error| self.fail(Failure::Hal(Box::new(error))))?;
        self.stop_acknowledged = false;
        self.state = State::Active;
        Ok(())
    }
    /// Only the reported prefix is valid. Gaps and overflow require resetting channel acquisition and partial message state.
    pub fn read(&mut self, buffer: &mut [Complex32]) -> Result<ReadOutcome, Error> {
        if self.state != State::Active {
            return Err(self.local(Failure::State));
        }
        if buffer.is_empty() || buffer.len() > self.limits.max_block_samples {
            return Err(self.local(Failure::Buffer(buffer.len())));
        }
        let current_hz = self.rx.tick_rate();
        if current_hz != self.tick_rate_hz {
            return Err(self.fail(Failure::ClockRateChanged {
                previous_hz: self.tick_rate_hz,
                current_hz,
            }));
        }
        let mut read = self
            .rx
            .rx_read(buffer, self.limits.timeout_us)
            .map_err(|error| self.fail(Failure::Hal(Box::new(error))))?;
        if read.samples_read > buffer.len() {
            return Err(self.fail(Failure::Count {
                reported: read.samples_read,
                capacity: buffer.len(),
            }));
        }
        if read.samples_read == 0 {
            self.pending_overflow |= read.overflow;
            return Ok(ReadOutcome::Timeout {
                hardware_overflow: read.overflow,
            });
        }
        if let Some(index) = buffer[..read.samples_read]
            .iter()
            .position(|sample| !sample.re.is_finite() || !sample.im.is_finite())
        {
            return Err(self.fail(Failure::Sample(index)));
        }
        if let Some(previous) = self.last_tick
            && read.time_ticks < previous
        {
            return Err(self.fail(Failure::TimestampRegression {
                previous_tick: previous,
                current_tick: read.time_ticks,
            }));
        }
        let mut first_sample = if let Some(previous_tick) = self.last_tick {
            // Project adjacent timestamps so fractional rate readback does not accumulate into a false sample overlap.
            let delta =
                u128::from(read.time_ticks - previous_tick) * u128::from(self.sample_rate_hz);
            let elapsed_samples = u64::try_from(
                (delta + u128::from(self.tick_rate_hz) / 2) / u128::from(self.tick_rate_hz),
            )
            .map_err(|_| self.fail(Failure::Range))?;
            self.next_sample
                .checked_sub(self.last_sample_count)
                .and_then(|start| start.checked_add(elapsed_samples))
                .ok_or_else(|| self.fail(Failure::Range))?
        } else {
            0
        };
        let discarded_samples = self.next_sample.saturating_sub(first_sample);
        if discarded_samples != 0 {
            self.pending_discontinuity = true;
            tracing::warn!(
                expected_sample = self.next_sample,
                received_sample = first_sample,
                discarded_samples,
                "RX overlap. Discarding repeated samples and continuing"
            );
            if discarded_samples >= read.samples_read as u64 {
                self.pending_overflow |= read.overflow;
                return Ok(ReadOutcome::Dropped {
                    repeated_samples: read.samples_read as u64,
                    hardware_overflow: read.overflow,
                });
            }
            let discard = discarded_samples as usize;
            buffer.copy_within(discard..read.samples_read, 0);
            read.samples_read -= discard;
            let ticks = u64::try_from(
                (u128::from(discarded_samples) * u128::from(self.tick_rate_hz)
                    + u128::from(self.sample_rate_hz) / 2)
                    / u128::from(self.sample_rate_hz),
            )
            .map_err(|_| self.fail(Failure::Range))?;
            read.time_ticks = read
                .time_ticks
                .checked_add(ticks)
                .ok_or_else(|| self.fail(Failure::Range))?;
            first_sample = self.next_sample;
        }
        let hardware_overflow = read.overflow || self.pending_overflow;
        let continuity = if self.origin_tick.is_none() {
            if hardware_overflow {
                Continuity::UnknownGap
            } else {
                Continuity::First
            }
        } else if discarded_samples != 0 {
            Continuity::Overlap { discarded_samples }
        } else if first_sample > self.next_sample {
            Continuity::Gap {
                missing_samples: first_sample - self.next_sample,
            }
        } else if hardware_overflow || self.pending_discontinuity {
            Continuity::UnknownGap
        } else {
            Continuity::Continuous
        };
        let segment = if self.origin_tick.is_some()
            && matches!(
                continuity,
                Continuity::Gap { .. } | Continuity::Overlap { .. } | Continuity::UnknownGap
            ) {
            self.segment
                .checked_add(1)
                .ok_or_else(|| self.fail(Failure::Range))?
        } else {
            self.segment
        };
        let next_sample = first_sample
            .checked_add(read.samples_read as u64)
            .ok_or_else(|| self.fail(Failure::Range))?;
        self.origin_tick.get_or_insert(read.time_ticks);
        self.last_tick = Some(read.time_ticks);
        self.last_sample_count = read.samples_read as u64;
        self.next_sample = next_sample;
        self.segment = segment;
        self.pending_overflow = false;
        self.pending_discontinuity = false;
        Ok(ReadOutcome::Block(Block {
            samples_read: read.samples_read,
            time_ticks: read.time_ticks,
            first_sample,
            segment,
            continuity,
            hardware_overflow,
        }))
    }
    pub fn stop(&mut self) -> Result<(), Error> {
        self.request_stop()
            .map_err(|error| self.fail(Failure::Hal(Box::new(error))))?;
        self.state = State::Stopped;
        Ok(())
    }
    fn request_stop(&mut self) -> Result<(), HalError> {
        self.stop_acknowledged = false;
        self.rx.stop()?;
        self.stop_acknowledged = true;
        Ok(())
    }
    fn fail(&mut self, failure: Failure) -> Error {
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
impl Drop for Receiver {
    fn drop(&mut self) {
        if !self.stop_acknowledged
            && let Err(error) = self.request_stop()
        {
            eprintln!("RX cleanup failed: {error}");
        }
    }
}
#[cfg(test)]
mod tests;
