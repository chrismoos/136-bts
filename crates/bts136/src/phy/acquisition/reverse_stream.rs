use super::{
    AcquiredSlot, AcquisitionError, AcquisitionWorkspace, FILTER_HALF_SPAN_SYMBOLS, MatchedFilter,
    SLOT_SYMBOLS, StartBounds,
};
use crate::phy::{
    modulation::Symbol,
    pulse::{RootRaisedCosine, SYMBOL_RATE_HZ},
    rach::{BurstFormat, ReverseControlFields},
    shortened::ShortenedBurst,
    traffic::TrafficFields,
};
use std::{collections::TryReserveError, error::Error as StdError, fmt};
const MAX_EXACT_SAMPLE: u64 = 1 << 53;
#[derive(Debug)]
pub enum Error {
    Acquisition(AcquisitionError),
    Allocation(TryReserveError),
    Limits,
    State,
    Input,
    Position,
    OutputCapacity,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "reverse stream failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    Finished,
    Failed,
}
#[derive(Debug)]
pub struct Burst<T = TrafficFields> {
    pub segment: u64,
    pub first_sample: f64,
    pub acquired: AcquiredSlot<T>,
    pub channel_profile: Option<super::channel_profile::ChannelProfile>,
}
pub struct ReverseStream<T = TrafficFields> {
    workspace: AcquisitionWorkspace,
    matched: MatchedFilter,
    incoming: Vec<Symbol>,
    samples: Vec<Symbol>,
    candidates: Vec<AcquiredSlot<T>>,
    decode_window: WindowDecoder<T>,
    channel_profiler: Option<ChannelProfiler<T>>,
    hop_samples: usize,
    window_samples: usize,
    prefix_samples: usize,
    samples_per_symbol: f64,
    max_bursts: usize,
    segment: Option<u64>,
    next_sample: u64,
    window_first_sample: u64,
    initial_window: bool,
    last_burst_sample: Option<f64>,
    state: State,
}
type WindowDecoder<T> = Box<
    dyn FnMut(
            &mut AcquisitionWorkspace,
            &[Symbol],
            StartBounds,
            &mut Vec<AcquiredSlot<T>>,
        ) -> Result<(), AcquisitionError>
        + Send,
>;
type ChannelProfiler<T> =
    fn(&AcquisitionWorkspace, &AcquiredSlot<T>) -> Option<super::channel_profile::ChannelProfile>;
pub type ReverseControlStream = ReverseStream<ReverseControlFields>;
impl ReverseStream<TrafficFields> {
    pub fn try_new(sample_rate_hz: f64, max_bursts: usize) -> Result<Self, Error> {
        Self::with_decoder(
            sample_rate_hz,
            max_bursts,
            Box::new(|workspace, samples, bounds, output| {
                workspace.reverse_traffic_window(samples, Some(bounds), output)
            }),
            Some(super::channel_profile::estimate),
        )
    }
}
impl ReverseStream<ShortenedBurst> {
    pub fn try_new_shortened(sample_rate_hz: f64, max_bursts: usize) -> Result<Self, Error> {
        Self::with_decoder(
            sample_rate_hz,
            max_bursts,
            Box::new(|workspace, samples, bounds, output| {
                workspace.shortened_window(samples, Some(bounds), output)
            }),
            None,
        )
    }
}
impl ReverseStream<ReverseControlFields> {
    pub fn try_new_control(
        sample_rate_hz: f64,
        max_bursts: usize,
        format: BurstFormat,
        dvcc: u8,
    ) -> Result<Self, Error> {
        Self::with_decoder(
            sample_rate_hz,
            max_bursts,
            Box::new(move |workspace, samples, bounds, output| {
                workspace.reverse_control_window(samples, format, dvcc, Some(bounds), output)
            }),
            None,
        )
    }
}
impl<T> ReverseStream<T> {
    fn with_decoder(
        sample_rate_hz: f64,
        max_bursts: usize,
        decode_window: WindowDecoder<T>,
        channel_profiler: Option<ChannelProfiler<T>>,
    ) -> Result<Self, Error> {
        let filter = RootRaisedCosine::try_new(sample_rate_hz, FILTER_HALF_SPAN_SYMBOLS)
            .map_err(|error| Error::Acquisition(AcquisitionError::Pulse(error)))?;
        if max_bursts == 0 {
            return Err(Error::Limits);
        }
        let samples_per_symbol = sample_rate_hz / SYMBOL_RATE_HZ;
        let hop_samples = (SLOT_SYMBOLS as f64 * samples_per_symbol).ceil() as usize;
        let prefix_samples =
            filter.delay_samples() * 2 + (samples_per_symbol * 2.0).ceil() as usize;
        let window_samples = hop_samples * 2
            + prefix_samples
            + filter.delay_samples()
            + (samples_per_symbol * 2.0).ceil() as usize;
        let workspace = AcquisitionWorkspace::try_new(sample_rate_hz, window_samples)
            .map_err(Error::Acquisition)?;
        let matched = MatchedFilter::try_new(sample_rate_hz)
            .map_err(|error| Error::Acquisition(AcquisitionError::Pulse(error)))?;
        let mut incoming = Vec::new();
        incoming
            .try_reserve_exact(hop_samples)
            .map_err(Error::Allocation)?;
        incoming.resize(hop_samples, Symbol { i: 0.0, q: 0.0 });
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(window_samples)
            .map_err(Error::Allocation)?;
        let mut candidates = Vec::new();
        candidates
            .try_reserve_exact(max_bursts)
            .map_err(Error::Allocation)?;
        Ok(Self {
            workspace,
            matched,
            incoming,
            decode_window,
            channel_profiler,
            samples,
            candidates,
            hop_samples,
            window_samples,
            prefix_samples,
            samples_per_symbol,
            max_bursts,
            segment: None,
            next_sample: 0,
            window_first_sample: 0,
            initial_window: true,
            last_burst_sample: None,
            state: State::Ready,
        })
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn max_read_samples(&self) -> usize {
        self.hop_samples
    }
    pub fn discontinuity(&mut self) {
        self.matched.reset();
        self.samples.clear();
        self.candidates.clear();
        self.segment = None;
        self.last_burst_sample = None;
        self.initial_window = true;
        self.state = State::Ready;
    }
    /// Supply contiguous sample indices within each segment. Changed segments discard retained IQ. Reserve max_bursts output slots.
    pub fn push(
        &mut self,
        segment: u64,
        first_sample: u64,
        samples: &[Symbol],
        output: &mut Vec<Burst<T>>,
    ) -> Result<(), Error> {
        self.admit(segment, first_sample, samples, output)?;
        let mut incoming = std::mem::take(&mut self.incoming);
        self.matched.process(samples, &mut incoming);
        let result = self.ingest(samples, &incoming[..samples.len()], output);
        self.incoming = incoming;
        result
    }
    /// Like `push`, with `filtered` taken from one `MatchedFilter` that has seen every sample of
    /// the segment in order. The caller resets that filter whenever the segment changes.
    pub fn push_filtered(
        &mut self,
        segment: u64,
        first_sample: u64,
        samples: &[Symbol],
        filtered: &[Symbol],
        output: &mut Vec<Burst<T>>,
    ) -> Result<(), Error> {
        if filtered.len() != samples.len() {
            return Err(Error::Input);
        }
        self.admit(segment, first_sample, samples, output)?;
        self.ingest(samples, filtered, output)
    }
    fn admit(
        &mut self,
        segment: u64,
        first_sample: u64,
        samples: &[Symbol],
        output: &mut Vec<Burst<T>>,
    ) -> Result<(), Error> {
        if self.state != State::Ready {
            return Err(Error::State);
        }
        if samples.is_empty()
            || samples.len() > self.hop_samples
            || samples
                .iter()
                .any(|sample| !sample.i.is_finite() || !sample.q.is_finite())
        {
            return Err(Error::Input);
        }
        let next = first_sample
            .checked_add(samples.len() as u64)
            .filter(|next| *next <= MAX_EXACT_SAMPLE)
            .ok_or(Error::Position)?;
        if self.segment.is_some_and(|previous| segment < previous)
            || (self.segment == Some(segment) && first_sample != self.next_sample)
        {
            return Err(Error::Position);
        }
        if output.capacity() < self.max_bursts {
            return Err(Error::OutputCapacity);
        }
        if self.segment != Some(segment) {
            self.discontinuity();
            self.segment = Some(segment);
            self.window_first_sample = first_sample;
        }
        output.clear();
        self.next_sample = next;
        Ok(())
    }
    fn ingest(
        &mut self,
        samples: &[Symbol],
        filtered: &[Symbol],
        output: &mut Vec<Burst<T>>,
    ) -> Result<(), Error> {
        let available = self.window_samples - self.samples.len();
        let first = samples.len().min(available);
        self.append(&samples[..first], &filtered[..first]);
        if self.samples.len() == self.window_samples {
            if let Err(error) = self.decode(false, output) {
                self.state = State::Failed;
                output.clear();
                return Err(error);
            }
            self.samples.copy_within(self.hop_samples.., 0);
            self.samples
                .truncate(self.window_samples - self.hop_samples);
            self.workspace
                .filtered_mut()
                .copy_within(self.hop_samples..self.window_samples, 0);
            self.window_first_sample += self.hop_samples as u64;
            self.initial_window = false;
        }
        self.append(&samples[first..], &filtered[first..]);
        Ok(())
    }
    fn append(&mut self, samples: &[Symbol], filtered: &[Symbol]) {
        let start = self.samples.len();
        self.workspace.filtered_mut()[start..start + filtered.len()].copy_from_slice(filtered);
        self.samples.extend_from_slice(samples);
    }
    pub fn finish(&mut self, output: &mut Vec<Burst<T>>) -> Result<(), Error> {
        if self.state != State::Ready {
            return Err(Error::State);
        }
        if output.capacity() < self.max_bursts {
            return Err(Error::OutputCapacity);
        }
        output.clear();
        if let Err(error) = self.decode(true, output) {
            self.state = State::Failed;
            output.clear();
            return Err(error);
        }
        self.state = State::Finished;
        Ok(())
    }
    fn decode(&mut self, final_window: bool, output: &mut Vec<Burst<T>>) -> Result<(), Error> {
        let lower = if self.initial_window {
            0.0
        } else {
            self.prefix_samples as f64 - self.samples_per_symbol
        };
        let upper = (self.hop_samples + self.prefix_samples) as f64 + self.samples_per_symbol;
        let bounds = StartBounds {
            lower,
            upper: if final_window { f64::INFINITY } else { upper },
        };
        (self.decode_window)(
            &mut self.workspace,
            &self.samples,
            bounds,
            &mut self.candidates,
        )
        .map_err(Error::Acquisition)?;
        if self.candidates.len() > self.max_bursts {
            return Err(Error::OutputCapacity);
        }
        for acquired in self.candidates.drain(..) {
            let local = acquired.slot_start_sample;
            let first_sample = self.window_first_sample as f64 + local;
            if local < lower
                || (!final_window && local >= upper)
                || self
                    .last_burst_sample
                    .is_some_and(|previous| first_sample - previous < self.samples_per_symbol)
            {
                continue;
            }
            self.last_burst_sample = Some(first_sample);
            output.push(Burst {
                segment: self
                    .segment
                    .expect("nonempty windows originate from a segment"),
                first_sample,
                channel_profile: self
                    .channel_profiler
                    .and_then(|profile| profile(&self.workspace, &acquired)),
                acquired,
            });
        }
        Ok(())
    }
}
