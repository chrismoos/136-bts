use super::modulation::Symbol;
use std::{error::Error, f64::consts::PI, fmt};

pub const SYMBOL_RATE_HZ: f64 = 24_300.0;
const ROLL_OFF: f64 = 0.35;
const MAX_HALF_TAPS: usize = 4096;
const SINGULARITY_TOLERANCE: f64 = 1e-10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPulseRate;
impl fmt::Display for InvalidPulseRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("pulse filter requires finite rate of at least two samples per symbol and a bounded nonzero span")
    }
}
impl Error for InvalidPulseRate {}

#[derive(Debug)]
pub struct RootRaisedCosine {
    taps: Vec<f64>,
    delay_samples: usize,
}
impl RootRaisedCosine {
    // IS-136.2-A §2.1.3.3.1, root raised cosine with alpha 0.35.
    pub fn try_new(
        sample_rate_hz: f64,
        half_span_symbols: usize,
    ) -> Result<Self, InvalidPulseRate> {
        let samples_per_symbol = sample_rate_hz / SYMBOL_RATE_HZ;
        let half = (samples_per_symbol * half_span_symbols as f64).ceil();
        if !samples_per_symbol.is_finite()
            || samples_per_symbol < 2.0
            || half < 1.0
            || half > MAX_HALF_TAPS as f64
        {
            return Err(InvalidPulseRate);
        }
        let delay_samples = half as usize;
        let mut taps: Vec<f64> = (0..=delay_samples * 2)
            .map(|index| pulse((index as f64 - delay_samples as f64) / samples_per_symbol))
            .collect();
        let norm = taps.iter().map(|tap| tap * tap).sum::<f64>().sqrt();
        for tap in &mut taps {
            *tap /= norm;
        }
        Ok(Self {
            taps,
            delay_samples,
        })
    }

    pub fn delay_samples(&self) -> usize {
        self.delay_samples
    }
    pub fn taps(&self) -> &[f64] {
        &self.taps
    }

    pub fn filter(&self, samples: &[Symbol]) -> Vec<Symbol> {
        let mut out = vec![Symbol { i: 0.0, q: 0.0 }; samples.len()];
        self.filter_into(samples, &mut out);
        out
    }

    pub(crate) fn filter_into(&self, samples: &[Symbol], out: &mut [Symbol]) {
        assert_eq!(samples.len(), out.len());
        out.fill(Symbol { i: 0.0, q: 0.0 });
        for (index, sample) in samples.iter().enumerate() {
            for (tap, output) in self.taps.iter().zip(out[index..].iter_mut()) {
                output.i += sample.i * tap;
                output.q += sample.q * tap;
            }
        }
    }
}

fn pulse(time: f64) -> f64 {
    if time.abs() < SINGULARITY_TOLERANCE {
        return 1.0 + ROLL_OFF * (4.0 / PI - 1.0);
    }
    if (time.abs() - 1.0 / (4.0 * ROLL_OFF)).abs() < SINGULARITY_TOLERANCE {
        return ROLL_OFF / 2.0f64.sqrt()
            * ((1.0 + 2.0 / PI) * (PI / (4.0 * ROLL_OFF)).sin()
                + (1.0 - 2.0 / PI) * (PI / (4.0 * ROLL_OFF)).cos());
    }
    ((PI * time * (1.0 - ROLL_OFF)).sin()
        + 4.0 * ROLL_OFF * time * (PI * time * (1.0 + ROLL_OFF)).cos())
        / (PI * time * (1.0 - (4.0 * ROLL_OFF * time).powi(2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_is_symmetric_normalized_and_finite() {
        let filter = RootRaisedCosine::try_new(SYMBOL_RATE_HZ * 4.0, 8).unwrap();
        for (a, b) in filter.taps.iter().zip(filter.taps.iter().rev()) {
            assert_eq!(a, b);
        }
        assert!((filter.taps.iter().map(|x| x * x).sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(pulse(0.0).is_finite());
        assert!(pulse(1.0 / (4.0 * ROLL_OFF)).is_finite());
        for rate in [f64::NAN, f64::INFINITY, 0.0, SYMBOL_RATE_HZ] {
            assert!(RootRaisedCosine::try_new(rate, 8).is_err());
        }
        assert!(RootRaisedCosine::try_new(SYMBOL_RATE_HZ * 4.0, 0).is_err());
    }
}
