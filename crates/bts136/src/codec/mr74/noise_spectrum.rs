use super::spectral::{self, ConversionError, LPC_ORDER, Spectrum};
const PREDICTION_OFFSET_Q12: i16 = 2457;
const VARIABILITY_MULTIPLIER_Q15: i16 = 9830;
const VARIABILITY_ONE_Q12: i16 = 4096;
const VARIABILITY_SCALE_SHIFT: u32 = 3;
const Q15_SHIFT: u32 = 15;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoiseSpectrum {
    pub baseline: Spectrum,
    pub varied: Spectrum,
    pub variability_q15: i16,
}
/// TS 26.073 dtx_dec uses the preceding prediction-gain mean and a selected LSF history-deviation vector.
pub fn vary_spectrum(
    lsp_q15: &[i16; LPC_ORDER],
    prediction_mean_q12: i16,
    deviation_q15: &[i16; LPC_ORDER],
) -> Result<NoiseSpectrum, ConversionError> {
    let variability = VARIABILITY_ONE_Q12
        .saturating_sub(multiply_q15(
            prediction_mean_q12.saturating_sub(PREDICTION_OFFSET_Q12),
            VARIABILITY_MULTIPLIER_Q15,
        ))
        .clamp(0, VARIABILITY_ONE_Q12);
    let variability_q15 =
        (i32::from(variability) << VARIABILITY_SCALE_SHIFT).min(i32::from(i16::MAX)) as i16;
    let mut baseline_lsf = spectral::lsp_to_lsf(lsp_q15)?;
    let mut varied_lsf = std::array::from_fn(|index| {
        baseline_lsf[index].saturating_add(multiply_q15(variability_q15, deviation_q15[index]))
    });
    spectral::reorder_frequencies(&mut baseline_lsf);
    spectral::reorder_frequencies(&mut varied_lsf);
    let baseline_lsp = spectral::lsf_to_lsp(&baseline_lsf)?;
    let varied_lsp = spectral::lsf_to_lsp(&varied_lsf)?;
    Ok(NoiseSpectrum {
        baseline: Spectrum {
            lsf_q15: baseline_lsf,
            lsp_q15: baseline_lsp,
        },
        varied: Spectrum {
            lsf_q15: varied_lsf,
            lsp_q15: varied_lsp,
        },
        variability_q15,
    })
}
fn multiply_q15(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::{sid::SidParameters, spectral::SpectralDecoder};
    #[test]
    fn full_prediction_mean_domain_matches_reference_spectra_and_variability() {
        let mut hash = 0xcbf29ce484222325u64;
        for mean in i16::MIN..=i16::MAX {
            let step = (i32::from(mean) - i32::from(i16::MIN)) as usize;
            let parameters = SidParameters::try_new([
                (step % 8) as u16,
                ((step * 17) % 256) as u16,
                ((step * 73) % 512) as u16,
                ((step * 197) % 512) as u16,
                0,
            ])
            .unwrap();
            let spectrum = SpectralDecoder::default().decode_reference_sid(&parameters);
            let deviation =
                std::array::from_fn(|index| (((step * 37 + index * 73) % 257) as i32 - 128) as i16);
            let output = vary_spectrum(&spectrum.lsp_q15, mean, &deviation).unwrap();
            for value in output
                .baseline
                .lsf_q15
                .into_iter()
                .chain(output.varied.lsf_q15)
                .chain(output.baseline.lsp_q15)
                .chain(output.varied.lsp_q15)
                .chain([output.variability_q15])
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        assert_eq!(hash, 0x0d2e9407c91ba3e2);
    }
    #[test]
    fn invalid_cosines_or_excessive_deviations_reject_without_indexing_outside_tables() {
        let mut lsp = [0; LPC_ORDER];
        lsp[1] = 1;
        assert_eq!(
            vary_spectrum(&lsp, 0, &[0; LPC_ORDER]),
            Err(ConversionError::UnorderedCosines { index: 1 })
        );
        let spectrum = SpectralDecoder::default()
            .decode_reference_sid(&SidParameters::try_new([0; 5]).unwrap());
        assert!(matches!(
            vary_spectrum(&spectrum.lsp_q15, 0, &[i16::MAX; LPC_ORDER]),
            Err(ConversionError::Frequency { .. })
        ));
    }
}
