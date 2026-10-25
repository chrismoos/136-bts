use super::{
    Parameters, SUBFRAME_SAMPLES, SUBFRAMES, adaptive,
    frame_loss::{SpeechLossState, SpeechQuality},
    gain,
    gain_concealment::GainConcealment,
    missing_parameters::MissingParameters,
    pitch,
};
const SHARP_MAX_Q14: i16 = 13017;
const Q15_SHIFT: u32 = 15;
const ROUND_BIAS: i32 = 1 << Q15_SHIFT;
const WORD_SHIFT: u32 = 16;
const OVERFLOW_RECOVERY_SHIFT: u32 = 2;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryAdjustment {
    Preserve,
    Quarter,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subframe {
    pub adaptive_q0: [i16; SUBFRAME_SAMPLES],
    pub innovation_q13: [i16; SUBFRAME_SAMPLES],
    pub feedback_q0: [i16; SUBFRAME_SAMPLES],
    pub gains: gain::Gains,
    pub pitch: pitch::PitchLag,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcitationDecoder {
    history_q0: [i16; adaptive::HISTORY_SAMPLES],
    sharpening_q14: i16,
    gains: gain::GainDecoder,
    pitch: pitch::PitchDecoder,
    concealment: GainConcealment,
    loss: SpeechLossState,
    missing: MissingParameters,
}
impl Default for ExcitationDecoder {
    fn default() -> Self {
        Self {
            history_q0: [0; adaptive::HISTORY_SAMPLES],
            sharpening_q14: 0,
            gains: gain::GainDecoder::default(),
            pitch: pitch::PitchDecoder::default(),
            concealment: GainConcealment::default(),
            loss: SpeechLossState::default(),
            missing: MissingParameters::default(),
        }
    }
}
impl ExcitationDecoder {
    pub(super) fn resume_noise(&mut self, energy_q10: i16, muted: bool) {
        self.reset();
        self.gains.resume_noise_energy(energy_q10);
        self.loss.resume_noise(muted);
    }

    pub(super) fn after_initial_good_speech(&mut self) {
        self.loss.after_initial_good_speech();
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// Good-frame recovery applies the preceding erased-frame gain limits.
    pub fn decode(&mut self, parameters: &Parameters) -> [Subframe; SUBFRAMES] {
        self.decode_with_history_adjustment(parameters, |_| HistoryAdjustment::Preserve)
    }
    /// TS 26.073 Decoder_amr quarters retained excitation after synthesis overflow before advancing the next subframe.
    pub fn decode_with_history_adjustment(
        &mut self,
        parameters: &Parameters,
        adjustment: impl FnMut(&Subframe) -> HistoryAdjustment,
    ) -> [Subframe; SUBFRAMES] {
        self.decode_quality(parameters, SpeechQuality::Good, adjustment)
    }
    /// Conceals supplied corrupted indices within established speech. Comfort-noise transitions are handled separately.
    pub fn decode_bad(&mut self, parameters: &Parameters) -> [Subframe; SUBFRAMES] {
        self.decode_quality(parameters, SpeechQuality::Erased, |_| {
            HistoryAdjustment::Preserve
        })
    }
    /// Missing payloads use the reference generated indices before established-speech concealment.
    pub fn conceal(&mut self) -> [Subframe; SUBFRAMES] {
        let parameters = self.missing.generate();
        self.decode_bad(&parameters)
    }
    pub(crate) fn decode_quality(
        &mut self,
        parameters: &Parameters,
        quality: SpeechQuality,
        mut adjustment: impl FnMut(&Subframe) -> HistoryAdjustment,
    ) -> [Subframe; SUBFRAMES] {
        let loss = self.loss.advance(quality);
        let lags = match quality {
            SpeechQuality::Good => self.pitch.decode(parameters),
            SpeechQuality::Erased => self.pitch.conceal(),
        };
        let subframes = parameters.subframes();
        std::array::from_fn(|index| {
            let pitch = lags[index];
            let adaptive_q0 = adaptive::predict(&self.history_q0, pitch);
            let mut innovation_q13 = *subframes[index]
                .fixed_codebook()
                .expect("validated MR74 parameters bound codebook indices")
                .samples_q13();
            let sharpening_q15 = self.sharpening_q14.saturating_mul(2);
            let lag_samples = usize::from(pitch.samples());
            // TS 26.073 Decoder_amr sharpens innovation recursively with the preceding subframe's clipped pitch gain.
            for sample in lag_samples..SUBFRAME_SAMPLES {
                let contribution = ((i32::from(innovation_q13[sample - lag_samples])
                    * i32::from(sharpening_q15))
                    >> Q15_SHIFT) as i16;
                innovation_q13[sample] = innovation_q13[sample].saturating_add(contribution);
            }
            let gains = match quality {
                SpeechQuality::Good => {
                    let gains = self
                        .gains
                        .decode(subframes[index].gain_index, &innovation_q13)
                        .expect("validated MR74 parameters bound gain indices");
                    self.concealment
                        .update_good(gains, loss.previous_frame_erased)
                }
                SpeechQuality::Erased => {
                    self.concealment.conceal(loss.attenuation, &mut self.gains)
                }
            };
            self.sharpening_q14 = gains.pitch_q14.min(SHARP_MAX_Q14);
            let feedback_q0 = std::array::from_fn(|sample| {
                let sum = doubled_product(adaptive_q0[sample], gains.pitch_q14)
                    .saturating_add(doubled_product(innovation_q13[sample], gains.code_q1));
                (sum.saturating_mul(2).saturating_add(ROUND_BIAS) >> WORD_SHIFT) as i16
            });
            let mut subframe = Subframe {
                adaptive_q0,
                innovation_q13,
                feedback_q0,
                gains,
                pitch,
            };
            if adjustment(&subframe) == HistoryAdjustment::Quarter {
                for sample in &mut self.history_q0 {
                    *sample >>= OVERFLOW_RECOVERY_SHIFT;
                }
                for sample in &mut subframe.feedback_q0 {
                    *sample >>= OVERFLOW_RECOVERY_SHIFT;
                }
            }
            self.history_q0.copy_within(SUBFRAME_SAMPLES.., 0);
            self.history_q0[adaptive::HISTORY_SAMPLES - SUBFRAME_SAMPLES..]
                .copy_from_slice(&subframe.feedback_q0);
            subframe
        })
    }
}
fn doubled_product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn parameters(frame: usize) -> Parameters {
        let mut values = [0; super::super::PARAMETER_COUNT];
        for sf in 0..SUBFRAMES {
            let step = frame * SUBFRAMES + sf;
            let pitch = if sf % 2 == 1 {
                (frame * 7 + sf) % 32
            } else if sf == 0 {
                frame % 256
            } else {
                (frame * 53 + 197) % 256
            };
            let start = super::super::LSF_INDICES + sf * 4;
            values[start..start + 4].copy_from_slice(&[
                pitch as u16,
                ((step * 73) % 8192) as u16,
                (step % 16) as u16,
                (step % 128) as u16,
            ]);
        }
        Parameters::try_new(values).unwrap()
    }
    #[test]
    fn complete_excitation_chain_matches_reference_across_consecutive_frames() {
        let mut decoder = ExcitationDecoder::default();
        let mut hash = 0xcbf29ce484222325u64;
        let mut history = [0i16; adaptive::HISTORY_SAMPLES];
        let mut include = |value: i16| {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        };
        for frame in 0..512 {
            let output = decoder.decode(&parameters(frame));
            for subframe in &output {
                for value in subframe
                    .adaptive_q0
                    .into_iter()
                    .chain(subframe.innovation_q13)
                    .chain(subframe.feedback_q0)
                    .chain([
                        subframe.gains.pitch_q14,
                        subframe.gains.code_q1,
                        subframe.pitch.samples() as i16,
                        subframe.pitch.fraction().thirds(),
                    ])
                {
                    include(value);
                }
                history.copy_within(SUBFRAME_SAMPLES.., 0);
                history[adaptive::HISTORY_SAMPLES - SUBFRAME_SAMPLES..]
                    .copy_from_slice(&subframe.feedback_q0);
                for value in history {
                    include(value);
                }
            }
            assert_eq!(decoder.history_q0, history);
        }
        assert_eq!(hash, 0x780a180e88dab0ce);
    }
    #[test]
    fn adjusted_history_changes_following_prediction_exactly_as_reference() {
        let mut decoder = ExcitationDecoder::default();
        let mut hash = 0xcbf29ce484222325u64;
        let mut history = [0i16; adaptive::HISTORY_SAMPLES];
        let mut include = |value: i16| {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        };
        for frame in 0..512 {
            let mut step = frame * SUBFRAMES;
            let output = decoder.decode_with_history_adjustment(&parameters(frame), |_| {
                let adjustment = if step % 7 == 3 {
                    HistoryAdjustment::Quarter
                } else {
                    HistoryAdjustment::Preserve
                };
                step += 1;
                adjustment
            });
            for (index, subframe) in output.iter().enumerate() {
                for value in subframe
                    .adaptive_q0
                    .into_iter()
                    .chain(subframe.innovation_q13)
                    .chain(subframe.feedback_q0)
                    .chain([
                        subframe.gains.pitch_q14,
                        subframe.gains.code_q1,
                        subframe.pitch.samples() as i16,
                        subframe.pitch.fraction().thirds(),
                    ])
                {
                    include(value);
                }
                if (frame * SUBFRAMES + index) % 7 == 3 {
                    for sample in &mut history {
                        *sample >>= OVERFLOW_RECOVERY_SHIFT;
                    }
                }
                history.copy_within(SUBFRAME_SAMPLES.., 0);
                history[adaptive::HISTORY_SAMPLES - SUBFRAME_SAMPLES..]
                    .copy_from_slice(&subframe.feedback_q0);
                for value in history {
                    include(value);
                }
            }
            assert_eq!(decoder.history_q0, history);
        }
        assert_eq!(hash, 0xe3ce56f219b61a7c);
    }
    #[test]
    fn reset_restores_all_excitation_state() {
        let mut decoder = ExcitationDecoder::default();
        let first = decoder.decode(&parameters(0));
        decoder.decode(&parameters(1));
        decoder.reset();
        assert_eq!(decoder.decode(&parameters(0)), first);
    }
    #[test]
    fn mixed_bad_and_missing_excitation_matches_reference_components_and_recovery() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..4 {
            let mut decoder = ExcitationDecoder::default();
            let mut history = [0i16; adaptive::HISTORY_SAMPLES];
            for frame in 0..512 {
                let bad = frame >= 2
                    && match pattern {
                        0 => frame % 7 == 3,
                        1 => frame % 32 >= 4,
                        2 => frame % 128 < 127,
                        _ => frame < 256,
                    };
                let output = if bad && frame % 2 == 0 {
                    decoder.conceal()
                } else if bad {
                    decoder.decode_bad(&parameters(frame))
                } else {
                    decoder.decode(&parameters(frame))
                };
                for subframe in output {
                    history.copy_within(SUBFRAME_SAMPLES.., 0);
                    history[adaptive::HISTORY_SAMPLES - SUBFRAME_SAMPLES..]
                        .copy_from_slice(&subframe.feedback_q0);
                    for value in subframe
                        .adaptive_q0
                        .into_iter()
                        .chain(subframe.innovation_q13)
                        .chain(subframe.feedback_q0)
                        .chain([
                            subframe.gains.pitch_q14,
                            subframe.gains.code_q1,
                            subframe.pitch.samples() as i16,
                            subframe.pitch.fraction().thirds(),
                        ])
                        .chain(history)
                    {
                        for byte in value.to_le_bytes() {
                            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                        }
                    }
                }
                assert_eq!(decoder.history_q0, history);
            }
            decoder.reset();
            assert_eq!(decoder, ExcitationDecoder::default());
        }
        assert_eq!(hash, 0x25534d4ba85172a5);
    }
}
