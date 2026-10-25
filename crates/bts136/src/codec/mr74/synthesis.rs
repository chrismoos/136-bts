use super::{SUBFRAME_SAMPLES, lpc::COEFFICIENTS, spectral::LPC_ORDER};
const SYNTHESIS_SCALE_SHIFT: u32 = 3;
const WORD_SHIFT: u32 = 16;
const ROUND_BIAS: i64 = 1 << (WORD_SHIFT - 1);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    pub samples_q0: [i16; SUBFRAME_SAMPLES],
    pub overflowed: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SynthesisFilter {
    history_q0: [i16; LPC_ORDER],
}
impl SynthesisFilter {
    pub(super) fn with_history(history_q0: [i16; LPC_ORDER]) -> Self {
        Self { history_q0 }
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// Preview preserves history so an overflow retry starts from the same preceding samples.
    pub fn preview(
        &self,
        coefficients_q12: &[i16; COEFFICIENTS],
        excitation_q0: &[i16; SUBFRAME_SAMPLES],
    ) -> Output {
        let mut buffer = [0i16; LPC_ORDER + SUBFRAME_SAMPLES];
        buffer[..LPC_ORDER].copy_from_slice(&self.history_q0);
        let mut overflowed = false;
        // TS 26.073 Syn_filt saturates each doubled product and subtraction before scaling and rounding.
        for (sample, &excitation) in excitation_q0.iter().enumerate() {
            let position = LPC_ORDER + sample;
            let mut sum = saturated(
                i64::from(excitation) * i64::from(coefficients_q12[0]) * 2,
                &mut overflowed,
            );
            for (tap, &coefficient) in coefficients_q12.iter().enumerate().skip(1) {
                let product = saturated(
                    i64::from(coefficient) * i64::from(buffer[position - tap]) * 2,
                    &mut overflowed,
                );
                sum = saturated(i64::from(sum) - i64::from(product), &mut overflowed);
            }
            sum = saturated(i64::from(sum) << SYNTHESIS_SCALE_SHIFT, &mut overflowed);
            buffer[position] =
                (saturated(i64::from(sum) + ROUND_BIAS, &mut overflowed) >> WORD_SHIFT) as i16;
        }
        Output {
            samples_q0: buffer[LPC_ORDER..]
                .try_into()
                .expect("one synthesized subframe follows filter history"),
            overflowed,
        }
    }
    /// Commits the final ten samples even when arithmetic saturates, matching Syn_filt with update enabled.
    pub fn filter(
        &mut self,
        coefficients_q12: &[i16; COEFFICIENTS],
        excitation_q0: &[i16; SUBFRAME_SAMPLES],
    ) -> Output {
        let output = self.preview(coefficients_q12, excitation_q0);
        self.history_q0
            .copy_from_slice(&output.samples_q0[SUBFRAME_SAMPLES - LPC_ORDER..]);
        output
    }
}
fn saturated(value: i64, overflowed: &mut bool) -> i32 {
    let clamped = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    *overflowed |= clamped != value;
    clamped as i32
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::{LSF_INDICES, PARAMETER_COUNT, Parameters, lpc::LpcDecoder};
    fn parameters(step: usize) -> Parameters {
        let mut values = [0; PARAMETER_COUNT];
        values[..LSF_INDICES].copy_from_slice(&[
            (step % 256) as u16,
            ((step * 73 + step / 512) % 512) as u16,
            ((step * 197 + step / 256) % 512) as u16,
        ]);
        Parameters::try_new(values).unwrap()
    }
    #[test]
    fn samples_history_and_overflow_match_reference_with_changing_coefficients() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..6 {
            let mut predictor = LpcDecoder::default();
            let mut filter = SynthesisFilter::default();
            for step in 0..512 {
                let frame = predictor.decode(&parameters(step));
                let mut coefficients = frame.coefficients_q12[step % 4];
                if pattern == 5 {
                    coefficients[0] = 4096;
                    for (index, coefficient) in coefficients.iter_mut().enumerate().skip(1) {
                        *coefficient = if index % 2 == 0 { i16::MAX } else { i16::MIN };
                    }
                }
                let excitation = std::array::from_fn(|index| match pattern {
                    0 => 0,
                    1 => {
                        if step == 0 && index == 0 {
                            30000
                        } else {
                            0
                        }
                    }
                    2 => {
                        if index % 2 == 0 {
                            i16::MAX
                        } else {
                            i16::MIN
                        }
                    }
                    3 => i16::MAX,
                    4 => i16::MIN,
                    _ => (((index * 347 + step * 997) % 65536) as i32 - 32768) as i16,
                });
                let output = filter.filter(&coefficients, &excitation);
                for value in output
                    .samples_q0
                    .into_iter()
                    .chain(filter.history_q0)
                    .chain([i16::from(output.overflowed)])
                {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
        assert_eq!(hash, 0x72a23ff8df14f548);
    }
    #[test]
    fn preview_and_reset_preserve_retry_and_initial_history() {
        let mut filter = SynthesisFilter::default();
        let coefficients = LpcDecoder::default()
            .decode(&parameters(0))
            .coefficients_q12[0];
        let excitation = [30000; SUBFRAME_SAMPLES];
        let before = filter.clone();
        let preview = filter.preview(&coefficients, &excitation);
        assert_eq!(filter, before);
        assert_eq!(filter.filter(&coefficients, &excitation), preview);
        filter.reset();
        assert_eq!(filter, before);
    }
}
