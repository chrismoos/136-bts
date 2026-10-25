use super::{
    SUBFRAME_SAMPLES, SUBFRAMES, decoder::FRAME_SAMPLES, energy::GainControl, lpc::COEFFICIENTS,
    spectral::LPC_ORDER, synthesis::SynthesisFilter,
};
const IMPULSE_SAMPLES: usize = 22;
const TILT_FACTOR_Q15: i16 = 26214;
const Q15_SHIFT: u32 = 15;
const WORD_SHIFT: u32 = 16;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
const RESIDUAL_SHIFT: u32 = 3;
// TS 26.073 Post_Filter selects these expansion factors for MR74.
const NUMERATOR_Q15: [i16; LPC_ORDER] = [18022, 9912, 5451, 2998, 1649, 907, 499, 274, 151, 83];
const DENOMINATOR_Q15: [i16; LPC_ORDER] =
    [22938, 16057, 11240, 7868, 5508, 3856, 2699, 1889, 1322, 925];
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Postfilter {
    input_history: [i16; LPC_ORDER],
    previous_residual: i16,
    synthesis: SynthesisFilter,
    gain: GainControl,
}
impl Postfilter {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn process(
        &mut self,
        samples: &mut [i16; FRAME_SAMPLES],
        coefficients: &[[i16; COEFFICIENTS]; SUBFRAMES],
    ) {
        let mut input = [0i16; LPC_ORDER + FRAME_SAMPLES];
        input[..LPC_ORDER].copy_from_slice(&self.input_history);
        input[LPC_ORDER..].copy_from_slice(samples);
        for (subframe, coefficients) in coefficients.iter().enumerate() {
            let numerator = weight(coefficients, &NUMERATOR_Q15);
            let denominator = weight(coefficients, &DENOMINATOR_Q15);
            let start = LPC_ORDER + subframe * SUBFRAME_SAMPLES;
            let mut residual = std::array::from_fn(|sample| {
                let position = start + sample;
                let mut sum = product(input[position], numerator[0]);
                for tap in 1..COEFFICIENTS {
                    sum = sum.saturating_add(product(numerator[tap], input[position - tap]));
                }
                let scaled = (i64::from(sum) << RESIDUAL_SHIFT)
                    .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                    as i32;
                rounded(scaled)
            });
            let mut impulse = [0i16; SUBFRAME_SAMPLES];
            impulse[..COEFFICIENTS].copy_from_slice(&numerator);
            let impulse = SynthesisFilter::default()
                .preview(&denominator, &impulse)
                .samples_q0;
            let energy = impulse[..IMPULSE_SAMPLES]
                .iter()
                .fold(0i32, |sum, &sample| {
                    sum.saturating_add(product(sample, sample))
                });
            let correlation = impulse[..IMPULSE_SAMPLES]
                .windows(2)
                .fold(0i32, |sum, pair| {
                    sum.saturating_add(product(pair[0], pair[1]))
                });
            let correlation = (correlation >> WORD_SHIFT) as i16;
            let energy = (energy >> WORD_SHIFT) as i16;
            let tilt = if correlation <= 0 {
                0
            } else {
                let weighted = multiply(correlation, TILT_FACTOR_Q15);
                ((i32::from(weighted) << Q15_SHIFT) / i32::from(energy)).min(i32::from(i16::MAX))
                    as i16
            };
            for sample in &mut residual {
                let raw = *sample;
                *sample = sample.saturating_sub(multiply(tilt, self.previous_residual));
                self.previous_residual = raw;
            }
            let mut output = self.synthesis.filter(&denominator, &residual).samples_q0;
            let original: [i16; SUBFRAME_SAMPLES] = input[start..start + SUBFRAME_SAMPLES]
                .try_into()
                .expect("input contains a full synthesis subframe");
            self.gain.process(&original, &mut output);
            samples[subframe * SUBFRAME_SAMPLES..(subframe + 1) * SUBFRAME_SAMPLES]
                .copy_from_slice(&output);
        }
        self.input_history
            .copy_from_slice(&input[input.len() - LPC_ORDER..]);
    }
}
pub(super) fn weight(
    coefficients: &[i16; COEFFICIENTS],
    factors: &[i16; LPC_ORDER],
) -> [i16; COEFFICIENTS] {
    std::array::from_fn(|index| {
        if index == 0 {
            coefficients[0]
        } else {
            rounded(product(coefficients[index], factors[index - 1]))
        }
    })
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn rounded(value: i32) -> i16 {
    (value.saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16
}
fn multiply(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
