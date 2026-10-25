use super::{lpc::COEFFICIENTS, spectral::LPC_ORDER};
const HALF_ORDER: usize = LPC_ORDER / 2;
const POLYNOMIAL_ONE_Q10: i16 = 1024;
const POLYNOMIAL_SCALE: i16 = 8192;
const HALF_POLYNOMIAL_SCALE: i16 = 4096;
const BISECTIONS: usize = 4;
const WORD_SHIFT: u32 = 16;
const Q15_SHIFT: u32 = 15;
const CHEBYSHEV_OUTPUT_SHIFT: u32 = 6;
const INTERPOLATION_SHIFT: u32 = 11;
const RECIPROCAL_SHIFT: u32 = 20;
const RECIPROCAL_NUMERATOR: i32 = 16383;
const GRID: [i16; 61] = [
    32760, 32723, 32588, 32364, 32051, 31651, 31164, 30591, 29935, 29196, 28377, 27481, 26509,
    25465, 24351, 23170, 21926, 20621, 19260, 17846, 16384, 14876, 13327, 11743, 10125, 8480, 6812,
    5126, 3425, 1714, 0, -1714, -3425, -5126, -6812, -8480, -10125, -11743, -13327, -14876, -16384,
    -17846, -19260, -20621, -21926, -23170, -24351, -25465, -26509, -27481, -28377, -29196, -29935,
    -30591, -31164, -31651, -32051, -32364, -32588, -32723, -32760,
];

// TS 26.073 Az_lsp alternates the sum and difference polynomial roots and retains all prior roots on failure.
pub fn from_predictor(
    coefficients: &[i16; COEFFICIENTS],
    previous: &[i16; LPC_ORDER],
) -> [i16; LPC_ORDER] {
    let mut polynomials = [[0i16; HALF_ORDER + 1]; 2];
    polynomials[0][0] = POLYNOMIAL_ONE_Q10;
    polynomials[1][0] = POLYNOMIAL_ONE_Q10;
    for i in 0..HALF_ORDER {
        let first = product(coefficients[i + 1], POLYNOMIAL_SCALE);
        let last = product(coefficients[LPC_ORDER - i], POLYNOMIAL_SCALE);
        let sum = (first.saturating_add(last) >> WORD_SHIFT) as i16;
        let difference = (first.saturating_sub(last) >> WORD_SHIFT) as i16;
        polynomials[0][i + 1] = sum.saturating_sub(polynomials[0][i]);
        polynomials[1][i + 1] = difference.saturating_add(polynomials[1][i]);
    }
    let mut roots = [0; LPC_ORDER];
    let mut found = 0;
    let mut polynomial = 0;
    let mut low = GRID[0];
    let mut low_value = chebyshev(low, &polynomials[polynomial]);
    for &point in &GRID[1..] {
        if found == LPC_ORDER {
            break;
        }
        let mut high = low;
        let mut high_value = low_value;
        low = point;
        low_value = chebyshev(low, &polynomials[polynomial]);
        if product(low_value, high_value) > 0 {
            continue;
        }
        for _ in 0..BISECTIONS {
            let middle = (low >> 1).saturating_add(high >> 1);
            let middle_value = chebyshev(middle, &polynomials[polynomial]);
            if product(low_value, middle_value) <= 0 {
                high = middle;
                high_value = middle_value;
            } else {
                low = middle;
                low_value = middle_value;
            }
        }
        let span = high.saturating_sub(low);
        let value_span = high_value.saturating_sub(low_value);
        let root = if value_span == 0 {
            low
        } else {
            let absolute = value_span.saturating_abs();
            let exponent = absolute.leading_zeros() - 1;
            let normalized = (i32::from(absolute) << exponent) as i16;
            let reciprocal = ((RECIPROCAL_NUMERATOR << Q15_SHIFT) / i32::from(normalized)) as i16;
            let mut ratio = (product(span, reciprocal) >> (RECIPROCAL_SHIFT - exponent)) as i16;
            if value_span < 0 {
                ratio = ratio.saturating_neg();
            }
            low.saturating_sub((product(low_value, ratio) >> INTERPOLATION_SHIFT) as i16)
        };
        roots[found] = root;
        found += 1;
        low = root;
        polynomial ^= 1;
        low_value = chebyshev(low, &polynomials[polynomial]);
    }
    if found == LPC_ORDER { roots } else { *previous }
}
fn chebyshev(x: i16, polynomial: &[i16; HALF_ORDER + 1]) -> i16 {
    const CHEBYSHEV_ONE: i32 = 256 << WORD_SHIFT;
    const DOUBLE_X_SCALE: i16 = 512;
    let mut previous = CHEBYSHEV_ONE;
    let mut current =
        product(x, DOUBLE_X_SCALE).saturating_add(product(polynomial[1], POLYNOMIAL_SCALE)) & !1;
    for &coefficient in &polynomial[2..HALF_ORDER] {
        let next = subtract_split(shift(multiply_word(current, x), 1), previous)
            .saturating_add(product(coefficient, POLYNOMIAL_SCALE));
        previous = current;
        current = next & !1;
    }
    let value = subtract_split(multiply_word(current, x), previous)
        .saturating_add(product(polynomial[HALF_ORDER], HALF_POLYNOMIAL_SCALE));
    (shift(value, CHEBYSHEV_OUTPUT_SHIFT) >> WORD_SHIFT) as i16
}
fn split(value: i32) -> (i16, i16) {
    let high = (value >> WORD_SHIFT) as i16;
    (high, ((value >> 1) - (i32::from(high) << Q15_SHIFT)) as i16)
}
fn subtract_split(value: i32, previous: i32) -> i32 {
    let (high, low) = split(previous);
    value
        .saturating_add(product(high, i16::MIN))
        .saturating_sub(product(low, 1))
}
fn multiply_word(value: i32, coefficient: i16) -> i32 {
    let (high, low) = split(value);
    let fraction = ((i32::from(low) * i32::from(coefficient)) >> Q15_SHIFT) as i16;
    product(high, coefficient).saturating_add(product(fraction, 1))
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn shift(value: i32, bits: u32) -> i32 {
    (i64::from(value) << bits).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::{autocorrelation::WINDOW_SAMPLES, levinson::Levinson};
    #[test]
    fn roots_and_previous_root_recovery_match_fixed_point_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..7 {
            let mut analyzer = Levinson::default();
            let mut previous = [
                30000, 26000, 21000, 15000, 8000, 0, -8000, -15000, -21000, -26000,
            ];
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
                        5 => ((i * 347 + 997) % 65536) as i16,
                        _ => ((i * 73) % 1024) as i16 - 512,
                    }
                });
                let predictor = analyzer.analyze(&samples);
                previous = from_predictor(&predictor.coefficients_q12, &previous);
                for value in previous {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
        assert_eq!(hash, 0x8210e7487c4ff72a);
    }
}
