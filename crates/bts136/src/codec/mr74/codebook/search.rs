use super::{Codebook, PULSES, TRACKS};
use crate::codec::mr74::{SUBFRAME_SAMPLES, energy::inverse_sqrt, pitch::PitchLag};
const POSITIONS_PER_TRACK: usize = SUBFRAME_SAMPLES / TRACKS;
const KEPT_STARTS: usize = 4;
const Q15_SHIFT: u32 = 15;
const WORD_SHIFT: u32 = 16;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
const HALF_Q15: i16 = 16384;
const QUARTER_Q15: i16 = 8192;
const EIGHTH_Q15: i16 = 4096;
const SIXTEENTH_Q15: i16 = 2048;
const IMPULSE_SCALE_SHIFT: i16 = 9;
const INVERSE_SCALE_SHIFT: i16 = 7;
const IMPULSE_HEADROOM_Q15: i16 = 32440;
const GRAY: [u16; POSITIONS_PER_TRACK] = [0, 1, 3, 2, 6, 4, 5, 7];
const LAST_TRACK_SHIFT: usize = 9;
const LAST_POSITION_SHIFT: usize = 10;
const GRAY_BITS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub pulse_positions: u16,
    pub pulse_signs: u16,
    pub innovation_q13: [i16; SUBFRAME_SAMPLES],
    pub filtered_q12: [i16; SUBFRAME_SAMPLES],
    pub sharpened_impulse_q12: [i16; SUBFRAME_SAMPLES],
}
/// TS 26.073 code_4i40_17bits, including target sign fixing, four-start pruning and recursive pitch sharpening.
pub fn search(
    target: &[i16; SUBFRAME_SAMPLES],
    impulse_q12: &[i16; SUBFRAME_SAMPLES],
    lag: PitchLag,
    pitch_sharp_q14: i16,
) -> SearchResult {
    let delay = usize::from(lag.samples());
    let sharp = pitch_sharp_q14.saturating_mul(2);
    let mut impulse = *impulse_q12;
    sharpen(&mut impulse, delay, sharp);
    let mut correlation = target_correlation(target, &impulse);
    let (signs, eligible) = fix_signs(&mut correlation);
    let matrix = impulse_correlation(&impulse, &signs);
    let pulses = select(&correlation, &eligible, &matrix);
    let mut positions = 0u16;
    let mut sign_bits = 0u16;
    for &position in &pulses {
        let track = position % TRACKS;
        let gray = GRAY[position / TRACKS];
        let bit = track.min(PULSES - 1);
        positions += if track < PULSES - 1 {
            gray << (track * GRAY_BITS)
        } else {
            (gray << LAST_POSITION_SHIFT) | (((track - (PULSES - 1)) as u16) << LAST_TRACK_SHIFT)
        };
        if signs[position] > 0 {
            sign_bits |= 1 << bit;
        }
    }
    let mut innovation = *Codebook::try_new(positions, sign_bits)
        .expect("searched pulse positions and signs fit the codebook")
        .samples_q13();
    let filtered = std::array::from_fn(|i| {
        let mut sum = 0i32;
        for &position in &pulses {
            let sample = if i >= position {
                impulse[i - position]
            } else {
                0
            };
            let sign = if signs[position] > 0 {
                i16::MAX
            } else {
                i16::MIN
            };
            sum = sum.saturating_add(product(sample, sign));
        }
        rounded(sum)
    });
    sharpen(&mut innovation, delay, sharp);
    SearchResult {
        pulse_positions: positions,
        pulse_signs: sign_bits,
        innovation_q13: innovation,
        filtered_q12: filtered,
        sharpened_impulse_q12: impulse,
    }
}
fn sharpen(samples: &mut [i16; SUBFRAME_SAMPLES], lag: usize, gain: i16) {
    for i in lag..SUBFRAME_SAMPLES {
        samples[i] = samples[i].saturating_add(fractional(samples[i - lag], gain));
    }
}
fn target_correlation(
    target: &[i16; SUBFRAME_SAMPLES],
    impulse: &[i16; SUBFRAME_SAMPLES],
) -> [i16; SUBFRAME_SAMPLES] {
    const INITIAL_MAXIMUM_SUM: i32 = 5;
    let mut raw = [0i32; SUBFRAME_SAMPLES];
    let mut total = INITIAL_MAXIMUM_SUM;
    for track in 0..TRACKS {
        let mut maximum = 0;
        for i in (track..SUBFRAME_SAMPLES).step_by(TRACKS) {
            let sum = target[i..]
                .iter()
                .zip(impulse)
                .fold(0i32, |sum, (&x, &h)| sum.saturating_add(product(x, h)));
            raw[i] = sum;
            maximum = maximum.max(sum.saturating_abs());
        }
        total = total.saturating_add(maximum >> 1);
    }
    let shift = normalize(total) - 1;
    raw.map(|value| rounded(scaled(value, shift)))
}
fn fix_signs(
    correlation: &mut [i16; SUBFRAME_SAMPLES],
) -> ([i16; SUBFRAME_SAMPLES], [bool; SUBFRAME_SAMPLES]) {
    let signs = correlation.map(|value| if value >= 0 { i16::MAX } else { -i16::MAX });
    for value in correlation.iter_mut() {
        *value = value.saturating_abs();
    }
    let mut candidates = *correlation;
    let mut position = 0;
    for track in 0..TRACKS {
        for _ in 0..POSITIONS_PER_TRACK - KEPT_STARTS {
            let mut minimum = i16::MAX;
            for i in (track..SUBFRAME_SAMPLES).step_by(TRACKS) {
                if candidates[i] >= 0 && candidates[i] < minimum {
                    minimum = candidates[i];
                    position = i;
                }
            }
            candidates[position] = -1;
        }
    }
    (signs, candidates.map(|v| v >= 0))
}
fn impulse_correlation(
    impulse: &[i16; SUBFRAME_SAMPLES],
    signs: &[i16; SUBFRAME_SAMPLES],
) -> [[i16; SUBFRAME_SAMPLES]; SUBFRAME_SAMPLES] {
    let energy = impulse
        .iter()
        .fold(2i32, |sum, &h| sum.saturating_add(product(h, h)));
    let normalized = if energy >> WORD_SHIFT == i32::from(i16::MAX) {
        impulse.map(|h| h >> 1)
    } else {
        let gain = fractional(
            (scaled(inverse_sqrt(energy >> 1), INVERSE_SCALE_SHIFT) >> WORD_SHIFT) as i16,
            IMPULSE_HEADROOM_Q15,
        );
        impulse.map(|h| rounded(scaled(product(h, gain), IMPULSE_SCALE_SHIFT)))
    };
    let mut matrix = [[0i16; SUBFRAME_SAMPLES]; SUBFRAME_SAMPLES];
    let mut sum = 0i32;
    for (k, &h) in normalized.iter().enumerate() {
        sum = sum.saturating_add(product(h, h));
        let i = SUBFRAME_SAMPLES - 1 - k;
        matrix[i][i] = rounded(sum);
    }
    for difference in 1..SUBFRAME_SAMPLES {
        let mut sum = 0i32;
        for k in 0..SUBFRAME_SAMPLES - difference {
            sum = sum.saturating_add(product(normalized[k], normalized[k + difference]));
            let j = SUBFRAME_SAMPLES - 1 - k;
            let i = j - difference;
            let value = fractional(rounded(sum), fractional(signs[i], signs[j]));
            matrix[i][j] = value;
            matrix[j][i] = value;
        }
    }
    matrix
}
fn select(
    correlation: &[i16; SUBFRAME_SAMPLES],
    eligible: &[bool; SUBFRAME_SAMPLES],
    matrix: &[[i16; SUBFRAME_SAMPLES]; SUBFRAME_SAMPLES],
) -> [usize; PULSES] {
    let mut best = [0, 1, 2, 3];
    let mut best_square = -1;
    let mut best_energy = 1;
    for last_track in PULSES - 1..TRACKS {
        let mut tracks = [0, 1, 2, last_track];
        for _ in 0..PULSES {
            for first in (tracks[0]..SUBFRAME_SAMPLES).step_by(TRACKS) {
                if !eligible[first] {
                    continue;
                }
                let mut selected = [first, tracks[1], tracks[2], tracks[3]];
                let mut correlation_sum = correlation[first];
                let mut energy = 1;
                let mut square = -1;
                let mut base_energy = product(matrix[first][first], QUARTER_Q15);
                for pulse in 1..PULSES {
                    let mut best_correlation = 0;
                    let mut chosen = tracks[pulse];
                    energy = 1;
                    square = -1;
                    for candidate in (tracks[pulse]..SUBFRAME_SAMPLES).step_by(TRACKS) {
                        let sum = correlation_sum.saturating_add(correlation[candidate]);
                        let (diagonal_scale, cross_scale) = if pulse == 1 {
                            (QUARTER_Q15, HALF_Q15)
                        } else {
                            (SIXTEENTH_Q15, EIGHTH_Q15)
                        };
                        let mut candidate_energy = base_energy
                            .saturating_add(product(matrix[candidate][candidate], diagonal_scale));
                        for prior in (0..pulse).rev() {
                            candidate_energy = candidate_energy.saturating_add(product(
                                matrix[selected[prior]][candidate],
                                cross_scale,
                            ));
                        }
                        let candidate_square = fractional(sum, sum);
                        let candidate_energy = rounded(candidate_energy);
                        if product(energy, candidate_square)
                            .saturating_sub(product(square, candidate_energy))
                            > 0
                        {
                            square = candidate_square;
                            energy = candidate_energy;
                            best_correlation = sum;
                            chosen = candidate;
                        }
                    }
                    selected[pulse] = chosen;
                    correlation_sum = best_correlation;
                    base_energy = if pulse == 1 {
                        product(energy, QUARTER_Q15)
                    } else {
                        i32::from(energy) << WORD_SHIFT
                    };
                }
                if product(best_energy, square).saturating_sub(product(best_square, energy)) > 0 {
                    best = selected;
                    best_square = square;
                    best_energy = energy;
                }
            }
            tracks.rotate_right(1);
        }
    }
    best
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn fractional(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
fn rounded(value: i32) -> i16 {
    (value.saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16
}
fn normalize(value: i32) -> i16 {
    if value == 0 {
        0
    } else {
        value.leading_zeros() as i16 - 1
    }
}
fn scaled(value: i32, shift: i16) -> i32 {
    if shift < 0 {
        value >> (shift.unsigned_abs().min(31))
    } else {
        (i64::from(value) << shift.min(31)).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }
}

#[cfg(test)]
mod tests;
