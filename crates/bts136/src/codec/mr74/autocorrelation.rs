use super::spectral::LPC_ORDER;

pub const WINDOW_SAMPLES: usize = 240;
const CORRELATIONS: usize = LPC_ORDER + 1;
const Q15_SHIFT: u32 = 15;
const WORD_SHIFT: u32 = 16;
const ROUND_BIAS: i32 = 1 << (Q15_SHIFT - 1);
const OVERFLOW_SAMPLE_SHIFT: u32 = 2;
const OVERFLOW_ENERGY_SHIFT: i16 = 4;
// TS 26.073 window_200_40 uses 40 samples of lookahead for MR74 LPC analysis.
const WINDOW_Q15: [i16; WINDOW_SAMPLES] = [
    2621, 2623, 2629, 2638, 2651, 2668, 2689, 2713, 2741, 2772, 2808, 2847, 2890, 2936, 2986, 3040,
    3097, 3158, 3223, 3291, 3363, 3438, 3517, 3599, 3685, 3774, 3867, 3963, 4063, 4166, 4272, 4382,
    4495, 4611, 4731, 4853, 4979, 5108, 5240, 5376, 5514, 5655, 5800, 5947, 6097, 6250, 6406, 6565,
    6726, 6890, 7057, 7227, 7399, 7573, 7750, 7930, 8112, 8296, 8483, 8672, 8863, 9057, 9252, 9450,
    9650, 9852, 10055, 10261, 10468, 10677, 10888, 11101, 11315, 11531, 11748, 11967, 12187, 12409,
    12632, 12856, 13082, 13308, 13536, 13764, 13994, 14225, 14456, 14688, 14921, 15155, 15389,
    15624, 15859, 16095, 16331, 16568, 16805, 17042, 17279, 17516, 17754, 17991, 18228, 18465,
    18702, 18939, 19175, 19411, 19647, 19882, 20117, 20350, 20584, 20816, 21048, 21279, 21509,
    21738, 21967, 22194, 22420, 22644, 22868, 23090, 23311, 23531, 23749, 23965, 24181, 24394,
    24606, 24816, 25024, 25231, 25435, 25638, 25839, 26037, 26234, 26428, 26621, 26811, 26999,
    27184, 27368, 27548, 27727, 27903, 28076, 28247, 28415, 28581, 28743, 28903, 29061, 29215,
    29367, 29515, 29661, 29804, 29944, 30081, 30214, 30345, 30472, 30597, 30718, 30836, 30950,
    31062, 31170, 31274, 31376, 31474, 31568, 31659, 31747, 31831, 31911, 31988, 32062, 32132,
    32198, 32261, 32320, 32376, 32428, 32476, 32521, 32561, 32599, 32632, 32662, 32688, 32711,
    32729, 32744, 32755, 32763, 32767, 32767, 32741, 32665, 32537, 32359, 32129, 31850, 31521,
    31143, 30716, 30242, 29720, 29151, 28538, 27879, 27177, 26433, 25647, 24821, 23957, 23055,
    22117, 21145, 20139, 19102, 18036, 16941, 15820, 14674, 13505, 12315, 11106, 9879, 8637, 7381,
    6114, 4838, 3554, 2264, 971,
];
const LAG_HIGH: [i16; LPC_ORDER] = [
    32728, 32619, 32438, 32187, 31867, 31480, 31029, 30517, 29946, 29321,
];
const LAG_LOW: [i16; LPC_ORDER] = [
    11904, 17280, 30720, 25856, 24192, 28992, 24384, 7360, 19520, 14784,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correlations {
    /// Split-word Q31 values with the low bit discarded, after lag windowing.
    pub values_q31: [i32; CORRELATIONS],
    pub normalization_shift: i16,
}
// TS 26.073 Autocorr and Lag_window, including overflow rescaling and split-word multiplication.
pub fn analyze(samples: &[i16; WINDOW_SAMPLES]) -> Correlations {
    let mut windowed = std::array::from_fn::<_, WINDOW_SAMPLES, _>(|i| {
        ((i32::from(samples[i]) * i32::from(WINDOW_Q15[i]) + ROUND_BIAS) >> Q15_SHIFT)
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    });
    let mut overflow_shift = 0;
    let energy = loop {
        let energy = windowed.iter().fold(0i32, |sum, &sample| {
            sum.saturating_add(product(sample, sample))
        });
        if energy < i32::MAX {
            break energy.saturating_add(1);
        }
        overflow_shift += OVERFLOW_ENERGY_SHIFT;
        for sample in &mut windowed {
            *sample >>= OVERFLOW_SAMPLE_SHIFT;
        }
    };
    let shift = energy.leading_zeros() - 1;
    let mut values_q31 = std::array::from_fn(|lag| {
        let sum = if lag == 0 {
            energy
        } else {
            windowed[..WINDOW_SAMPLES - lag]
                .iter()
                .zip(&windowed[lag..])
                .fold(0i32, |sum, (&left, &right)| {
                    sum.saturating_add(product(left, right))
                })
        };
        ((i64::from(sum) << shift).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32) & !1
    });
    for (i, value) in values_q31.iter_mut().enumerate().skip(1) {
        let (high, low) = split(*value);
        let first = multiply_q15(high, LAG_LOW[i - 1]);
        let second = multiply_q15(low, LAG_HIGH[i - 1]);
        *value = product(high, LAG_HIGH[i - 1])
            .saturating_add(product(first, 1))
            .saturating_add(product(second, 1))
            & !1;
    }
    Correlations {
        values_q31,
        normalization_shift: shift as i16 - overflow_shift,
    }
}
fn split(value: i32) -> (i16, i16) {
    let high = (value >> WORD_SHIFT) as i16;
    let low = ((value >> 1) - (i32::from(high) << Q15_SHIFT)) as i16;
    (high, low)
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn multiply_q15(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windowed_correlations_and_overflow_normalization_match_fixed_point_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..7 {
            for frame in 0..16 {
                let samples = std::array::from_fn(|i| {
                    let i = frame * WINDOW_SAMPLES + i;
                    match pattern {
                        0 => 0,
                        1 => {
                            if i == 0 {
                                30000
                            } else {
                                0
                            }
                        }
                        2 => {
                            if i.is_multiple_of(2) {
                                i16::MAX
                            } else {
                                i16::MIN
                            }
                        }
                        3 => i16::MAX,
                        4 => i16::MIN,
                        5 => ((i * 347 + 997) % 65536) as i32 as i16,
                        _ => ((i * 73) % 1024) as i16 - 512,
                    }
                });
                let correlations = analyze(&samples);
                for value in correlations.values_q31 {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
                for byte in correlations.normalization_shift.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        assert_eq!(hash, 0xc646625724d3c2f9);
    }
}
