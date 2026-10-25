use super::{
    Parameters, SUBFRAMES,
    spectral::{LPC_ORDER, SpectralDecoder, Spectrum},
};
pub const COEFFICIENTS: usize = LPC_ORDER + 1;
const POLYNOMIAL_ORDER: usize = LPC_ORDER / 2;
const POLYNOMIAL_SAMPLES: usize = POLYNOMIAL_ORDER + 1;
const POLYNOMIAL_ONE_Q24: i32 = 1 << 24;
const POLYNOMIAL_LSP_FACTOR: i16 = 512;
const Q12_ONE: i16 = 1 << 12;
const COEFFICIENT_SHIFT: u32 = 13;
const WORD_SHIFT: u32 = 16;
const Q15_SHIFT: u32 = 15;
// TS 26.073 lsp_init_data initializes the preceding frame's cosine-domain spectrum.
pub(super) const INITIAL_LSP_Q15: [i16; LPC_ORDER] = [
    30000, 26000, 21000, 15000, 8000, 0, -8000, -15000, -21000, -26000,
];
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub spectrum: Spectrum,
    pub coefficients_q12: [[i16; COEFFICIENTS]; SUBFRAMES],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LpcDecoder {
    spectral: SpectralDecoder,
    previous_lsp_q15: [i16; LPC_ORDER],
}
impl Default for LpcDecoder {
    fn default() -> Self {
        Self {
            spectral: SpectralDecoder::default(),
            previous_lsp_q15: INITIAL_LSP_Q15,
        }
    }
}
impl LpcDecoder {
    pub(super) fn resume_noise_spectrum(&mut self, spectrum: Spectrum) {
        self.spectral.resume_noise_spectrum(spectrum);
        self.previous_lsp_q15 = spectrum.lsp_q15;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// Consecutive good MR74 frames advance both residual prediction and preceding-frame interpolation state.
    pub fn decode(&mut self, parameters: &Parameters) -> Frame {
        let spectrum = self.spectral.decode(parameters);
        self.interpolate(spectrum)
    }
    /// Erasures advance both the concealed spectral predictor and the preceding-frame interpolation state.
    pub fn conceal(&mut self) -> Frame {
        let spectrum = self.spectral.conceal();
        self.interpolate(spectrum)
    }
    fn interpolate(&mut self, spectrum: Spectrum) -> Frame {
        let coefficients_q12 = std::array::from_fn(|subframe| {
            // TS 26.073 Int_lpc_1to3 shifts each term separately before addition.
            let lsp = std::array::from_fn(|index| {
                let old = self.previous_lsp_q15[index];
                let new = spectrum.lsp_q15[index];
                match subframe {
                    0 => (new >> 2).saturating_add(old.saturating_sub(old >> 2)),
                    1 => (old >> 1).saturating_add(new >> 1),
                    2 => (old >> 2).saturating_add(new.saturating_sub(new >> 2)),
                    _ => new,
                }
            });
            convert(&lsp)
        });
        self.previous_lsp_q15 = spectrum.lsp_q15;
        Frame {
            spectrum,
            coefficients_q12,
        }
    }
}
// TS 26.073 Lsp_Az expands two Q24 polynomials with split-word multiplication and rounds to Q12.
pub(super) fn convert(lsp: &[i16; LPC_ORDER]) -> [i16; COEFFICIENTS] {
    let mut first = polynomial(lsp, 0);
    let mut second = polynomial(lsp, 1);
    for index in (1..=POLYNOMIAL_ORDER).rev() {
        first[index] = first[index].saturating_add(first[index - 1]);
        second[index] = second[index].saturating_sub(second[index - 1]);
    }
    let mut coefficients = [0; COEFFICIENTS];
    coefficients[0] = Q12_ONE;
    for index in 1..=POLYNOMIAL_ORDER {
        coefficients[index] = rounded_coefficient(first[index].saturating_add(second[index]));
        coefficients[COEFFICIENTS - index] =
            rounded_coefficient(first[index].saturating_sub(second[index]));
    }
    coefficients
}
fn polynomial(lsp: &[i16; LPC_ORDER], offset: usize) -> [i32; POLYNOMIAL_SAMPLES] {
    let mut coefficients = [0i32; POLYNOMIAL_SAMPLES];
    coefficients[0] = POLYNOMIAL_ONE_Q24;
    coefficients[1] = 0i32.saturating_sub(doubled_product(lsp[offset], POLYNOMIAL_LSP_FACTOR));
    for degree in 2..=POLYNOMIAL_ORDER {
        let root = lsp[offset + (degree - 1) * 2];
        coefficients[degree] = coefficients[degree - 2];
        for index in (2..=degree).rev() {
            let previous = coefficients[index - 1];
            let high = (previous >> WORD_SHIFT) as i16;
            let low = ((previous >> 1) - (i32::from(high) << Q15_SHIFT)) as i16;
            let fraction = ((i32::from(low) * i32::from(root)) >> Q15_SHIFT) as i16;
            let product = doubled_product(high, root).saturating_add(doubled_product(fraction, 1));
            coefficients[index] = coefficients[index]
                .saturating_add(coefficients[index - 2])
                .saturating_sub(product.saturating_mul(2));
        }
        coefficients[1] =
            coefficients[1].saturating_sub(doubled_product(root, POLYNOMIAL_LSP_FACTOR));
    }
    coefficients
}
fn doubled_product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn rounded_coefficient(value: i32) -> i16 {
    ((value >> COEFFICIENT_SHIFT) + i32::from(value & (1 << (COEFFICIENT_SHIFT - 1)) != 0)) as i16
}
#[cfg(test)]
mod tests {
    use super::*;
    fn parameters(step: usize) -> Parameters {
        let mut values = [0; super::super::PARAMETER_COUNT];
        values[..super::super::LSF_INDICES].copy_from_slice(&[
            (step % 256) as u16,
            ((step * 73 + step / 512) % 512) as u16,
            ((step * 197 + step / 256) % 512) as u16,
        ]);
        Parameters::try_new(values).unwrap()
    }
    #[test]
    fn interpolated_coefficients_and_saturated_polynomials_match_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        let mut include = |value: i16| {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        };
        let mut decoder = LpcDecoder::default();
        for step in 0..4096 {
            let frame = decoder.decode(&parameters(step));
            for value in frame
                .coefficients_q12
                .into_iter()
                .flatten()
                .chain(frame.spectrum.lsp_q15)
            {
                include(value);
            }
        }
        for pattern in 0..5 {
            let lsp = std::array::from_fn(|index| match pattern {
                0 => 0,
                1 => i16::MAX,
                2 => i16::MIN,
                3 => {
                    if index % 2 == 0 {
                        i16::MAX
                    } else {
                        i16::MIN
                    }
                }
                _ => (index as i32 * 7000 - 30000) as i16,
            });
            for value in convert(&lsp) {
                include(value);
            }
        }
        assert_eq!(hash, 0x3c4c6b7728e19488);
    }
    #[test]
    fn erased_frame_interpolation_and_recovery_match_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        let mut include = |value: i16| {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        };
        for pattern in 0..4 {
            let mut decoder = LpcDecoder::default();
            for step in 0..4096 {
                let bad = match pattern {
                    0 => step % 7 == 3,
                    1 => step % 32 >= 4,
                    2 => step % 128 < 127,
                    _ => step < 2048,
                };
                let frame = if bad {
                    decoder.conceal()
                } else {
                    decoder.decode(&parameters(step))
                };
                for value in frame
                    .coefficients_q12
                    .into_iter()
                    .flatten()
                    .chain(frame.spectrum.lsp_q15)
                {
                    include(value);
                }
            }
            decoder.reset();
            assert_eq!(decoder, LpcDecoder::default());
        }
        for pattern in 0..5 {
            let lsp = std::array::from_fn(|index| match pattern {
                0 => 0,
                1 => i16::MAX,
                2 => i16::MIN,
                3 => {
                    if index % 2 == 0 {
                        i16::MAX
                    } else {
                        i16::MIN
                    }
                }
                _ => (index as i32 * 7000 - 30000) as i16,
            });
            for value in convert(&lsp) {
                include(value);
            }
        }
        assert_eq!(hash, 0x6dea5ba5bb3b130a);
    }
    #[test]
    fn reset_restores_both_spectral_prediction_and_interpolation_state() {
        let mut decoder = LpcDecoder::default();
        let first = decoder.decode(&parameters(0));
        decoder.decode(&parameters(1));
        decoder.reset();
        assert_eq!(decoder.decode(&parameters(0)), first);
    }
}
