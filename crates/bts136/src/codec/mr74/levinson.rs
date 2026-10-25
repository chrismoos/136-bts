use super::{
    autocorrelation::{WINDOW_SAMPLES, analyze},
    lpc::COEFFICIENTS,
    spectral::LPC_ORDER,
};

const REFLECTIONS: usize = 4;
const LPC_ONE_Q12: i16 = 4096;
const LPC_PRECISION_SHIFT: u32 = 4;
const UNSTABLE_REFLECTION: i16 = 32750;
const WORD_SHIFT: u32 = 16;
const Q15_SHIFT: u32 = 15;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
const RECIPROCAL_NUMERATOR: i32 = 0x3fff;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predictor {
    pub coefficients_q12: [i16; COEFFICIENTS],
    pub reflections_q15: [i16; REFLECTIONS],
    pub reused_previous: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Levinson {
    previous: [i16; COEFFICIENTS],
}
impl Default for Levinson {
    fn default() -> Self {
        let mut previous = [0; COEFFICIENTS];
        previous[0] = LPC_ONE_Q12;
        Self { previous }
    }
}
impl Levinson {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 Autocorr, Lag_window and Levinson, with prior-filter substitution for unstable recursion.
    pub fn analyze(&mut self, samples: &[i16; WINDOW_SAMPLES]) -> Predictor {
        let correlation = analyze(samples).values_q31;
        let mut coefficients = [0i32; COEFFICIENTS];
        let mut reflections = [0i16; REFLECTIONS];
        let mut reflection = divide(correlation[1].saturating_abs(), correlation[0]);
        if correlation[1] > 0 {
            reflection = reflection.saturating_neg();
        }
        reflections[0] = rounded(reflection);
        coefficients[1] = (reflection >> LPC_PRECISION_SHIFT) & !1;
        let complement =
            i32::MAX.saturating_sub(multiply(reflection, reflection).saturating_abs()) & !1;
        let initial_energy = multiply(correlation[0], complement);
        let mut exponent = normalize(initial_energy);
        let mut energy = shift(initial_energy, exponent) & !1;
        for degree in 2..=LPC_ORDER {
            let mut sum = 0i32;
            for j in 1..degree {
                sum = sum.saturating_add(multiply(correlation[j], coefficients[degree - j]));
            }
            sum = shift(sum, LPC_PRECISION_SHIFT).saturating_add(correlation[degree]);
            let mut next = divide(sum.saturating_abs(), energy);
            if sum > 0 {
                next = next.saturating_neg();
            }
            next = shift(next, exponent);
            let (high, _) = split(next);
            if degree <= REFLECTIONS {
                reflections[degree - 1] = rounded(next);
            }
            if high.saturating_abs() > UNSTABLE_REFLECTION {
                return Predictor {
                    coefficients_q12: self.previous,
                    reflections_q15: [0; REFLECTIONS],
                    reused_previous: true,
                };
            }
            reflection = next & !1;
            let mut updated = coefficients;
            for j in 1..degree {
                updated[j] = multiply(reflection, coefficients[degree - j])
                    .saturating_add(coefficients[j])
                    & !1;
            }
            updated[degree] = (next >> LPC_PRECISION_SHIFT) & !1;
            let complement =
                i32::MAX.saturating_sub(multiply(reflection, reflection).saturating_abs()) & !1;
            let reduced = multiply(energy, complement);
            let extra = normalize(reduced);
            energy = shift(reduced, extra) & !1;
            exponent += extra;
            coefficients = updated;
        }
        let mut output = [0; COEFFICIENTS];
        output[0] = LPC_ONE_Q12;
        for i in 1..COEFFICIENTS {
            output[i] = rounded(shift(coefficients[i], 1));
        }
        self.previous = output;
        Predictor {
            coefficients_q12: output,
            reflections_q15: reflections,
            reused_previous: false,
        }
    }
}
fn rounded(value: i32) -> i16 {
    (value.saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16
}
fn split(value: i32) -> (i16, i16) {
    let high = (value >> WORD_SHIFT) as i16;
    (high, ((value >> 1) - (i32::from(high) << Q15_SHIFT)) as i16)
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn fractional(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
fn multiply(left: i32, right: i32) -> i32 {
    let (lh, ll) = split(left);
    let (rh, rl) = split(right);
    product(lh, rh)
        .saturating_add(product(fractional(lh, rl), 1))
        .saturating_add(product(fractional(ll, rh), 1))
}
fn multiply_word(left: i32, right: i16) -> i32 {
    let (high, low) = split(left);
    product(high, right).saturating_add(product(fractional(low, right), 1))
}
// TS 26.073 Div_32 uses one reciprocal refinement rather than integer Q31 division.
fn divide(numerator: i32, denominator: i32) -> i32 {
    let (high, _) = split(denominator);
    let approximation =
        ((RECIPROCAL_NUMERATOR << Q15_SHIFT) / i32::from(high)).min(i32::from(i16::MAX)) as i16;
    let error = i32::MAX.saturating_sub(multiply_word(denominator, approximation));
    let inverse = multiply_word(error & !1, approximation);
    shift(multiply(numerator & !1, inverse & !1), 2)
}
fn normalize(value: i32) -> u32 {
    match value {
        0 => 0,
        -1 => 31,
        value if value < 0 => (!value).leading_zeros() - 1,
        value => value.leading_zeros() - 1,
    }
}
fn shift(value: i32, bits: u32) -> i32 {
    (i64::from(value) << bits.min(31)).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn analysis_and_predictor_history_match_fixed_point_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..7 {
            let mut analyzer = Levinson::default();
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
                let output = analyzer.analyze(&samples);
                for value in output
                    .coefficients_q12
                    .into_iter()
                    .chain(output.reflections_q15)
                    .chain(analyzer.previous)
                {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            analyzer.reset();
            assert_eq!(analyzer, Levinson::default());
        }
        assert_eq!(hash, 0xd0bfaee0745b68a1);
    }
}
