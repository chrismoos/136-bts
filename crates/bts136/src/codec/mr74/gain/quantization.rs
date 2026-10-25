use super::{GainDecoder, Gains, TABLE};
use crate::codec::mr74::{SUBFRAME_SAMPLES, logarithm};
const WORD_SHIFT: u32 = 16;
const Q15_SHIFT: u32 = 15;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
const MAX_PITCH_GAIN_Q14: i16 = 19661;
const RESONANCE_LIMIT_Q14: i16 = 15565;
const INNOVATION_SHIFT: u32 = 3;
const GAIN_TERMS: usize = 5;
const CODE_EXPONENT_BIAS: i16 = 11;
const CORRELATION_EXPONENT: i16 = 15;
const MIN_POSITIVE_CORRELATION: i16 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainLimit {
    Unrestricted,
    Resonance,
}
impl GainLimit {
    pub fn pitch_q14(self) -> i16 {
        match self {
            Self::Unrestricted => i16::MAX,
            Self::Resonance => RESONANCE_LIMIT_Q14,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PitchGain {
    pub gain_q14: i16,
    correlations: [i16; 4],
}
/// TS 26.073 G_pitch retains normalized correlations and repeats overflowing sums with a quarter-scale adaptive vector.
pub fn pitch_gain(
    target: &[i16; SUBFRAME_SAMPLES],
    adaptive: &[i16; SUBFRAME_SAMPLES],
) -> PitchGain {
    let scaled = adaptive.map(|v| v >> 2);
    let (mut energy, overflow) = dot(adaptive, adaptive);
    let mut exponent_yy = normalize(energy);
    if overflow {
        energy = dot(&scaled, &scaled).0;
        exponent_yy = normalize(energy);
    }
    let yy = rounded(shift(energy, exponent_yy));
    if overflow {
        exponent_yy -= 4;
    }
    let (mut correlation, overflow) = dot(target, adaptive);
    let mut exponent_xy = normalize(correlation);
    if overflow {
        correlation = dot(target, &scaled).0;
        exponent_xy = normalize(correlation);
    }
    let xy = rounded(shift(correlation, exponent_xy));
    if overflow {
        exponent_xy -= 2;
    }
    let gain = if xy < MIN_POSITIVE_CORRELATION {
        0
    } else {
        let ratio = ((i32::from(xy >> 1) << Q15_SHIFT) / i32::from(yy)) as i16;
        let exponent = exponent_yy - exponent_xy;
        shift_word(ratio, exponent).min(MAX_PITCH_GAIN_Q14)
    };
    PitchGain {
        gain_q14: gain,
        correlations: [
            yy,
            CORRELATION_EXPONENT - exponent_yy,
            xy,
            CORRELATION_EXPONENT - exponent_xy,
        ],
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuantizedGain {
    pub index: u16,
    pub gains: Gains,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quantizer {
    decoder: GainDecoder,
}
impl Quantizer {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 calc_filt_energies and Qua_gain for MR74. Innovation includes the pitch sharpening used by gain prediction.
    pub fn quantize(
        &mut self,
        target: &[i16; SUBFRAME_SAMPLES],
        adaptive: &[i16; SUBFRAME_SAMPLES],
        filtered_innovation_q12: &[i16; SUBFRAME_SAMPLES],
        innovation_q13: &[i16; SUBFRAME_SAMPLES],
        limit: GainLimit,
    ) -> QuantizedGain {
        let correlations = pitch_gain(target, adaptive).correlations;
        let (fractions, exponents) =
            energy_coefficients(target, adaptive, filtered_innovation_q12, correlations);
        let prediction = self.decoder.predict(innovation_q13);
        let base = logarithm::pow2(14, prediction.fraction)
            .expect("predicted logarithm has a Q15 fraction") as i16;
        let code_exponent = prediction.exponent - CODE_EXPONENT_BIAS;
        let scales = [
            exponents[0] - 13,
            exponents[1] - 14,
            exponents[2] + 15 + code_exponent * 2,
            exponents[3] + code_exponent,
            exponents[4] + 1 + code_exponent,
        ];
        let maximum = *scales.iter().max().expect("five gain terms") + 1;
        let coefficients = std::array::from_fn::<_, GAIN_TERMS, _>(|i| {
            shift(i32::from(fractions[i]) << WORD_SHIFT, scales[i] - maximum) & !1
        });
        let mut minimum = i32::MAX;
        let mut index = 0;
        for (i, &[pitch, correction, _]) in TABLE.iter().enumerate() {
            if pitch > limit.pitch_q14() {
                continue;
            }
            let code = fractional(correction, base);
            let terms = [
                fractional(pitch, pitch),
                pitch,
                fractional(code, code),
                code,
                fractional(code, pitch),
            ];
            let distance = coefficients
                .iter()
                .zip(terms)
                .fold(0i32, |sum, (&coefficient, term)| {
                    sum.saturating_add(multiply_word(coefficient, term))
                });
            if distance < minimum {
                minimum = distance;
                index = i as u16;
            }
        }
        let gains = self
            .decoder
            .decode(index, innovation_q13)
            .expect("selected gain table entry is valid");
        QuantizedGain { index, gains }
    }
}
fn energy_coefficients(
    target: &[i16; SUBFRAME_SAMPLES],
    adaptive: &[i16; SUBFRAME_SAMPLES],
    innovation: &[i16; SUBFRAME_SAMPLES],
    correlations: [i16; 4],
) -> ([i16; GAIN_TERMS], [i16; GAIN_TERMS]) {
    let scaled = innovation.map(|v| v >> INNOVATION_SHIFT);
    let mut fractions = [correlations[0], correlations[2].saturating_neg(), 0, 0, 0];
    let mut exponents = [correlations[1], correlations[3] + 1, 0, 0, 0];
    for (i, (left, right, bias)) in [
        (&scaled, &scaled, -3),
        (target, &scaled, 7),
        (adaptive, &scaled, 7),
    ]
    .into_iter()
    .enumerate()
    {
        let sum = dot(left, right).0;
        let exponent = normalize(sum);
        let fraction = (shift(sum, exponent) >> WORD_SHIFT) as i16;
        fractions[i + 2] = if i == 1 {
            fraction.saturating_neg()
        } else {
            fraction
        };
        exponents[i + 2] = bias - exponent;
    }
    (fractions, exponents)
}
fn dot(left: &[i16; SUBFRAME_SAMPLES], right: &[i16; SUBFRAME_SAMPLES]) -> (i32, bool) {
    let mut sum = 1i32;
    let mut overflow = false;
    for (&left, &right) in left.iter().zip(right) {
        let raw = i64::from(left) * i64::from(right) * 2;
        let product = raw.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        overflow |= raw != product;
        let raw = i64::from(sum) + product;
        let clamped = raw.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        overflow |= raw != clamped;
        sum = clamped as i32;
    }
    (sum, overflow)
}
fn normalize(value: i32) -> i16 {
    match value {
        0 => 0,
        -1 => 31,
        v if v < 0 => (!v).leading_zeros() as i16 - 1,
        v => v.leading_zeros() as i16 - 1,
    }
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
fn shift(value: i32, exponent: i16) -> i32 {
    if exponent < 0 {
        value >> exponent.unsigned_abs().min(31)
    } else {
        (i64::from(value) << exponent.min(31)).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32
    }
}
fn shift_word(value: i16, exponent: i16) -> i16 {
    if exponent < 0 {
        value >> exponent.unsigned_abs().min(15)
    } else {
        ((i32::from(value)) << exponent.min(15)).clamp(i32::from(i16::MIN), i32::from(i16::MAX))
            as i16
    }
}
fn multiply_word(value: i32, coefficient: i16) -> i32 {
    let high = (value >> WORD_SHIFT) as i16;
    let low = ((value >> 1) - (i32::from(high) << Q15_SHIFT)) as i16;
    product(high, coefficient).saturating_add(product(fractional(low, coefficient), 1))
}
#[cfg(test)]
mod tests;
