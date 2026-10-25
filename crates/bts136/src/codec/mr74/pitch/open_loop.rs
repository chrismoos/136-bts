use super::{MAX_SEARCH_LAG_SAMPLES, MIN_SEARCH_LAG_SAMPLES, search::OpenLoopLag};
use crate::codec::mr74::{decoder::FRAME_SAMPLES, energy::inverse_sqrt};
const HALF_FRAME_SAMPLES: usize = FRAME_SAMPLES / 2;
const HISTORY_SAMPLES: usize = MAX_SEARCH_LAG_SAMPLES as usize;
const INPUT_SAMPLES: usize = HISTORY_SAMPLES + HALF_FRAME_SAMPLES;
const MINIMUM_LAG: usize = MIN_SEARCH_LAG_SAMPLES as usize;
const SCALE_SHIFT: u32 = 3;
const LOW_ENERGY_LIMIT: i32 = 1 << 20;
const SHORT_LAG_THRESHOLD_Q15: i16 = 27853;
const WORD_SHIFT: u32 = 16;
const Q15_SHIFT: u32 = 15;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detector {
    history_q0: [i16; HISTORY_SAMPLES],
}
impl Default for Detector {
    fn default() -> Self {
        Self {
            history_q0: [0; HISTORY_SAMPLES],
        }
    }
}
impl Detector {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 Pitch_ol for MR74 without DTX. Supply consecutive 80-sample intervals of perceptually weighted speech.
    pub fn push(&mut self, samples_q0: &[i16; HALF_FRAME_SAMPLES]) -> OpenLoopLag {
        let mut input = [0i16; INPUT_SAMPLES];
        input[..HISTORY_SAMPLES].copy_from_slice(&self.history_q0);
        input[HISTORY_SAMPLES..].copy_from_slice(samples_q0);
        self.history_q0
            .copy_from_slice(&input[HALF_FRAME_SAMPLES..]);
        let energy = input
            .iter()
            .fold(0i32, |sum, &v| sum.saturating_add(product(v, v)));
        if energy == i32::MAX {
            for value in &mut input {
                *value >>= SCALE_SHIFT;
            }
        } else if energy < LOW_ENERGY_LIMIT {
            for value in &mut input {
                *value = shift_word(*value, SCALE_SHIFT);
            }
        }
        let correlation = std::array::from_fn::<_, { HISTORY_SAMPLES + 1 }, _>(|lag| {
            if lag < MINIMUM_LAG {
                return 0;
            }
            input[HISTORY_SAMPLES..]
                .iter()
                .zip(&input[HISTORY_SAMPLES - lag..])
                .fold(0i32, |sum, (&a, &b)| sum.saturating_add(product(a, b)))
        });
        let (mut lag, mut maximum) =
            candidate(&input, &correlation, MINIMUM_LAG * 4, HISTORY_SAMPLES);
        for (minimum, maximum_lag) in [
            (MINIMUM_LAG * 2, MINIMUM_LAG * 4 - 1),
            (MINIMUM_LAG, MINIMUM_LAG * 2 - 1),
        ] {
            let (candidate, value) = candidate(&input, &correlation, minimum, maximum_lag);
            if fractional(maximum, SHORT_LAG_THRESHOLD_Q15) < value {
                lag = candidate;
                maximum = value;
            }
        }
        OpenLoopLag::try_new(lag as u16)
            .expect("open-loop selection stays within codec pitch bounds")
    }
}
fn candidate(
    input: &[i16; INPUT_SAMPLES],
    correlation: &[i32; HISTORY_SAMPLES + 1],
    minimum: usize,
    maximum: usize,
) -> (usize, i16) {
    let mut lag = maximum;
    let mut best = i32::MIN;
    for i in (minimum..=maximum).rev() {
        if correlation[i] >= best {
            lag = i;
            best = correlation[i];
        }
    }
    let energy = input[HISTORY_SAMPLES - lag..HISTORY_SAMPLES - lag + HALF_FRAME_SAMPLES]
        .iter()
        .fold(0i32, |sum, &v| sum.saturating_add(product(v, v)));
    (lag, multiply(best & !1, inverse_sqrt(energy) & !1) as i16)
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn fractional(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
fn split(value: i32) -> (i16, i16) {
    let high = (value >> WORD_SHIFT) as i16;
    (high, ((value >> 1) - (i32::from(high) << Q15_SHIFT)) as i16)
}
fn multiply(left: i32, right: i32) -> i32 {
    let (lh, ll) = split(left);
    let (rh, rl) = split(right);
    product(lh, rh)
        .saturating_add(product(fractional(lh, rl), 1))
        .saturating_add(product(fractional(ll, rh), 1))
}
fn shift_word(value: i16, shift: u32) -> i16 {
    (i32::from(value) << shift).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
#[cfg(test)]
mod tests;
