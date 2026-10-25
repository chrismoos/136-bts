use super::{
    SUBFRAME_SAMPLES, SUBFRAMES,
    comfort_noise::ComfortNoiseExcitation,
    decoder::FRAME_SAMPLES,
    lpc,
    noise_history::{BackwardNoise, HISTORY_FRAMES},
    noise_level::NoiseLevel,
    noise_spectrum,
    sid::SidParameters,
    sid_interpolation::{SidInterpolation, Transition},
    spectral::{self, ConversionError, LPC_ORDER, Spectrum},
    synthesis::SynthesisFilter,
};
const Q15_SHIFT: u32 = 15;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub samples_q0: [i16; FRAME_SAMPLES],
    pub coefficients_q12: [[i16; lpc::COEFFICIENTS]; SUBFRAMES],
    pub spectrum: Spectrum,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoiseSynthesis {
    interpolation: SidInterpolation,
    level: NoiseLevel,
    excitation: ComfortNoiseExcitation,
    deviations_q15: [[i16; LPC_ORDER]; HISTORY_FRAMES],
}
impl NoiseSynthesis {
    pub(super) fn prediction_energy_q10(&self) -> i16 {
        super::sid::prediction_energy(self.interpolation.current_log_energy_q11())
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn apply_sid(
        &mut self,
        parameters: &SidParameters,
        interval_frames: u16,
        transition: Transition,
    ) {
        self.interpolation
            .apply(parameters, interval_frames, transition);
    }
    /// Muting changes the following frame. The caller resets SID age after this update.
    pub fn mute_after_render(&mut self, since_last_sid_frames: u16) {
        self.interpolation.mute_after_render(since_last_sid_frames);
    }
    pub fn hold_sid(&mut self) {
        self.interpolation.hold_current();
    }
    pub fn apply_backward(&mut self, noise: &BackwardNoise) -> Result<(), ConversionError> {
        spectral::lsf_to_lsp(&noise.spectrum.lsf_q15)?;
        spectral::lsp_to_lsf(&noise.spectrum.lsp_q15)?;
        self.interpolation
            .replace_backward_descriptor(&noise.spectrum, noise.log_energy_q11);
        self.level.set_hangover_adjustment();
        self.deviations_q15 = noise.deviations_q15;
        Ok(())
    }
    /// TS 26.073 dtx_dec shares synthesis history with speech while using baseline coefficients for postfiltering and varied coefficients for noise synthesis.
    pub fn render(
        &mut self,
        since_last_sid_frames: u16,
        synthesis: &mut SynthesisFilter,
    ) -> Result<Frame, ConversionError> {
        let snapshot = self.interpolation.at(since_last_sid_frames);
        let mut excitation = self.excitation.clone();
        let mut level = self.level.clone();
        let index = usize::from(excitation.spectral_index());
        let spectra = noise_spectrum::vary_spectrum(
            &snapshot.lsp_q15,
            level.prediction_mean_q12(),
            &self.deviations_q15[index],
        )?;
        let baseline = lpc::convert(&spectra.baseline.lsp_q15);
        let varied = lpc::convert(&spectra.varied.lsp_q15);
        let coefficients = baseline[1..]
            .try_into()
            .expect("predictor has exactly ten nonleading coefficients");
        let gain = level
            .calculate(snapshot.log_energy_q26, coefficients)
            .gain_q4;
        let mut samples_q0 = [0; FRAME_SAMPLES];
        for subframe in 0..SUBFRAMES {
            let code = excitation.generate().map(|sample| {
                ((i32::from(gain) * i32::from(sample)) >> Q15_SHIFT)
                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
            });
            let output = synthesis.filter(&varied, &code);
            samples_q0[subframe * SUBFRAME_SAMPLES..(subframe + 1) * SUBFRAME_SAMPLES]
                .copy_from_slice(&output.samples_q0);
        }
        self.excitation = excitation;
        self.level = level;
        Ok(Frame {
            samples_q0,
            coefficients_q12: [baseline; SUBFRAMES],
            spectrum: spectra.baseline,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sid_driven_noise_samples_coefficients_and_spectra_match_native_dtx_synthesis() {
        let mut hash = 0xcbf29ce484222325u64;
        for (pattern, interval) in [1, 8, 32, 64].into_iter().enumerate() {
            let mut decoder = NoiseSynthesis::default();
            let mut synthesis = SynthesisFilter::default();
            let mut age = 0u16;
            let mut updated = false;
            for frame in 0..512 {
                age = age.saturating_add(1);
                let valid = frame % interval == 0 && (pattern != 3 || frame > 0);
                let invalid = !valid && frame % 17 == 7;
                if valid {
                    let parameters = SidParameters::try_new([
                        (frame % 8) as u16,
                        ((frame * 17) % 256) as u16,
                        ((frame * 73) % 512) as u16,
                        ((frame * 197) % 512) as u16,
                        (frame % 64) as u16,
                    ])
                    .unwrap();
                    decoder.apply_sid(
                        &parameters,
                        age,
                        if updated {
                            Transition::Interpolate
                        } else {
                            Transition::Immediate
                        },
                    );
                    age = 0;
                    updated = true;
                } else if invalid {
                    decoder.hold_sid();
                }
                let output = decoder.render(age, &mut synthesis).unwrap();
                for value in output
                    .samples_q0
                    .into_iter()
                    .chain(output.coefficients_q12.into_iter().flatten())
                    .chain(output.spectrum.lsf_q15)
                {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            decoder.reset();
            assert_eq!(decoder, NoiseSynthesis::default());
        }
        assert_eq!(hash, 0x59361392f4e75e63);
    }
    #[test]
    fn post_frame_noise_muting_matches_native_dtx_samples_and_recovery() {
        let mut hash = 0xcbf29ce484222325u64;
        for (pattern, interval) in [1, 8, 32, 64].into_iter().enumerate() {
            let mut decoder = NoiseSynthesis::default();
            let mut synthesis = SynthesisFilter::default();
            let mut age = 0u16;
            let mut updated = false;
            for frame in 0..512 {
                age = age.saturating_add(1);
                let valid = frame % interval == 0 && (pattern != 3 || frame > 0);
                let invalid = !valid && frame % 17 == 7;
                if valid {
                    let parameters = SidParameters::try_new([
                        (frame % 8) as u16,
                        ((frame * 17) % 256) as u16,
                        ((frame * 73) % 512) as u16,
                        ((frame * 197) % 512) as u16,
                        (frame % 64) as u16,
                    ])
                    .unwrap();
                    decoder.apply_sid(
                        &parameters,
                        age,
                        if updated {
                            Transition::Interpolate
                        } else {
                            Transition::Immediate
                        },
                    );
                    age = 0;
                    updated = true;
                } else if invalid {
                    decoder.hold_sid();
                }
                let output = decoder.render(age, &mut synthesis).unwrap();
                if frame % 96 >= 48 && !valid {
                    decoder.mute_after_render(age);
                    age = 0;
                }
                for value in output
                    .samples_q0
                    .into_iter()
                    .chain(output.coefficients_q12.into_iter().flatten())
                    .chain(output.spectrum.lsf_q15)
                {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            decoder.reset();
            assert_eq!(decoder, NoiseSynthesis::default());
        }
        assert_eq!(hash, 0x2c09241a61197bbb);
    }
    #[test]
    fn rejected_variability_preserves_noise_generator_level_and_synthesis_history() {
        let mut decoder = NoiseSynthesis {
            deviations_q15: [[i16::MAX; LPC_ORDER]; HISTORY_FRAMES],
            ..NoiseSynthesis::default()
        };
        let before = decoder.clone();
        let mut synthesis = SynthesisFilter::default();
        let before_synthesis = synthesis.clone();
        assert!(decoder.render(0, &mut synthesis).is_err());
        assert_eq!(decoder, before);
        assert_eq!(synthesis, before_synthesis);
    }
}
