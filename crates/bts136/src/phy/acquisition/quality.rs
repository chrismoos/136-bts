use super::{MIN_SYMBOL_POWER, Symbol, difference};
use num_complex::Complex64;
const PERCENT: f64 = 100.0;
const POWER_DB: f64 = 10.0;
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BurstQuality {
    pub sync_evm_percent: f64,
    pub sync_level_dbfs: f64,
}
pub(super) fn measure_sync(
    symbols: &[Symbol],
    differences: &[Symbol],
    rotation: f64,
    raw: &[Symbol],
) -> Option<BurstQuality> {
    if symbols.len() != crate::phy::sync::SYNC_SYMBOLS + 1
        || differences.len() != crate::phy::sync::SYNC_SYMBOLS
    {
        return None;
    }
    // The predecessor of a shortened sync word can be a zero spacer.
    measure(&symbols[1..], &differences[1..], rotation, raw)
}
fn measure(
    symbols: &[Symbol],
    differences: &[Symbol],
    rotation: f64,
    raw: &[Symbol],
) -> Option<BurstQuality> {
    if symbols.len() != differences.len() + 1 || raw.is_empty() {
        return None;
    }
    let mut reference = Complex64::new(1.0, 0.0);
    let mut projection = Complex64::new(0.0, 0.0);
    for (index, symbol) in symbols.iter().enumerate() {
        if index > 0 {
            let step = differences[index - 1];
            reference *= Complex64::new(step.i, step.q);
        }
        let received = Complex64::new(symbol.i, symbol.q)
            * Complex64::from_polar(1.0, -rotation * index as f64);
        projection += received * reference.conj();
    }
    let gain = projection / symbols.len() as f64;
    let signal_power = gain.norm_sqr();
    if !signal_power.is_finite() || signal_power < MIN_SYMBOL_POWER {
        return None;
    }
    reference = Complex64::new(1.0, 0.0);
    let mut residual = 0.0;
    for (index, symbol) in symbols.iter().enumerate() {
        if index > 0 {
            let step = differences[index - 1];
            reference *= Complex64::new(step.i, step.q);
        }
        let received = Complex64::new(symbol.i, symbol.q)
            * Complex64::from_polar(1.0, -rotation * index as f64);
        residual += (received - gain * reference).norm_sqr();
    }
    let raw_power = raw
        .iter()
        .map(|symbol| difference(*symbol, *symbol).i)
        .sum::<f64>()
        / raw.len() as f64;
    let evm = PERCENT * (residual / (symbols.len() as f64 * signal_power)).sqrt();
    if !evm.is_finite() || !raw_power.is_finite() || raw_power < MIN_SYMBOL_POWER {
        return None;
    }
    Some(BurstQuality {
        sync_evm_percent: evm,
        sync_level_dbfs: POWER_DB * raw_power.log10(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::phy::{modulation::Dibit, sync::SyncWord};
    fn measurement(error: f64, gain: Complex64, rotation: f64) -> BurstQuality {
        let bits = SyncWord::One.bits();
        let steps: Vec<_> = bits
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bits| {
                let z = Complex64::from_polar(
                    1.0,
                    f64::from(Dibit::from_bits(*bits).phase_quarters())
                        * std::f64::consts::FRAC_PI_4,
                );
                Symbol { i: z.re, q: z.im }
            })
            .collect();
        let mut reference = Complex64::new(1.0, 0.0);
        let symbols: Vec<_> = (0..=steps.len())
            .map(|index| {
                if index > 0 {
                    let step = steps[index - 1];
                    reference *= Complex64::new(step.i, step.q);
                }
                let noise = if index == steps.len() {
                    0.0
                } else if index % 2 == 0 {
                    error
                } else {
                    -error
                };
                let z = gain
                    * reference
                    * Complex64::new(1.0, noise)
                    * Complex64::from_polar(1.0, rotation * index as f64);
                Symbol { i: z.re, q: z.im }
            })
            .collect();
        measure(&symbols, &steps, rotation, &[Symbol { i: 0.1, q: 0.0 }; 32]).unwrap()
    }
    #[test]
    fn known_error_is_normalized_and_independent_of_gain_phase_and_cfo() {
        for gain in [Complex64::new(1.0, 0.0), Complex64::from_polar(0.2, 1.3)] {
            let clean = measurement(0.0, gain, 0.07);
            assert!(clean.sync_evm_percent < 1e-10);
            assert!((clean.sync_level_dbfs + 20.0).abs() < 1e-10);
            let noisy = measurement(0.1, gain, 0.07);
            let expected = 10.0 * (14.0_f64 / 15.0).sqrt();
            assert!(
                (noisy.sync_evm_percent - expected).abs() < 1e-9,
                "{noisy:?}"
            );
            assert!(measurement(0.2, gain, 0.07).sync_evm_percent > noisy.sync_evm_percent);
        }
    }
    #[test]
    fn zero_signal_cannot_produce_a_good_quality_reading() {
        assert!(
            measure(
                &[Symbol { i: 0.0, q: 0.0 }; 2],
                &[Symbol { i: 1.0, q: 0.0 }],
                0.0,
                &[Symbol { i: 0.0, q: 0.0 }]
            )
            .is_none()
        );
    }
    #[test]
    fn erased_sync_predecessor_does_not_add_an_error_floor() {
        let bits = SyncWord::One.bits();
        let steps: Vec<_> = bits
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bits| {
                let z = Complex64::from_polar(
                    1.0,
                    f64::from(Dibit::from_bits(*bits).phase_quarters())
                        * std::f64::consts::FRAC_PI_4,
                );
                Symbol { i: z.re, q: z.im }
            })
            .collect();
        let mut symbols = vec![Symbol { i: 0.0, q: 0.0 }];
        let mut reference = Complex64::new(1.0, 0.0);
        for step in &steps {
            reference *= Complex64::new(step.i, step.q);
            symbols.push(Symbol {
                i: reference.re,
                q: reference.im,
            });
        }
        let raw = [Symbol { i: 0.1, q: 0.0 }; 32];
        let biased = measure(&symbols, &steps, 0.0, &raw).unwrap();
        assert!(biased.sync_evm_percent > 25.0);
        let measured = measure_sync(&symbols, &steps, 0.0, &raw).unwrap();
        assert!(measured.sync_evm_percent < 1e-10);
    }
}
