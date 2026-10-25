use super::{AcquiredSlot, AcquisitionWorkspace, REVERSE_DATA_SYMBOL, SLOT_SYMBOLS, interpolate};
use crate::phy::{
    modulation::{Dibit, DifferentialEncoder},
    pulse::SYMBOL_RATE_HZ,
};
use num_complex::Complex64;
use std::f64::consts::TAU;

const MAX_DELAY_SYMBOLS: i32 = 16;
const MAX_PATHS: usize = 4;
const FIRST_TRAINING_SYMBOL: usize = REVERSE_DATA_SYMBOL + MAX_DELAY_SYMBOLS as usize;
const TRAINING_SYMBOLS: usize = SLOT_SYMBOLS - MAX_DELAY_SYMBOLS as usize - FIRST_TRAINING_SYMBOL;
const MIN_ORTHOGONAL_POWER: f64 = 0.1;
const MIN_RELATIVE_PATH_POWER: f64 = 0.04;
const MIN_RELATIVE_FIT_POWER: f64 = MIN_RELATIVE_PATH_POWER / MAX_PATHS as f64;
const MAX_RESIDUAL_RATIO: f64 = 0.03;
const MAX_FREQUENCY_REFINEMENT_HZ: f64 = 200.0;
const MIN_SIGNAL_POWER: f64 = 1e-20;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelProfile {
    pub earliest_symbol: i32,
    pub latest_symbol: i32,
    pub main_path_power: f64,
    pub residual_ratio: f64,
}

pub(super) fn estimate<T>(
    workspace: &AcquisitionWorkspace,
    burst: &AcquiredSlot<T>,
) -> Option<ChannelProfile> {
    let mut reference = [Complex64::new(0.0, 0.0); SLOT_SYMBOLS];
    let mut encoder = DifferentialEncoder::default();
    for (index, value) in reference.iter_mut().enumerate().skip(REVERSE_DATA_SYMBOL) {
        let symbol = encoder.encode(Dibit::from_bits([
            burst.hard_bits[index * 2],
            burst.hard_bits[index * 2 + 1],
        ]));
        *value = Complex64::new(symbol.i, symbol.q);
    }
    let mut received = [Complex64::new(0.0, 0.0); TRAINING_SYMBOLS];
    for (index, value) in received.iter_mut().enumerate() {
        let symbol = FIRST_TRAINING_SYMBOL + index;
        let position = burst.slot_start_sample
            + workspace.filter.delay_samples() as f64
            + symbol as f64 * workspace.samples_per_symbol;
        if position < 0.0 || position + 1.0 >= workspace.filtered.len() as f64 {
            return None;
        }
        let sample = interpolate(&workspace.filtered, position);
        *value = Complex64::new(sample.i, sample.q)
            * Complex64::from_polar(
                1.0,
                -TAU * burst.frequency_offset_hz * symbol as f64 / SYMBOL_RATE_HZ,
            );
    }
    fit_with_refinement(&reference, &received)
}

fn fit_with_refinement(
    reference: &[Complex64; SLOT_SYMBOLS],
    received: &[Complex64; TRAINING_SYMBOLS],
) -> Option<ChannelProfile> {
    if let Some(profile) = fit(reference, received) {
        return Some(profile);
    }
    let half = TRAINING_SYMBOLS / 2;
    let partial: [Complex64; 2] = std::array::from_fn(|part| {
        (part * half..(part + 1) * half)
            .map(|n| reference[FIRST_TRAINING_SYMBOL + n].conj() * received[n])
            .sum()
    });
    if partial
        .iter()
        .any(|value| value.norm_sqr() <= MIN_SIGNAL_POWER)
    {
        return None;
    }
    let radians = (partial[0].conj() * partial[1]).arg() / half as f64;
    if (radians * SYMBOL_RATE_HZ / TAU).abs() > MAX_FREQUENCY_REFINEMENT_HZ {
        return None;
    }
    let corrected =
        std::array::from_fn(|n| received[n] * Complex64::from_polar(1.0, -radians * n as f64));
    fit(reference, &corrected)
}
fn column(reference: &[Complex64; SLOT_SYMBOLS], lag: i32) -> [Complex64; TRAINING_SYMBOLS] {
    std::array::from_fn(|index| {
        reference[(FIRST_TRAINING_SYMBOL as i32 + index as i32 - lag) as usize]
    })
}
fn dot(a: &[Complex64; TRAINING_SYMBOLS], b: &[Complex64; TRAINING_SYMBOLS]) -> Complex64 {
    a.iter().zip(b).map(|(a, b)| a.conj() * b).sum()
}
fn fit(
    reference: &[Complex64; SLOT_SYMBOLS],
    received: &[Complex64; TRAINING_SYMBOLS],
) -> Option<ChannelProfile> {
    let power: f64 = received.iter().map(|sample| sample.norm_sqr()).sum();
    if !power.is_finite() || power <= MIN_SIGNAL_POWER {
        return None;
    }
    let main_reference = column(reference, 0);
    let reference_energy = dot(&main_reference, &main_reference).re;
    if reference_energy <= MIN_SIGNAL_POWER {
        return None;
    }
    for lag in -MAX_DELAY_SYMBOLS..=MAX_DELAY_SYMBOLS {
        if lag == 0 {
            continue;
        }
        let candidate = column(reference, lag);
        let candidate_energy = dot(&candidate, &candidate).re;
        if dot(&main_reference, &candidate).norm_sqr()
            >= reference_energy * candidate_energy * (1.0 - MIN_ORTHOGONAL_POWER)
        {
            return None;
        }
    }
    let mut basis = [[Complex64::new(0.0, 0.0); TRAINING_SYMBOLS]; MAX_PATHS];
    let mut triangular = [[Complex64::new(0.0, 0.0); MAX_PATHS]; MAX_PATHS];
    let mut projections = [Complex64::new(0.0, 0.0); MAX_PATHS];
    let mut lags = [0; MAX_PATHS];
    let mut residual = *received;
    let mut paths = 0;
    let mut main_power = 0.0;
    for path in 0..MAX_PATHS {
        let mut best = None;
        let mut best_score = 0.0;
        for lag in -MAX_DELAY_SYMBOLS..=MAX_DELAY_SYMBOLS {
            if (path == 0 && lag != 0) || (path != 0 && lags[..path].contains(&lag)) {
                continue;
            }
            let original = column(reference, lag);
            let mut orthogonal = original;
            let mut coefficients = [Complex64::new(0.0, 0.0); MAX_PATHS];
            for prior in 0..path {
                coefficients[prior] = dot(&basis[prior], &original);
                for (value, basis) in orthogonal.iter_mut().zip(&basis[prior]) {
                    *value -= coefficients[prior] * basis;
                }
            }
            let energy = dot(&orthogonal, &orthogonal).re;
            if energy < TRAINING_SYMBOLS as f64 * MIN_ORTHOGONAL_POWER {
                continue;
            }
            let correlation = dot(&orthogonal, &residual);
            let score = correlation.norm_sqr() / energy;
            if score > best_score {
                best_score = score;
                best = Some((lag, orthogonal, coefficients, energy));
            }
        }
        let Some((lag, orthogonal, coefficients, energy)) = best else {
            break;
        };
        let gain_power = best_score / energy;
        if path != 0 && gain_power < main_power * MIN_RELATIVE_FIT_POWER {
            break;
        }
        if path == 0 {
            main_power = gain_power;
        }
        let norm = energy.sqrt();
        for (value, input) in basis[path].iter_mut().zip(orthogonal) {
            *value = input / norm;
        }
        for prior in 0..path {
            triangular[prior][path] = coefficients[prior];
        }
        triangular[path][path] = Complex64::new(norm, 0.0);
        projections[path] = dot(&basis[path], received);
        for (value, basis) in residual.iter_mut().zip(&basis[path]) {
            *value -= projections[path] * basis;
        }
        lags[path] = lag;
        paths += 1;
    }
    let residual_ratio = dot(&residual, &residual).re / power;
    if paths == 0 || !residual_ratio.is_finite() || residual_ratio > MAX_RESIDUAL_RATIO {
        return None;
    }
    let mut gains = [Complex64::new(0.0, 0.0); MAX_PATHS];
    for path in (0..paths).rev() {
        let following: Complex64 = (path + 1..paths)
            .map(|next| triangular[path][next] * gains[next])
            .sum();
        gains[path] = (projections[path] - following) / triangular[path][path];
    }
    let main_path_power = gains[0].norm_sqr();
    if main_path_power <= MIN_SIGNAL_POWER {
        return None;
    }
    let mut earliest_symbol = 0;
    let mut latest_symbol = 0;
    for path in 1..paths {
        let power = gains[path].norm_sqr();
        let significant = power >= main_path_power * MIN_RELATIVE_PATH_POWER
            || (power >= main_path_power * MIN_RELATIVE_FIT_POWER
                && (1..paths).any(|neighbor| {
                    neighbor != path
                        && lags[path].abs_diff(lags[neighbor]) == 1
                        && gains[neighbor].norm_sqr() >= main_path_power * MIN_RELATIVE_FIT_POWER
                        && power + gains[neighbor].norm_sqr()
                            >= main_path_power * MIN_RELATIVE_PATH_POWER
                }));
        if significant {
            earliest_symbol = earliest_symbol.min(lags[path]);
            latest_symbol = latest_symbol.max(lags[path]);
        }
    }
    Some(ChannelProfile {
        earliest_symbol,
        latest_symbol,
        main_path_power,
        residual_ratio,
    })
}
#[cfg(test)]
mod tests;
