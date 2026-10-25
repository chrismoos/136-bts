use super::{
    FRAME_SAMPLES, SUBFRAME_SAMPLES, VselpParameters,
    concealment::BadFrameMasker,
    excitation::{HistoryError, LongTermHistory},
    gains::{GainError, PitchPrediction},
    synthesis::{SynthesisError, SynthesisFilter},
};
use crate::phy::speech::{CrcStatus, SpeechCandidate};
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Gain {
        subframe: usize,
        error: GainError,
    },
    History {
        subframe: usize,
        error: HistoryError,
    },
    Synthesis {
        subframe: usize,
        error: SynthesisError,
    },
}
impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP frame synthesis failed: {self:?}")
    }
}
impl Error for DecodeError {}

#[derive(Debug, Clone, PartialEq)]
pub struct Decoder {
    previous: VselpParameters,
    long_term: LongTermHistory,
    short_term: SynthesisFilter,
    masker: BadFrameMasker,
}
impl Decoder {
    /// Seeds interpolation and masking from the preceding trusted frame with zero synthesis history.
    pub fn new(previous: VselpParameters) -> Self {
        Self {
            masker: BadFrameMasker::new(previous.clone()),
            previous,
            long_term: LongTermHistory::new(),
            short_term: SynthesisFilter::new(),
        }
    }
    pub fn reset(&mut self, previous: VselpParameters) {
        *self = Self::new(previous);
    }
    /// Returns unclipped samples relative to maximum PCM amplitude, before optional spectral postfiltering.
    /// Treats parameters as CRC-valid. Use decode_candidate to apply a channel decoder's CRC result.
    pub fn decode(&mut self, frame: &VselpParameters) -> Result<[f64; FRAME_SAMPLES], DecodeError> {
        self.decode_with_crc(frame, CrcStatus::Valid)
    }
    pub fn decode_candidate(
        &mut self,
        candidate: &SpeechCandidate,
    ) -> Result<[f64; FRAME_SAMPLES], DecodeError> {
        self.decode_with_crc(
            &VselpParameters::from_speech_bits(&candidate.frame.bits),
            candidate.crc,
        )
    }
    /// Reuses last trusted excitation for an unavailable frame and advances bad-frame masking by one frame.
    pub fn conceal_missing_frame(&mut self) -> Result<[f64; FRAME_SAMPLES], DecodeError> {
        self.decode_with_crc(&self.masker.trusted_frame().clone(), CrcStatus::Failed)
    }
    fn decode_with_crc(
        &mut self,
        frame: &VselpParameters,
        crc: CrcStatus,
    ) -> Result<[f64; FRAME_SAMPLES], DecodeError> {
        let mut next = self.clone();
        let masked = next.masker.apply(frame, crc);
        let output = next.synthesize(&masked)?;
        *self = next;
        Ok(output)
    }
    // IS-54-B §§2.2.2.2.4.4.5–2.2.2.2.4.4.7 store combined excitation before short-term filtering.
    fn synthesize(&mut self, frame: &VselpParameters) -> Result<[f64; FRAME_SAMPLES], DecodeError> {
        let predictors = frame.subframe_predictors(&self.previous);
        let mut output = [0.0; FRAME_SAMPLES];
        for (subframe, (parameters, predictor)) in
            frame.subframes().iter().zip(predictors).enumerate()
        {
            let adaptive = self.long_term.excitation(parameters);
            let pitch = if parameters.pitch_lag_samples().is_some() {
                PitchPrediction::Enabled
            } else {
                PitchPrediction::Disabled
            };
            let combined = parameters
                .gain_parameters()
                .reconstruct(
                    &predictor,
                    pitch,
                    &adaptive,
                    &parameters.codebook_excitation(),
                )
                .map_err(|error| DecodeError::Gain { subframe, error })?;
            self.long_term
                .commit(&combined.samples)
                .map_err(|error| DecodeError::History { subframe, error })?;
            let samples = self
                .short_term
                .process(&predictor.coefficients, &combined.samples)
                .map_err(|error| DecodeError::Synthesis { subframe, error })?;
            let start = subframe * SUBFRAME_SAMPLES;
            output[start..start + SUBFRAME_SAMPLES].copy_from_slice(&samples);
        }
        self.previous = frame.clone();
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vselp::{LPC_COEFFICIENTS, SUBFRAMES, VselpSubframe};
    fn frame() -> VselpParameters {
        VselpParameters::try_new(
            20,
            [32, 16, 16, 8, 8, 4, 4, 4, 4, 2],
            [VselpSubframe::try_new(1, 37, 54, 170).unwrap(); SUBFRAMES],
        )
        .unwrap()
    }
    #[test]
    fn reset_restores_both_filter_histories_and_preceding_frame_interpolation() {
        let frame = frame();
        let mut decoder = Decoder::new(frame.clone());
        let first = decoder.decode(&frame).unwrap();
        let second = decoder.decode(&frame).unwrap();
        assert_ne!(first, second);
        decoder.reset(frame.clone());
        assert_eq!(decoder.decode(&frame).unwrap(), first);
        assert_eq!(decoder.decode(&frame).unwrap(), second);
    }
    #[test]
    fn frame_failure_does_not_commit_any_partial_subframe_or_predictor_state() {
        let frame = frame();
        let mut decoder = Decoder::new(frame.clone());
        decoder.decode(&frame).unwrap();
        decoder
            .short_term
            .process(&[0.0; LPC_COEFFICIENTS], &[f64::MAX; SUBFRAME_SAMPLES])
            .unwrap();
        let before = decoder.clone();
        assert!(matches!(
            decoder.decode(&frame),
            Err(DecodeError::Synthesis { .. })
        ));
        assert_eq!(decoder, before);
        assert!(matches!(
            decoder.conceal_missing_frame(),
            Err(DecodeError::Synthesis { .. })
        ));
        assert_eq!(decoder, before);
        let candidate = SpeechCandidate {
            crc: CrcStatus::Failed,
            frame: crate::phy::speech::SpeechFrame {
                bits: frame.speech_bits(),
                corrected_coded_bits: 0,
            },
        };
        assert!(matches!(
            decoder.decode_candidate(&candidate),
            Err(DecodeError::Synthesis { .. })
        ));
        assert_eq!(decoder, before);
    }
    #[test]
    fn silence_with_zero_history_produces_exact_zero_samples() {
        let silent = VselpParameters::default();
        let mut decoder = Decoder::new(silent.clone());
        for _ in 0..3 {
            assert_eq!(decoder.decode(&silent).unwrap(), [0.0; FRAME_SAMPLES]);
        }
    }
    #[test]
    fn missing_frames_repeat_trusted_excitation_and_obey_attenuation_and_two_good_frame_recovery() {
        let seed = frame();
        let mut decoder = Decoder::new(seed.clone());
        let mut expected = Decoder::new(seed.clone());
        assert_eq!(
            decoder.decode(&seed).unwrap(),
            expected.decode(&seed).unwrap()
        );
        for r0 in [20, 20, 18, 16, 14, 0, 0] {
            let masked = VselpParameters::try_new(r0, *seed.lpc(), *seed.subframes()).unwrap();
            assert_eq!(
                decoder.conceal_missing_frame().unwrap(),
                expected.decode(&masked).unwrap()
            );
        }
        let recovery = VselpParameters::try_new(
            10,
            [30, 15, 14, 7, 7, 3, 3, 3, 3, 1],
            [VselpSubframe::try_new(21, 54, 37, 60).unwrap(); SUBFRAMES],
        )
        .unwrap();
        let first_good = VselpParameters::try_new(0, *seed.lpc(), *recovery.subframes()).unwrap();
        assert_eq!(
            decoder.decode(&recovery).unwrap(),
            expected.decode(&first_good).unwrap()
        );
        assert_eq!(
            decoder.decode(&recovery).unwrap(),
            expected.decode(&recovery).unwrap()
        );
    }
}
