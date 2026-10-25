use super::SUBFRAME_SAMPLES;
const WORD_SHIFT: u32 = 16;
const Q15_SHIFT: u32 = 15;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
const ENERGY_SAMPLE_SHIFT: u32 = 2;
const ENERGY_RESULT_SHIFT: u32 = 4;
const RATIO_SCALE_SHIFT: i16 = 7;
const GAIN_SCALE_SHIFT: i16 = 9;
const OUTPUT_SCALE_SHIFT: i16 = 3;
const INVERSE_SCALE_EXPONENT: u32 = 30;
const INVERSE_ZERO: i32 = 0x3fffffff;
const INVERSE_INDEX_SHIFT: u32 = 25;
const INVERSE_FRACTION_SHIFT: u32 = 10;
const INVERSE_TABLE_BASE: usize = 16;
const FRACTION_MASK: i32 = (1 << Q15_SHIFT) - 1;
// TS 26.073 Inv_sqrt uses this Q15 interpolation table for normalized energy ratios.
const INVERSE_TABLE: [i32; 49] = [
    32767, 31790, 30894, 30070, 29309, 28602, 27945, 27330, 26755, 26214, 25705, 25225, 24770,
    24339, 23930, 23541, 23170, 22817, 22479, 22155, 21845, 21548, 21263, 20988, 20724, 20470,
    20225, 19988, 19760, 19539, 19326, 19119, 18919, 18725, 18536, 18354, 18176, 18004, 17837,
    17674, 17515, 17361, 17211, 17064, 16921, 16782, 16646, 16514, 16384,
];
/// TS 26.073 agc2 matches enhanced excitation energy before synthesis, with no state between subframes.
pub fn match_energy(input_q0: &[i16; SUBFRAME_SAMPLES], output_q0: &mut [i16; SUBFRAME_SAMPLES]) {
    let output_energy = energy(output_q0);
    if output_energy == 0 {
        return;
    }
    let gain = energy_gain(input_q0, output_energy);
    for sample in output_q0 {
        *sample = (shift(doubled_product(*sample, gain), OUTPUT_SCALE_SHIFT) >> WORD_SHIFT) as i16;
    }
}
const SMOOTHING_Q15: i16 = 29491;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GainControl {
    previous_gain: i16,
}
impl Default for GainControl {
    fn default() -> Self {
        Self {
            previous_gain: 4096,
        }
    }
}
impl GainControl {
    pub(super) fn process(
        &mut self,
        input: &[i16; SUBFRAME_SAMPLES],
        output: &mut [i16; SUBFRAME_SAMPLES],
    ) {
        let output_energy = energy(output);
        if output_energy == 0 {
            self.previous_gain = 0;
            return;
        }
        let target = multiply_q15(energy_gain(input, output_energy), i16::MAX - SMOOTHING_Q15);
        for sample in output {
            self.previous_gain =
                multiply_q15(self.previous_gain, SMOOTHING_Q15).saturating_add(target);
            *sample = (shift(
                doubled_product(*sample, self.previous_gain),
                OUTPUT_SCALE_SHIFT,
            ) >> WORD_SHIFT) as i16;
        }
    }
}
fn multiply_q15(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
fn energy_gain(input_q0: &[i16; SUBFRAME_SAMPLES], output_energy: i32) -> i16 {
    let mut exponent = normalization(output_energy) as i16 - 1;
    let gain_out = rounded(shift(output_energy, exponent));
    let input_energy = energy(input_q0);
    if input_energy == 0 {
        0
    } else {
        let input_exponent = normalization(input_energy) as i16;
        let gain_in = rounded(shift(input_energy, input_exponent));
        exponent -= input_exponent;
        let ratio =
            ((i32::from(gain_out) << Q15_SHIFT) / i32::from(gain_in)).min(i32::from(i16::MAX));
        let scaled_ratio = shift(shift(ratio, RATIO_SCALE_SHIFT), -exponent);
        rounded(shift(inverse_sqrt(scaled_ratio), GAIN_SCALE_SHIFT))
    }
}
fn energy(samples: &[i16; SUBFRAME_SAMPLES]) -> i32 {
    let sum = samples.iter().fold(0i32, |sum, &sample| {
        sum.saturating_add(doubled_product(sample, sample))
    });
    if sum == i32::MAX {
        samples.iter().fold(0i32, |sum, &sample| {
            let sample = sample >> ENERGY_SAMPLE_SHIFT;
            sum.saturating_add(doubled_product(sample, sample))
        })
    } else {
        sum >> ENERGY_RESULT_SHIFT
    }
}
fn normalization(value: i32) -> u32 {
    if value == 0 {
        0
    } else {
        value.leading_zeros() - 1
    }
}
fn doubled_product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn rounded(value: i32) -> i16 {
    (value.saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16
}
fn shift(value: i32, shift: i16) -> i32 {
    if shift <= 0 {
        return value >> u32::from(shift.unsigned_abs()).min(i32::BITS - 1);
    }
    (i64::from(value) << (shift as u32).min(i32::BITS))
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}
pub(super) fn inverse_sqrt(value: i32) -> i32 {
    if value <= 0 {
        return INVERSE_ZERO;
    }
    let normalization = normalization(value);
    let mut normalized = value << normalization;
    let exponent = INVERSE_SCALE_EXPONENT - normalization;
    if exponent.is_multiple_of(2) {
        normalized >>= 1;
    }
    let index = (normalized >> INVERSE_INDEX_SHIFT) as usize - INVERSE_TABLE_BASE;
    let fraction = (normalized >> INVERSE_FRACTION_SHIFT) & FRACTION_MASK;
    let interpolated = (INVERSE_TABLE[index] << WORD_SHIFT)
        - 2 * (INVERSE_TABLE[index] - INVERSE_TABLE[index + 1]) * fraction;
    interpolated >> (exponent / 2 + 1)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn include(hash: &mut u64, bytes: impl IntoIterator<Item = u8>) {
        for byte in bytes {
            *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    }
    #[test]
    fn inverse_square_root_matches_reference_normalized_domain() {
        let mut hash = 0xcbf29ce484222325;
        for exponent in 0..=INVERSE_SCALE_EXPONENT {
            for fraction in 0..=FRACTION_MASK {
                let value = (1u64 << exponent) + (((fraction as u64) << exponent) >> Q15_SHIFT);
                include(&mut hash, inverse_sqrt(value as i32).to_le_bytes());
            }
        }
        for value in [0, -1, i32::MIN, i32::MAX] {
            include(&mut hash, inverse_sqrt(value).to_le_bytes());
        }
        assert_eq!(hash, 0x523aecfe92d4b85b);
    }
    fn sample(pattern: usize, index: usize, step: usize) -> i16 {
        match pattern {
            0 => 0,
            1 => {
                if index == step % SUBFRAME_SAMPLES {
                    8191
                } else {
                    0
                }
            }
            2 => {
                if index.is_multiple_of(2) {
                    i16::MAX
                } else {
                    i16::MIN
                }
            }
            3 => i16::MAX,
            4 => i16::MIN,
            5 => (((index * 347 + step * 997) % 65536) as i32 - 32768) as i16,
            _ => (((index * 73 + step * 197) % 1024) as i32 - 512) as i16,
        }
    }
    #[test]
    fn excitation_energy_matching_matches_reference_for_zero_sparse_and_saturated_inputs() {
        let mut hash = 0xcbf29ce484222325;
        for input in 0..7 {
            for output in 0..7 {
                for step in 0..32 {
                    let input = std::array::from_fn(|index| sample(input, index, step));
                    let mut output = std::array::from_fn(|index| sample(output, index, step));
                    match_energy(&input, &mut output);
                    for value in output {
                        include(&mut hash, value.to_le_bytes());
                    }
                }
            }
        }
        assert_eq!(hash, 0xb7194fbec625e335);
    }
}
