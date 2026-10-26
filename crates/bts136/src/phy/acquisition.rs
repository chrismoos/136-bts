pub mod channel_profile;
pub mod quality;
pub mod reverse_stream;

use super::{
    control::{self, ForwardControlFields, LogicalChannel},
    modulation::{self, Dibit, Symbol},
    pulse::{InvalidPulseRate, RootRaisedCosine, SYMBOL_RATE_HZ},
    rach::{self, BurstFormat, ReverseControlFields},
    shortened::{self, ShortenedBurst},
    sync::{SYNC_SYMBOLS, SyncWord},
    traffic::{self, ForwardTraffic, SLOT_BITS, TrafficFields},
};

use num_complex::Complex32;
use sdr_hal::dsp::fir::SymmetricComplexFir32;
use std::{collections::TryReserveError, error::Error, fmt};

const FILTER_HALF_SPAN_SYMBOLS: usize = 8;
const TIMING_PHASES: usize = 16;
const SLOT_SYMBOLS: usize = SLOT_BITS / 2;
const REVERSE_SYNC_SYMBOL: usize = 14;
const MIN_SYNC_CORRELATION: f64 = 0.90;
const MIN_SYMBOL_POWER: f64 = 1e-20;
const BOUNDS_MARGIN_SYMBOLS: f64 = 2.0;
// Single-precision screening error is far below this gap, so no window that passes in double
// precision is screened out.
const SCREEN_CORRELATION: f64 = MIN_SYNC_CORRELATION - 1e-3;
const REVERSE_DATA_SYMBOL: usize = 6;
const TAU: f64 = std::f64::consts::TAU;

#[derive(Debug, Clone, PartialEq)]
pub struct AcquiredSlot<T> {
    pub slot_start_sample: f64,
    pub sync_correlation: f64,
    pub frequency_offset_hz: f64,
    pub fields: T,
    pub quality: Option<quality::BurstQuality>,
    /// Inactive guard/ramp positions and erased shortened-burst symbols are zero placeholders.
    pub hard_bits: [bool; SLOT_BITS],
}

pub type ReverseTrafficBurst = AcquiredSlot<TrafficFields>;
pub type ForwardTrafficBurst = AcquiredSlot<ForwardTraffic>;
pub type ReverseControlBurst = AcquiredSlot<ReverseControlFields>;
pub type ForwardControlBurst = AcquiredSlot<ForwardControlFields>;

pub fn decode_reverse_traffic(
    samples: &[Symbol],
    sample_rate_hz: f64,
) -> Result<Vec<ReverseTrafficBurst>, InvalidPulseRate> {
    acquire(
        samples,
        sample_rate_hz,
        &[REVERSE_SYNC_SYMBOL],
        REVERSE_DATA_SYMBOL..SLOT_SYMBOLS,
        |bits, _| traffic::decode_reverse(bits).ok(),
    )
}

pub fn decode_forward_traffic(
    samples: &[Symbol],
    sample_rate_hz: f64,
) -> Result<Vec<ForwardTrafficBurst>, InvalidPulseRate> {
    acquire(samples, sample_rate_hz, &[0], 0..SLOT_SYMBOLS, |bits, _| {
        traffic::decode_forward(bits).ok()
    })
}

pub fn decode_forward_control(
    samples: &[Symbol],
    sample_rate_hz: f64,
    channel: LogicalChannel,
    dvcc: u8,
) -> Result<Vec<ForwardControlBurst>, InvalidPulseRate> {
    acquire(samples, sample_rate_hz, &[0], 0..SLOT_SYMBOLS, |bits, _| {
        control::decode_forward(bits, channel, dvcc).ok()
    })
}

pub fn decode_reverse_control(
    samples: &[Symbol],
    sample_rate_hz: f64,
    format: BurstFormat,
    dvcc: u8,
) -> Result<Vec<ReverseControlBurst>, InvalidPulseRate> {
    acquire(
        samples,
        sample_rate_hz,
        &[REVERSE_SYNC_SYMBOL],
        format.decision_symbols(),
        |bits, soft| {
            rach::decode(bits, format, dvcc)
                .or_else(|_| rach::decode_soft(bits, soft, format, dvcc))
                .ok()
        },
    )
}

fn acquire<T>(
    samples: &[Symbol],
    sample_rate_hz: f64,
    sync_offsets: &[usize],
    decision_symbols: std::ops::Range<usize>,
    decode: impl Fn(&[bool; SLOT_BITS], &[f64; SLOT_BITS]) -> Option<T>,
) -> Result<Vec<AcquiredSlot<T>>, InvalidPulseRate> {
    let mut workspace = AcquisitionWorkspace::try_new(sample_rate_hz, samples.len().max(1))
        .map_err(|_| InvalidPulseRate)?;
    let mut found = Vec::new();
    workspace
        .filter_raw(samples)
        .and_then(|()| {
            workspace.search(
                samples,
                sync_offsets,
                decision_symbols,
                decode,
                &mut found,
                OutputPolicy::Grow,
                None,
            )
        })
        .map_err(|_| InvalidPulseRate)?;
    Ok(found)
}

#[derive(Debug)]
pub enum AcquisitionError {
    Pulse(InvalidPulseRate),
    SampleCount { maximum: usize, actual: usize },
    NonFiniteSample(usize),
    OutputCapacity,
    Allocation(TryReserveError),
}
impl fmt::Display for AcquisitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "acquisition failed: {self:?}")
    }
}
impl Error for AcquisitionError {}
enum OutputPolicy {
    Grow,
    Bounded,
    Shortened,
}

/// Half-open range of window-local slot start samples whose bursts a caller keeps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StartBounds {
    pub lower: f64,
    pub upper: f64,
}

pub struct MatchedFilter {
    taps: Vec<f64>,
    fir: SymmetricComplexFir32,
    input: Vec<Complex32>,
    output: Vec<Complex32>,
}
impl MatchedFilter {
    pub fn try_new(sample_rate_hz: f64) -> Result<Self, InvalidPulseRate> {
        let filter = RootRaisedCosine::try_new(sample_rate_hz, FILTER_HALF_SPAN_SYMBOLS)?;
        Ok(Self::from_taps(filter.taps()))
    }
    fn from_taps(taps: &[f64]) -> Self {
        Self {
            taps: taps.to_vec(),
            fir: SymmetricComplexFir32::new(taps),
            input: Vec::new(),
            output: Vec::new(),
        }
    }
    pub fn reset(&mut self) {
        self.fir = SymmetricComplexFir32::new(&self.taps);
    }
    /// # Panics
    /// Panics if `output` is shorter than `samples`.
    pub fn process(&mut self, samples: &[Symbol], output: &mut [Symbol]) {
        self.input.clear();
        self.input.extend(
            samples
                .iter()
                .map(|sample| Complex32::new(sample.i as f32, sample.q as f32)),
        );
        self.fir.process_block_into(&self.input, &mut self.output);
        for (filtered, value) in output[..samples.len()].iter_mut().zip(&self.output) {
            *filtered = Symbol {
                i: f64::from(value.re),
                q: f64::from(value.im),
            };
        }
    }
}

pub struct AcquisitionWorkspace {
    filter: RootRaisedCosine,
    matched: MatchedFilter,
    filtered: Vec<Symbol>,
    symbols: Vec<Symbol>,
    differences: Vec<Symbol>,
    weighted: Vec<Symbol>,
    norms: Vec<f64>,
    window_norms: Vec<f64>,
    screen: Screen,
    references: [[Symbol; SYNC_SYMBOLS]; SyncWord::ALL.len()],
    samples_per_symbol: f64,
}
struct Screen {
    weighted_i: Vec<f32>,
    weighted_q: Vec<f32>,
    sum_i: Vec<f32>,
    sum_q: Vec<f32>,
}
impl AcquisitionWorkspace {
    pub fn try_new(sample_rate_hz: f64, max_samples: usize) -> Result<Self, AcquisitionError> {
        let filter = RootRaisedCosine::try_new(sample_rate_hz, FILTER_HALF_SPAN_SYMBOLS)
            .map_err(AcquisitionError::Pulse)?;
        if max_samples == 0 {
            return Err(AcquisitionError::SampleCount {
                maximum: 0,
                actual: 0,
            });
        }
        let samples_per_symbol = sample_rate_hz / SYMBOL_RATE_HZ;
        let max_symbols = (max_samples as f64 / samples_per_symbol).ceil() as usize;
        Ok(Self {
            matched: MatchedFilter::from_taps(filter.taps()),
            filter,
            filtered: buffer(max_samples)?,
            symbols: buffer(max_symbols)?,
            differences: buffer(max_symbols)?,
            weighted: buffer(max_symbols)?,
            norms: real_buffer(max_symbols)?,
            window_norms: real_buffer(max_symbols)?,
            references: SyncWord::ALL.map(|word| {
                let bits = word.bits();
                std::array::from_fn(|index| {
                    let bits = bits.as_chunks::<2>().0[index];
                    let phase = f64::from(Dibit::from_bits(bits).phase_quarters())
                        * std::f64::consts::FRAC_PI_4;
                    let (q, i) = phase.sin_cos();
                    Symbol { i, q }
                })
            }),
            screen: Screen {
                weighted_i: single_buffer(max_symbols)?,
                weighted_q: single_buffer(max_symbols)?,
                sum_i: single_buffer(max_symbols)?,
                sum_q: single_buffer(max_symbols)?,
            },
            samples_per_symbol,
        })
    }
    /// Replaces output only after input validation. Reserve output capacity beforehand. Capacity failure clears partial results.
    pub fn reverse_traffic(
        &mut self,
        samples: &[Symbol],
        output: &mut Vec<ReverseTrafficBurst>,
    ) -> Result<(), AcquisitionError> {
        self.filter_raw(samples)?;
        self.reverse_traffic_window(samples, None, output)
    }
    pub fn shortened(
        &mut self,
        samples: &[Symbol],
        output: &mut Vec<AcquiredSlot<ShortenedBurst>>,
    ) -> Result<(), AcquisitionError> {
        self.filter_raw(samples)?;
        self.shortened_window(samples, None, output)
    }
    pub fn reverse_control(
        &mut self,
        samples: &[Symbol],
        format: BurstFormat,
        dvcc: u8,
        output: &mut Vec<ReverseControlBurst>,
    ) -> Result<(), AcquisitionError> {
        self.filter_raw(samples)?;
        self.reverse_control_window(samples, format, dvcc, None, output)
    }
    /// The window variants read matched-filter output already placed by `filtered_mut`.
    pub(crate) fn reverse_traffic_window(
        &mut self,
        samples: &[Symbol],
        bounds: Option<StartBounds>,
        output: &mut Vec<ReverseTrafficBurst>,
    ) -> Result<(), AcquisitionError> {
        self.search(
            samples,
            &[REVERSE_SYNC_SYMBOL],
            REVERSE_DATA_SYMBOL..SLOT_SYMBOLS,
            |bits, _| traffic::decode_reverse(bits).ok(),
            output,
            OutputPolicy::Bounded,
            bounds,
        )
    }
    pub(crate) fn shortened_window(
        &mut self,
        samples: &[Symbol],
        bounds: Option<StartBounds>,
        output: &mut Vec<AcquiredSlot<ShortenedBurst>>,
    ) -> Result<(), AcquisitionError> {
        self.search(
            samples,
            &shortened::SYNC_OFFSETS,
            shortened::SYNC_OFFSETS[0]..shortened::ACTIVE_END_SYMBOL,
            |bits, _| shortened::decode(bits),
            output,
            OutputPolicy::Shortened,
            bounds,
        )
    }
    pub(crate) fn reverse_control_window(
        &mut self,
        samples: &[Symbol],
        format: BurstFormat,
        dvcc: u8,
        bounds: Option<StartBounds>,
        output: &mut Vec<ReverseControlBurst>,
    ) -> Result<(), AcquisitionError> {
        self.search(
            samples,
            &[REVERSE_SYNC_SYMBOL],
            format.decision_symbols(),
            |bits, soft| {
                rach::decode(bits, format, dvcc)
                    .or_else(|_| rach::decode_soft(bits, soft, format, dvcc))
                    .ok()
            },
            output,
            OutputPolicy::Bounded,
            bounds,
        )
    }
    pub(crate) fn filtered_mut(&mut self) -> &mut [Symbol] {
        &mut self.filtered
    }
    fn filter_raw(&mut self, samples: &[Symbol]) -> Result<(), AcquisitionError> {
        self.validate(samples)?;
        self.matched.reset();
        self.matched.process(samples, &mut self.filtered);
        Ok(())
    }
    fn validate(&self, samples: &[Symbol]) -> Result<(), AcquisitionError> {
        if samples.len() > self.filtered.len() {
            return Err(AcquisitionError::SampleCount {
                maximum: self.filtered.len(),
                actual: samples.len(),
            });
        }
        if let Some(index) = samples
            .iter()
            .position(|sample| !sample.i.is_finite() || !sample.q.is_finite())
        {
            return Err(AcquisitionError::NonFiniteSample(index));
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn search<T>(
        &mut self,
        samples: &[Symbol],
        sync_offsets: &[usize],
        decision_symbols: std::ops::Range<usize>,
        decode: impl Fn(&[bool; SLOT_BITS], &[f64; SLOT_BITS]) -> Option<T>,
        found: &mut Vec<AcquiredSlot<T>>,
        policy: OutputPolicy,
        bounds: Option<StartBounds>,
    ) -> Result<(), AcquisitionError> {
        self.validate(samples)?;
        let Self {
            filter,
            filtered,
            symbols: symbol_buffer,
            differences: difference_buffer,
            weighted: weighted_buffer,
            norms: norm_buffer,
            window_norms: window_norm_buffer,
            screen,
            references,
            samples_per_symbol,
            ..
        } = self;
        let samples_per_symbol = *samples_per_symbol;
        let filtered = &filtered[..samples.len()];
        let delay = filter.delay_samples() as f64;
        // Candidates near the kept range still take part in duplicate suppression.
        let margin = BOUNDS_MARGIN_SYMBOLS * samples_per_symbol;
        let bounds = bounds.map(|bounds| StartBounds {
            lower: bounds.lower - margin,
            upper: bounds.upper + margin,
        });
        let first_sync = sync_offsets.iter().copied().min().unwrap_or(0);
        let last_sync = sync_offsets.iter().copied().max().unwrap_or(0);
        found.clear();
        for phase in 0..TIMING_PHASES {
            let offset = phase as f64 * samples_per_symbol / TIMING_PHASES as f64;
            let symbol_count =
                ((filtered.len() as f64 - 1.0 - offset) / samples_per_symbol).max(0.0) as usize;
            let symbols = &mut symbol_buffer[..symbol_count];
            for (index, symbol) in symbols.iter_mut().enumerate() {
                *symbol = interpolate(filtered, offset + index as f64 * samples_per_symbol);
            }
            let difference_count = symbol_count.saturating_sub(1);
            let differences = &mut difference_buffer[..difference_count];
            for (output, pair) in differences.iter_mut().zip(symbols.windows(2)) {
                *output = difference(pair[0], pair[1]);
            }
            let weighted = &mut weighted_buffer[..difference_count];
            let norms = &mut norm_buffer[..difference_count];
            for (index, symbol) in differences.iter().enumerate() {
                let magnitude = (symbol.i * symbol.i + symbol.q * symbol.q).sqrt();
                (weighted[index], norms[index]) = if magnitude * magnitude < MIN_SYMBOL_POWER {
                    (Symbol { i: 0.0, q: 0.0 }, 0.0)
                } else {
                    (*symbol, magnitude)
                };
                screen.weighted_i[index] = weighted[index].i as f32;
                screen.weighted_q[index] = weighted[index].q as f32;
            }
            let window_count = (difference_count + 1).saturating_sub(SYNC_SYMBOLS);
            let mut first_index = sync_offsets[0];
            let mut end_index = window_count;
            if let Some(bounds) = bounds {
                let index_at = |start: f64| (start - offset + delay) / samples_per_symbol - 1.0;
                first_index = first_index
                    .max((index_at(bounds.lower).floor().max(0.0) as usize) + first_sync);
                end_index = end_index.min(
                    (index_at(bounds.upper).ceil().max(0.0) as usize)
                        .saturating_add(last_sync)
                        .saturating_add(1),
                );
            }
            if first_index >= end_index {
                continue;
            }
            let window_norms = &mut window_norm_buffer[..window_count];
            for (index, total) in window_norms
                .iter_mut()
                .enumerate()
                .take(end_index)
                .skip(first_index)
            {
                *total = norms[index..index + SYNC_SYMBOLS].iter().sum();
            }
            for reference in &*references {
                screen.correlate(reference, first_index..end_index);
                for index in first_index..end_index {
                    let total_norm = window_norms[index];
                    if !total_norm.is_finite() || total_norm == 0.0 {
                        continue;
                    }
                    let screened = f64::from(screen.sum_i[index]).powi(2)
                        + f64::from(screen.sum_q[index]).powi(2);
                    if screened < (SCREEN_CORRELATION * total_norm).powi(2) {
                        continue;
                    }
                    let sum = weighted[index..index + SYNC_SYMBOLS]
                        .iter()
                        .zip(reference)
                        .fold(Symbol { i: 0.0, q: 0.0 }, |sum, (symbol, reference)| {
                            let product = difference(*reference, *symbol);
                            Symbol {
                                i: sum.i + product.i,
                                q: sum.q + product.q,
                            }
                        });
                    let correlation = (sum.i * sum.i + sum.q * sum.q).sqrt() / total_norm;
                    if correlation < MIN_SYNC_CORRELATION {
                        continue;
                    }
                    for sync_offset in sync_offsets
                        .iter()
                        .copied()
                        .filter(|offset| *offset <= index)
                    {
                        let start = index - sync_offset;
                        let slot_start_sample =
                            offset + (start + 1) as f64 * samples_per_symbol - delay;
                        if bounds.is_some_and(|bounds| {
                            slot_start_sample < bounds.lower || slot_start_sample >= bounds.upper
                        }) {
                            continue;
                        }
                        let Some(slot) = differences.get(start..start + decision_symbols.end)
                        else {
                            continue;
                        };
                        let rotation = sum.q.atan2(sum.i);
                        let (sin, cos) = rotation.sin_cos();
                        let mut hard_bits = [false; SLOT_BITS];
                        let mut soft_bits = [0.0; SLOT_BITS];
                        let mut valid = true;
                        for (index, difference) in
                            slot.iter().enumerate().skip(decision_symbols.start)
                        {
                            let derotated = Symbol {
                                i: difference.i * cos + difference.q * sin,
                                q: difference.q * cos - difference.i * sin,
                            };
                            let Some(dibit) = modulation::decode(Symbol::REFERENCE, derotated)
                            else {
                                if matches!(policy, OutputPolicy::Shortened) {
                                    continue;
                                }
                                valid = false;
                                break;
                            };
                            hard_bits[index * 2..index * 2 + 2].copy_from_slice(&dibit.bits());
                            // IS-136.2-A §2.1.3.3.1 maps Xk to the differential Q sign and Yk to I.
                            soft_bits[index * 2] = derotated.q;
                            soft_bits[index * 2 + 1] = derotated.i;
                        }
                        if !valid {
                            continue;
                        }
                        let Some(fields) = decode(&hard_bits, &soft_bits) else {
                            continue;
                        };
                        if slot_start_sample < 0.0 {
                            continue;
                        }
                        let burst = AcquiredSlot {
                            slot_start_sample,
                            sync_correlation: correlation,
                            frequency_offset_hz: rotation * SYMBOL_RATE_HZ / TAU,
                            quality: {
                                let first = (slot_start_sample
                                    + sync_offset as f64 * samples_per_symbol)
                                    .floor() as usize;
                                let end = (slot_start_sample
                                    + (sync_offset + SYNC_SYMBOLS) as f64 * samples_per_symbol)
                                    .ceil() as usize;
                                samples.get(first..end).and_then(|raw| {
                                    quality::measure_sync(
                                        &symbols[index..index + SYNC_SYMBOLS + 1],
                                        reference,
                                        rotation,
                                        raw,
                                    )
                                })
                            },
                            fields,
                            hard_bits,
                        };
                        if let Some(previous) = found.iter_mut().find(|previous| {
                            (previous.slot_start_sample - slot_start_sample).abs()
                                < samples_per_symbol
                        }) {
                            if previous.sync_correlation < correlation {
                                *previous = burst;
                            }
                        } else {
                            if matches!(policy, OutputPolicy::Bounded | OutputPolicy::Shortened)
                                && found.len() == found.capacity()
                            {
                                found.clear();
                                return Err(AcquisitionError::OutputCapacity);
                            }
                            found.push(burst);
                        }
                    }
                }
            }
        }
        found.sort_unstable_by(|a, b| a.slot_start_sample.total_cmp(&b.slot_start_sample));
        Ok(())
    }
}

fn difference(previous: Symbol, current: Symbol) -> Symbol {
    Symbol {
        i: current.i * previous.i + current.q * previous.q,
        q: current.q * previous.i - current.i * previous.q,
    }
}

fn interpolate(samples: &[Symbol], position: f64) -> Symbol {
    let first = position as usize;
    let fraction = position - first as f64;
    let a = samples[first];
    let b = samples[first + 1];
    Symbol {
        i: a.i + (b.i - a.i) * fraction,
        q: a.q + (b.q - a.q) * fraction,
    }
}

impl Screen {
    fn correlate(&mut self, reference: &[Symbol; SYNC_SYMBOLS], windows: std::ops::Range<usize>) {
        let sum_i = &mut self.sum_i[windows.clone()];
        let sum_q = &mut self.sum_q[windows.clone()];
        sum_i.fill(0.0);
        sum_q.fill(0.0);
        for (lag, reference) in reference.iter().enumerate() {
            let (ri, rq) = (reference.i as f32, reference.q as f32);
            let start = windows.start + lag;
            let weighted_i = &self.weighted_i[start..start + sum_i.len()];
            let weighted_q = &self.weighted_q[start..start + sum_q.len()];
            for (((sum_i, sum_q), symbol_i), symbol_q) in sum_i
                .iter_mut()
                .zip(sum_q.iter_mut())
                .zip(weighted_i)
                .zip(weighted_q)
            {
                *sum_i += symbol_i * ri + symbol_q * rq;
                *sum_q += symbol_q * ri - symbol_i * rq;
            }
        }
    }
}

fn single_buffer(count: usize) -> Result<Vec<f32>, AcquisitionError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(count)
        .map_err(AcquisitionError::Allocation)?;
    buffer.resize(count, 0.0);
    Ok(buffer)
}

fn real_buffer(count: usize) -> Result<Vec<f64>, AcquisitionError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(count)
        .map_err(AcquisitionError::Allocation)?;
    buffer.resize(count, 0.0);
    Ok(buffer)
}

fn buffer(count: usize) -> Result<Vec<Symbol>, AcquisitionError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(count)
        .map_err(AcquisitionError::Allocation)?;
    buffer.resize(count, Symbol { i: 0.0, q: 0.0 });
    Ok(buffer)
}
