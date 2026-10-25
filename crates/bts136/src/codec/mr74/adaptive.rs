use super::{SUBFRAME_SAMPLES, pitch::PitchLag};
const MAX_LAG_SAMPLES: usize = 144;
const INTERPOLATION_SIDE_SAMPLES: usize = 10;
const PHASES: usize = 6;
const THIRD_PHASE_STEP: i16 = 2;
const ROUND_BIAS: i32 = 1 << 15;
const OUTPUT_SHIFT: u32 = 16;
pub const HISTORY_SAMPLES: usize = MAX_LAG_SAMPLES + INTERPOLATION_SIDE_SAMPLES;
// TS 26.073 Pred_lt_3or6 uses this 61-tap Q15 interpolation filter, distinct from the pitch-search filter.
const COEFFICIENTS: [i16; PHASES * INTERPOLATION_SIDE_SAMPLES + 1] = [
    29443, 28346, 25207, 20449, 14701, 8693, 3143, -1352, -4402, -5865, -5850, -4673, -2783, -672,
    1211, 2536, 3130, 2991, 2259, 1170, 0, -1001, -1652, -1868, -1666, -1147, -464, 218, 756, 1060,
    1099, 904, 550, 135, -245, -514, -634, -602, -451, -231, 0, 191, 308, 340, 296, 198, 78, -36,
    -120, -163, -165, -132, -79, -19, 34, 73, 91, 89, 70, 38, 0,
];
/// History is chronological. The caller stores final gain-combined excitation after prediction, not this adaptive vector alone.
pub fn predict(history: &[i16; HISTORY_SAMPLES], lag: PitchLag) -> [i16; SUBFRAME_SAMPLES] {
    let mut buffer = [0i16; HISTORY_SAMPLES + SUBFRAME_SAMPLES];
    buffer[..HISTORY_SAMPLES].copy_from_slice(history);
    let mut phase = -lag.fraction().thirds() * THIRD_PHASE_STEP;
    let mut start = HISTORY_SAMPLES - usize::from(lag.samples());
    if phase < 0 {
        phase += PHASES as i16;
        start -= 1;
    }
    let phase = phase as usize;
    for sample in 0..SUBFRAME_SAMPLES {
        let left = start + sample;
        let right = left + 1;
        let mut sum = 0i32;
        for tap in 0..INTERPOLATION_SIDE_SAMPLES {
            sum = mac(sum, buffer[left - tap], COEFFICIENTS[phase + tap * PHASES]);
            sum = mac(
                sum,
                buffer[right + tap],
                COEFFICIENTS[PHASES - phase + tap * PHASES],
            );
        }
        buffer[HISTORY_SAMPLES + sample] = (sum.saturating_add(ROUND_BIAS) >> OUTPUT_SHIFT) as i16;
    }
    buffer[HISTORY_SAMPLES..]
        .try_into()
        .expect("one predicted subframe follows history")
}
fn mac(sum: i32, sample: i16, coefficient: i16) -> i32 {
    sum.saturating_add((i32::from(sample) * i32::from(coefficient)).saturating_mul(2))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::pitch::PitchLag;
    #[test]
    fn recursive_prediction_matches_all_lags_across_saturation_and_impulse_histories() {
        const EXPECTED: u64 = 0x0c72c173aba278fa;
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;
        let mut hash = FNV_OFFSET;
        for pattern in 0..6 {
            let history = std::array::from_fn(|index| match pattern {
                0 => 0,
                1 => {
                    if index == HISTORY_SAMPLES - 1 {
                        30000
                    } else {
                        0
                    }
                }
                2 => {
                    if index % 2 == 0 {
                        i32::from(i16::MAX)
                    } else {
                        i32::from(i16::MIN)
                    }
                }
                3 => i32::from(i16::MAX),
                4 => i32::from(i16::MIN),
                _ => ((index * 347 + 997) % 65536) as i32 - 32768,
            } as i16);
            let mut include = |lag| {
                for byte in predict(&history, lag)
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes())
                {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
                }
            };
            for index in u8::MIN..=u8::MAX {
                include(PitchLag::from_absolute(index));
            }
            for previous_samples in 19..=144 {
                let previous = if previous_samples == 19 {
                    PitchLag::from_absolute(0)
                } else if previous_samples == 144 {
                    PitchLag::from_relative(31, PitchLag::from_absolute(255)).unwrap()
                } else if previous_samples <= 84 {
                    PitchLag::from_absolute((3 * previous_samples - 58) as u8)
                } else {
                    PitchLag::from_absolute((previous_samples + 112) as u8)
                };
                for index in 0..32 {
                    include(PitchLag::from_relative(index, previous).unwrap());
                }
            }
        }
        assert_eq!(hash, EXPECTED);
    }
}
