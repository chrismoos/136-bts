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

use std::{collections::TryReserveError, error::Error, fmt};

const FILTER_HALF_SPAN_SYMBOLS: usize = 8;
const TIMING_PHASES: usize = 16;
const SLOT_SYMBOLS: usize = SLOT_BITS / 2;
const REVERSE_SYNC_SYMBOL: usize = 14;
const MIN_SYNC_CORRELATION: f64 = 0.90;
const MIN_SYMBOL_POWER: f64 = 1e-20;
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
        .acquire(
            samples,
            sync_offsets,
            decision_symbols,
            decode,
            &mut found,
            OutputPolicy::Grow,
        )
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
pub struct AcquisitionWorkspace {
    filter: RootRaisedCosine,
    filtered: Vec<Symbol>,
    symbols: Vec<Symbol>,
    differences: Vec<Symbol>,
    samples_per_symbol: f64,
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
            filter,
            filtered: buffer(max_samples)?,
            symbols: buffer(max_symbols)?,
            differences: buffer(max_symbols)?,
            samples_per_symbol,
        })
    }
    /// Replaces output only after input validation. Reserve output capacity beforehand. Capacity failure clears partial results.
    pub fn reverse_traffic(
        &mut self,
        samples: &[Symbol],
        output: &mut Vec<ReverseTrafficBurst>,
    ) -> Result<(), AcquisitionError> {
        self.acquire(
            samples,
            &[REVERSE_SYNC_SYMBOL],
            REVERSE_DATA_SYMBOL..SLOT_SYMBOLS,
            |bits, _| traffic::decode_reverse(bits).ok(),
            output,
            OutputPolicy::Bounded,
        )
    }
    pub fn shortened(
        &mut self,
        samples: &[Symbol],
        output: &mut Vec<AcquiredSlot<ShortenedBurst>>,
    ) -> Result<(), AcquisitionError> {
        self.acquire(
            samples,
            &shortened::SYNC_OFFSETS,
            shortened::SYNC_OFFSETS[0]..shortened::ACTIVE_END_SYMBOL,
            |bits, _| shortened::decode(bits),
            output,
            OutputPolicy::Shortened,
        )
    }
    pub fn reverse_control(
        &mut self,
        samples: &[Symbol],
        format: BurstFormat,
        dvcc: u8,
        output: &mut Vec<ReverseControlBurst>,
    ) -> Result<(), AcquisitionError> {
        self.acquire(
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
        )
    }
    fn acquire<T>(
        &mut self,
        samples: &[Symbol],
        sync_offsets: &[usize],
        decision_symbols: std::ops::Range<usize>,
        decode: impl Fn(&[bool; SLOT_BITS], &[f64; SLOT_BITS]) -> Option<T>,
        found: &mut Vec<AcquiredSlot<T>>,
        policy: OutputPolicy,
    ) -> Result<(), AcquisitionError> {
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
        let filter = &self.filter;
        let filtered = &mut self.filtered[..samples.len()];
        filter.filter_into(samples, filtered);
        let samples_per_symbol = self.samples_per_symbol;
        found.clear();
        for phase in 0..TIMING_PHASES {
            let offset = phase as f64 * samples_per_symbol / TIMING_PHASES as f64;
            let symbol_count =
                ((filtered.len() as f64 - 1.0 - offset) / samples_per_symbol).max(0.0) as usize;
            let symbols = &mut self.symbols[..symbol_count];
            for (index, symbol) in symbols.iter_mut().enumerate() {
                *symbol = interpolate(filtered, offset + index as f64 * samples_per_symbol);
            }
            let differences = &mut self.differences[..symbol_count.saturating_sub(1)];
            for (output, pair) in differences.iter_mut().zip(symbols.windows(2)) {
                *output = difference(pair[0], pair[1]);
            }
            for word in SyncWord::ALL {
                let bits = word.bits();
                let reference: [Symbol; SYNC_SYMBOLS] = std::array::from_fn(|index| {
                    let bits = bits.as_chunks::<2>().0[index];
                    let phase = f64::from(Dibit::from_bits(bits).phase_quarters())
                        * std::f64::consts::FRAC_PI_4;
                    let (q, i) = phase.sin_cos();
                    Symbol { i, q }
                });
                for (index, window) in differences
                    .windows(SYNC_SYMBOLS)
                    .enumerate()
                    .skip(sync_offsets[0])
                {
                    let (sum, total_norm) = window.iter().zip(&reference).fold(
                        (Symbol { i: 0.0, q: 0.0 }, 0.0),
                        |(sum, total_norm), (symbol, reference)| {
                            let norm = (symbol.i * symbol.i + symbol.q * symbol.q).sqrt();
                            if norm * norm < MIN_SYMBOL_POWER {
                                return (sum, total_norm);
                            }
                            let product = difference(*reference, *symbol);
                            (
                                Symbol {
                                    i: sum.i + product.i,
                                    q: sum.q + product.q,
                                },
                                total_norm + norm,
                            )
                        },
                    );
                    if !total_norm.is_finite() || total_norm == 0.0 {
                        continue;
                    }
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
                        let slot_start_sample = offset + (start + 1) as f64 * samples_per_symbol
                            - filter.delay_samples() as f64;
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
                                        &reference,
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

fn buffer(count: usize) -> Result<Vec<Symbol>, AcquisitionError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(count)
        .map_err(AcquisitionError::Allocation)?;
    buffer.resize(count, Symbol { i: 0.0, q: 0.0 });
    Ok(buffer)
}
