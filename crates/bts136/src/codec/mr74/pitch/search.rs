use super::{MAX_SEARCH_LAG_SAMPLES, MIN_SEARCH_LAG_SAMPLES, PitchLag};
use crate::codec::mr74::{SUBFRAME_SAMPLES, adaptive::HISTORY_SAMPLES, energy::inverse_sqrt};
use std::{error::Error, fmt};
const INTERPOLATION_SAMPLES: usize = 4;
const PHASES: usize = 6;
const MAX_FRACTIONAL_LAG: usize = 84;
const ABSOLUTE_RADIUS: usize = 3;
const ABSOLUTE_SPAN: usize = 6;
const RELATIVE_RADIUS: usize = 5;
const RELATIVE_SPAN: usize = 9;
const FRACTION_DENOMINATOR: i16 = 3;
const ABSOLUTE_INDEX_BIAS: i16 = 58;
const INTEGER_INDEX_BIAS: i16 = 112;
const RELATIVE_INDEX_BIAS: i16 = 2;
const INTEGER_ENCODING_LIMIT: usize = 85;
const CORRELATION_SAMPLES: usize = MAX_SEARCH_LAG_SAMPLES as usize + INTERPOLATION_SAMPLES + 1;
const WORD_SHIFT: u32 = 16;
const Q15_SHIFT: u32 = 15;
const FILTER_SHIFT: u32 = 3;
const SCALED_FILTER_SHIFT: u32 = 1;
const ENERGY_LIMIT: i32 = 1 << 26;
const SAMPLE_SCALING: u32 = 2;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
// TS 26.073 Interpol_3or6 uses this search filter, distinct from adaptive prediction interpolation.
const INTERPOLATOR: [i16; PHASES * INTERPOLATION_SAMPLES + 1] = [
    29519, 28316, 24906, 19838, 13896, 7945, 2755, -1127, -3459, -4304, -3969, -2899, -1561, -336,
    534, 970, 1023, 823, 516, 220, 0, -131, -194, -215, 0,
];
pub const EXCITATION_SAMPLES: usize = HISTORY_SAMPLES + SUBFRAME_SAMPLES;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subframe {
    One,
    Two,
    Three,
    Four,
}
impl Subframe {
    fn absolute(self) -> bool {
        matches!(self, Self::One | Self::Three)
    }
    fn half(self) -> usize {
        usize::from(matches!(self, Self::Three | Self::Four))
    }
    fn next(self) -> Self {
        match self {
            Self::One => Self::Two,
            Self::Two => Self::Three,
            Self::Three => Self::Four,
            Self::Four => Self::One,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenLoopLag(u16);
impl OpenLoopLag {
    pub fn samples(self) -> u16 {
        self.0
    }
    pub fn try_new(samples: u16) -> Result<Self, SearchError> {
        if !(MIN_SEARCH_LAG_SAMPLES..=MAX_SEARCH_LAG_SAMPLES).contains(&samples) {
            return Err(SearchError::OpenLoopLag(samples));
        }
        Ok(Self(samples))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    OpenLoopLag(u16),
    SubframeOrder {
        expected: Subframe,
        received: Subframe,
    },
}
impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MR74 pitch search failed: {self:?}")
    }
}
impl Error for SearchError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchedPitch {
    pub lag: PitchLag,
    pub index: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    previous: PitchLag,
    next: Subframe,
}
impl Default for Search {
    fn default() -> Self {
        const INITIAL_PITCH_INDEX: u8 = 62;
        Self {
            previous: PitchLag::from_absolute(INITIAL_PITCH_INDEX),
            next: Subframe::One,
        }
    }
}
impl Search {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 Pitch_fr for MR74. Input contains chronological excitation history followed by the current LPC residual.
    pub fn search(
        &mut self,
        excitation: &[i16; EXCITATION_SAMPLES],
        target: &[i16; SUBFRAME_SAMPLES],
        impulse_q12: &[i16; SUBFRAME_SAMPLES],
        open_loop: [OpenLoopLag; 2],
        subframe: Subframe,
    ) -> Result<SearchedPitch, SearchError> {
        if subframe != self.next {
            return Err(SearchError::SubframeOrder {
                expected: self.next,
                received: subframe,
            });
        }
        let (center, radius, span) = if subframe.absolute() {
            (
                usize::from(open_loop[subframe.half()].0),
                ABSOLUTE_RADIUS,
                ABSOLUTE_SPAN,
            )
        } else {
            (
                usize::from(self.previous.samples()),
                RELATIVE_RADIUS,
                RELATIVE_SPAN,
            )
        };
        let minimum = center.saturating_sub(radius).clamp(
            usize::from(MIN_SEARCH_LAG_SAMPLES),
            usize::from(MAX_SEARCH_LAG_SAMPLES) - span,
        );
        let maximum = minimum + span;
        let correlation = normalized_correlation(
            excitation,
            target,
            impulse_q12,
            minimum - INTERPOLATION_SAMPLES,
            maximum + INTERPOLATION_SAMPLES,
        );
        let mut lag = minimum;
        for candidate in minimum + 1..=maximum {
            if correlation[candidate] >= correlation[lag] {
                lag = candidate;
            }
        }
        let mut fraction = 0i16;
        if !subframe.absolute() || lag <= MAX_FRACTIONAL_LAG {
            fraction = -2;
            let mut best = interpolate(&correlation, lag, fraction);
            for candidate in -1..=2 {
                let value = interpolate(&correlation, lag, candidate);
                if value > best {
                    best = value;
                    fraction = candidate;
                }
            }
            if fraction == -2 {
                fraction = 1;
                lag -= 1;
            }
            if fraction == 2 {
                fraction = -1;
                lag += 1;
            }
        }
        let index = if subframe.absolute() {
            if lag <= INTEGER_ENCODING_LIMIT {
                lag as i16 * FRACTION_DENOMINATOR - ABSOLUTE_INDEX_BIAS + fraction
            } else {
                lag as i16 + INTEGER_INDEX_BIAS
            }
        } else {
            (lag as i16 - minimum as i16) * FRACTION_DENOMINATOR + RELATIVE_INDEX_BIAS + fraction
        };
        let index = index as u8;
        let lag = if subframe.absolute() {
            PitchLag::from_absolute(index)
        } else {
            PitchLag::from_relative(index, self.previous)
                .expect("relative pitch search fits five transmitted bits")
        };
        self.previous = lag;
        self.next = subframe.next();
        Ok(SearchedPitch { lag, index })
    }
}
fn normalized_correlation(
    excitation: &[i16; EXCITATION_SAMPLES],
    target: &[i16; SUBFRAME_SAMPLES],
    impulse: &[i16; SUBFRAME_SAMPLES],
    minimum: usize,
    maximum: usize,
) -> [i16; CORRELATION_SAMPLES] {
    let mut offset = HISTORY_SAMPLES - minimum;
    let mut filtered = convolve(
        excitation[offset..offset + SUBFRAME_SAMPLES]
            .try_into()
            .expect("pitch interval is inside excitation history"),
        impulse,
    );
    let energy = filtered
        .iter()
        .fold(0i32, |sum, &v| sum.saturating_add(product(v, v)));
    let scaling = if energy <= ENERGY_LIMIT {
        0
    } else {
        SAMPLE_SCALING
    };
    if scaling != 0 {
        for value in &mut filtered {
            *value >>= scaling;
        }
    }
    let factor = if scaling == 0 {
        FILTER_SHIFT
    } else {
        SCALED_FILTER_SHIFT
    };
    let mut correlation = [0; CORRELATION_SAMPLES];
    for (lag, destination) in correlation
        .iter_mut()
        .enumerate()
        .take(maximum + 1)
        .skip(minimum)
    {
        let energy = filtered
            .iter()
            .fold(0i32, |sum, &v| sum.saturating_add(product(v, v)));
        let sum = filtered
            .iter()
            .zip(target)
            .fold(0i32, |sum, (&v, &x)| sum.saturating_add(product(v, x)));
        *destination =
            (shift(multiply(sum & !1, inverse_sqrt(energy) & !1), WORD_SHIFT) >> WORD_SHIFT) as i16;
        if lag == maximum {
            break;
        }
        offset -= 1;
        for j in (1..SUBFRAME_SAMPLES).rev() {
            filtered[j] = ((shift(product(excitation[offset], impulse[j]), factor) >> WORD_SHIFT)
                as i16)
                .saturating_add(filtered[j - 1]);
        }
        filtered[0] = excitation[offset] >> scaling;
    }
    correlation
}
/// TS 26.073 Convolve truncates the scaled result without a rounding bias.
pub fn convolve(
    input: &[i16; SUBFRAME_SAMPLES],
    impulse_q12: &[i16; SUBFRAME_SAMPLES],
) -> [i16; SUBFRAME_SAMPLES] {
    std::array::from_fn(|n| {
        let sum = (0..=n).fold(0i32, |sum, i| {
            sum.saturating_add(product(input[i], impulse_q12[n - i]))
        });
        (shift(sum, FILTER_SHIFT) >> WORD_SHIFT) as i16
    })
}
fn interpolate(correlation: &[i16; CORRELATION_SAMPLES], mut lag: usize, fraction: i16) -> i16 {
    let mut phase = fraction * 2;
    if phase < 0 {
        phase += PHASES as i16;
        lag -= 1;
    }
    let phase = phase as usize;
    let mut sum = 0i32;
    for i in 0..INTERPOLATION_SAMPLES {
        sum = sum.saturating_add(product(
            correlation[lag - i],
            INTERPOLATOR[phase + i * PHASES],
        ));
        sum = sum.saturating_add(product(
            correlation[lag + 1 + i],
            INTERPOLATOR[PHASES - phase + i * PHASES],
        ));
    }
    (sum.saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn shift(value: i32, shift: u32) -> i32 {
    (i64::from(value) << shift).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}
fn split(value: i32) -> (i16, i16) {
    let high = (value >> WORD_SHIFT) as i16;
    (high, ((value >> 1) - (i32::from(high) << Q15_SHIFT)) as i16)
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
#[cfg(test)]
mod tests;
