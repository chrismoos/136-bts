use super::{logarithm, spectral::LPC_ORDER};
const COEFFICIENT_UNITY_Q12: i16 = 4096;
const REFLECTION_SCALE_SHIFT: u32 = 3;
const Q15_SHIFT: u32 = 15;
const WORD_SHIFT: u32 = 16;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
const DIVISION_NUMERATOR: i32 = 16384;
const LOG_GAIN_SHIFT: u32 = 12;
const LOG_FRACTION_SHIFT: u32 = Q15_SHIFT - LOG_GAIN_SHIFT;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredictionGain {
    pub reflection_q15: [i16; LPC_ORDER],
    pub error_q15: i16,
    pub log_gain_q12: i16,
}
/// TS 26.073 A_Refl and dtx_dec compute prediction gain, replacing unstable reflection recursion with zeros.
pub fn prediction_gain(coefficients_q12: &[i16; LPC_ORDER]) -> PredictionGain {
    let reflection_q15 = reflection(coefficients_q12);
    let error_q15 = reflection_q15.iter().fold(i16::MAX, |error, &coefficient| {
        multiply_q15(
            error,
            i16::MAX.saturating_sub(multiply_q15(coefficient, coefficient)),
        )
    });
    let log = logarithm::log2(i32::from(error_q15));
    let integral = ((i32::from(log.exponent) - Q15_SHIFT as i32) << LOG_GAIN_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    let combined = integral.saturating_add((log.fraction_q15 >> LOG_FRACTION_SHIFT) as i16);
    PredictionGain {
        reflection_q15,
        error_q15,
        log_gain_q12: combined.saturating_neg() >> 1,
    }
}
fn reflection(coefficients: &[i16; LPC_ORDER]) -> [i16; LPC_ORDER] {
    let mut state = *coefficients;
    let mut output = [0i16; LPC_ORDER];
    for order in (0..LPC_ORDER).rev() {
        if state[order].saturating_abs() >= COEFFICIENT_UNITY_Q12 {
            return [0; LPC_ORDER];
        }
        output[order] = state[order] << REFLECTION_SCALE_SHIFT;
        let error = i32::MAX.saturating_sub(product(output[order], output[order]));
        let normalization = error.leading_zeros() - 1;
        let scale = Q15_SHIFT - normalization;
        let normalized = rounded(error << normalization);
        let multiplier = ((DIVISION_NUMERATOR << Q15_SHIFT) / i32::from(normalized))
            .min(i32::from(i16::MAX)) as i16;
        let mut next = [0i16; LPC_ORDER];
        for index in 0..order {
            let difference = (i32::from(state[index]) << WORD_SHIFT)
                .saturating_sub(product(output[order], state[order - index - 1]));
            let value = product(multiplier, rounded(difference));
            let scaled = (value >> scale) + i32::from(value & (1 << (scale - 1)) != 0);
            if scaled.saturating_abs() > i32::from(i16::MAX) {
                return [0; LPC_ORDER];
            }
            next[index] = scaled as i16;
        }
        state = next;
    }
    output
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn multiply_q15(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
fn rounded(value: i32) -> i16 {
    (value.saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::{LSF_INDICES, PARAMETER_COUNT, Parameters, lpc::LpcDecoder};
    #[test]
    fn reflection_recursion_error_and_prediction_gain_match_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        let mut include = |prediction: PredictionGain| {
            for value in prediction
                .reflection_q15
                .into_iter()
                .chain([prediction.error_q15, prediction.log_gain_q12])
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        };
        let mut decoder = LpcDecoder::default();
        for step in 0..4096 {
            let mut values = [0; PARAMETER_COUNT];
            values[..LSF_INDICES].copy_from_slice(&[
                (step % 256) as u16,
                ((step * 73 + step / 512) % 512) as u16,
                ((step * 197 + step / 256) % 512) as u16,
            ]);
            let frame = decoder.decode(&Parameters::try_new(values).unwrap());
            let coefficients = frame.coefficients_q12[3][1..].try_into().unwrap();
            include(prediction_gain(coefficients));
        }
        let mut seed = 1u32;
        for vector in 0..8192 {
            let coefficients = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                if vector % 2 == 1 {
                    seed as i16
                } else {
                    ((seed % 8191) as i32 - 4095) as i16
                }
            });
            include(prediction_gain(&coefficients));
        }
        assert_eq!(hash, 0xd2494a1fd07d35ff);
    }
    #[test]
    fn unstable_coefficients_reset_all_reflection_values() {
        for coefficient in [
            i16::MIN,
            -COEFFICIENT_UNITY_Q12,
            COEFFICIENT_UNITY_Q12,
            i16::MAX,
        ] {
            let mut coefficients = [0; LPC_ORDER];
            coefficients[LPC_ORDER - 1] = coefficient;
            let prediction = prediction_gain(&coefficients);
            assert_eq!(prediction.reflection_q15, [0; LPC_ORDER]);
            assert_eq!(prediction.error_q15, 32757);
            assert_eq!(prediction.log_gain_q12, 1);
        }
    }
}
