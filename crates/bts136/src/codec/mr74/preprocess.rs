const FEEDFORWARD: [i16; 3] = [1899, -3798, 1899];
const FEEDBACK: [i16; 2] = [7807, -3733];
const FILTER_SCALE_SHIFT: u32 = 3;
const WORD_SHIFT: u32 = 16;
const Q15_SHIFT: u32 = 15;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preprocess {
    previous_input: [i16; 2],
    previous_output: [i32; 2],
}
impl Preprocess {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 Pre_Process applies the 80 Hz high-pass filter and halves input amplitude.
    pub fn process(&mut self, samples: &mut [i16]) {
        for sample in samples {
            let mut sum = split_product(self.previous_output[0], FEEDBACK[0])
                .saturating_add(split_product(self.previous_output[1], FEEDBACK[1]));
            sum = sum.saturating_add(doubled_product(*sample, FEEDFORWARD[0]));
            sum = sum.saturating_add(doubled_product(self.previous_input[0], FEEDFORWARD[1]));
            sum = sum.saturating_add(doubled_product(self.previous_input[1], FEEDFORWARD[2]));
            let filtered = (i64::from(sum) << FILTER_SCALE_SHIFT)
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            self.previous_input = [*sample, self.previous_input[0]];
            // L_Extract discards the low bit of internal output before the next split-word multiply.
            self.previous_output = [filtered & !1, self.previous_output[0]];
            *sample = (filtered.saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16;
        }
    }
}
fn split(value: i32) -> (i16, i16) {
    let high = (value >> WORD_SHIFT) as i16;
    let low = ((value >> 1) - (i32::from(high) << Q15_SHIFT)) as i16;
    (high, low)
}
fn split_product(value: i32, coefficient: i16) -> i32 {
    let (high, low) = split(value);
    let fraction = ((i32::from(low) * i32::from(coefficient)) >> Q15_SHIFT) as i16;
    doubled_product(high, coefficient).saturating_add(doubled_product(fraction, 1))
}
fn doubled_product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::decoder::FRAME_SAMPLES;
    fn sample(pattern: usize, index: usize) -> i16 {
        match pattern {
            0 => 0,
            1 => {
                if index == 0 {
                    30000
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
            5 => (((index * 347 + 997) % 65536) as i32 - 32768) as i16,
            _ => (((index * 73) % 1024) as i32 - 512) as i16,
        }
    }
    #[test]
    fn high_pass_samples_and_split_word_history_match_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..7 {
            let mut filter = Preprocess::default();
            for frame in 0..16 {
                let mut samples: [i16; FRAME_SAMPLES] =
                    std::array::from_fn(|index| sample(pattern, frame * FRAME_SAMPLES + index));
                filter.process(&mut samples);
                let first = split(filter.previous_output[0]);
                let second = split(filter.previous_output[1]);
                for value in samples.into_iter().chain([
                    first.0,
                    first.1,
                    second.0,
                    second.1,
                    filter.previous_input[0],
                    filter.previous_input[1],
                ]) {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
        assert_eq!(hash, 0x9fe98cc5d352ae1a);
    }
    #[test]
    fn partitioned_processing_and_reset_preserve_exact_filter_state() {
        let source: [i16; FRAME_SAMPLES] = std::array::from_fn(|index| sample(5, index));
        let mut expected = source;
        let mut bulk = Preprocess::default();
        bulk.process(&mut expected);
        for length in [1, 3, 7, 40, 97] {
            let mut actual = source;
            let mut filter = Preprocess::default();
            for chunk in actual.chunks_mut(length) {
                filter.process(chunk);
            }
            assert_eq!(actual, expected);
            assert_eq!(filter, bulk);
            filter.process(&mut []);
            assert_eq!(filter, bulk);
            filter.reset();
            assert_eq!(filter, Preprocess::default());
        }
    }
}
