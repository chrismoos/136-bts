use super::{
    decoder::FRAME_SAMPLES,
    dtx::{Decision, Mode, ReceivedFrame, ReceiverState, SidData},
    noise_history::NoiseHistory,
    noise_synthesis::{Frame, NoiseSynthesis},
    sid::SidParameters,
    sid_interpolation::Transition,
    spectral::{ConversionError, LPC_ORDER},
    synthesis::SynthesisFilter,
};
use std::{error::Error, fmt};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    SidFirst,
    SidUpdate(SidParameters),
    SidBad,
    NoData,
    Onset,
    SpeechBad,
}
impl Input {
    fn classification(self) -> ReceivedFrame {
        match self {
            Self::SidFirst => ReceivedFrame::SidFirst,
            Self::SidUpdate(_) => ReceivedFrame::SidUpdate,
            Self::SidBad => ReceivedFrame::SidBad,
            Self::NoData => ReceivedFrame::NoData,
            Self::Onset => ReceivedFrame::Onset,
            Self::SpeechBad => ReceivedFrame::SpeechBad,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveError {
    SpeechRequired,
    NoiseRequired,
    Spectrum(ConversionError),
}
impl fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoiseRequired => f.write_str("received frame requires comfort-noise synthesis"),
            Self::SpeechRequired => f.write_str("received frame requires speech concealment"),
            Self::Spectrum(error) => fmt::Display::fmt(error, f),
        }
    }
}
impl Error for ReceiveError {}
impl From<ConversionError> for ReceiveError {
    fn from(error: ConversionError) -> Self {
        Self::Spectrum(error)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub decision: Decision,
    pub frame: Frame,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoiseReceiver {
    receiver: ReceiverState,
    history: NoiseHistory,
    noise: NoiseSynthesis,
    last_spectrum: Option<super::spectral::Spectrum>,
    prediction_energy_q10: i16,
}
impl Default for NoiseReceiver {
    fn default() -> Self {
        Self {
            receiver: ReceiverState::default(),
            history: NoiseHistory::default(),
            noise: NoiseSynthesis::default(),
            last_spectrum: None,
            prediction_energy_q10: super::gain::INITIAL_ENERGY_Q10,
        }
    }
}
impl NoiseReceiver {
    pub(super) fn mode(&self) -> Mode {
        self.receiver.current_mode()
    }
    pub(super) fn resume_state(&self) -> (Option<super::spectral::Spectrum>, i16) {
        (self.last_spectrum, self.prediction_energy_q10)
    }
    pub(super) fn observe_speech(
        &mut self,
        classification: ReceivedFrame,
        frequencies: &[i16; LPC_ORDER],
        samples: &[i16; FRAME_SAMPLES],
    ) -> Result<(), ReceiveError> {
        let mut staged = self.clone();
        if staged.receiver.advance(classification).mode != Mode::Speech {
            return Err(ReceiveError::NoiseRequired);
        }
        staged.history.record(frequencies, samples)?;
        *self = staged;
        Ok(())
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn observe_good_speech(
        &mut self,
        frequencies: &[i16; LPC_ORDER],
        samples: &[i16; FRAME_SAMPLES],
    ) -> Result<(), ConversionError> {
        self.history.record(frequencies, samples)?;
        self.receiver.advance(ReceivedFrame::SpeechGood);
        Ok(())
    }
    /// TS 26.073 rx_dtx_handler/dtx_dec apply descriptors before rendering, then commit muting and backward-update age resets.
    pub fn receive(
        &mut self,
        input: Input,
        synthesis: &mut SynthesisFilter,
    ) -> Result<Output, ReceiveError> {
        let mut staged = self.clone();
        let decision = staged.receiver.advance(input.classification());
        if decision.mode == Mode::Speech {
            return Err(ReceiveError::SpeechRequired);
        }
        let backward = decision.hangover_added && decision.sid_data != SidData::None;
        if backward {
            let descriptor = staged.history.analyze();
            staged.noise.apply_backward(&descriptor)?;
        }
        match input {
            Input::SidUpdate(parameters) => {
                let transition = if !staged.receiver.has_parameters()
                    || decision.previous_mode == Mode::Speech
                {
                    Transition::Immediate
                } else {
                    Transition::Interpolate
                };
                staged
                    .noise
                    .apply_sid(&parameters, staged.receiver.sid_age_frames(), transition);
                staged.receiver.parameters_applied();
            }
            Input::SidFirst | Input::SidBad => staged.noise.hold_sid(),
            _ => {}
        }
        if decision.sid_data != SidData::None {
            staged.prediction_energy_q10 = staged.noise.prediction_energy_q10();
        }
        let age = staged.receiver.sid_age_frames();
        let frame = staged.noise.render(age, synthesis)?;
        if decision.mode == Mode::Muted {
            staged.noise.mute_after_render(age);
            staged.receiver.noise_muted();
        }
        if backward {
            staged.receiver.backward_parameters_applied();
        }
        staged.last_spectrum = Some(frame.spectrum);
        *self = staged;
        Ok(Output { decision, frame })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_noise_reception_matches_native_handler_and_dtx_synthesis() {
        let mut hash = 0xcbf29ce484222325u64;
        for (pattern, interval) in [8, 32, 64, 128].into_iter().enumerate() {
            let mut decoder = NoiseReceiver::default();
            let mut synthesis = SynthesisFilter::default();
            for frame in 0..512 {
                let input = if frame == 0 && pattern == 0 {
                    Input::SidFirst
                } else if frame % interval == 0 && (pattern != 3 || frame > 0) {
                    Input::SidUpdate(
                        SidParameters::try_new([
                            (frame % 8) as u16,
                            ((frame * 17) % 256) as u16,
                            ((frame * 73) % 512) as u16,
                            ((frame * 197) % 512) as u16,
                            (frame % 64) as u16,
                        ])
                        .unwrap(),
                    )
                } else if frame % 17 == 7 {
                    Input::SidBad
                } else if frame % 29 == 11 {
                    Input::Onset
                } else if frame % 23 == 9 {
                    Input::SpeechBad
                } else if frame % 37 == 2 {
                    Input::SidFirst
                } else {
                    Input::NoData
                };
                let output = decoder.receive(input, &mut synthesis).unwrap();
                let mode = match output.decision.mode {
                    Mode::Speech => 0,
                    Mode::ComfortNoise => 1,
                    Mode::Muted => 2,
                };
                for value in [mode]
                    .into_iter()
                    .chain(output.frame.samples_q0)
                    .chain(output.frame.coefficients_q12.into_iter().flatten())
                    .chain(output.frame.spectrum.lsf_q15)
                {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            decoder.reset();
            assert_eq!(decoder, NoiseReceiver::default());
        }
        assert_eq!(hash, 0xcf9242ac3b02d340);
    }
    #[test]
    fn speech_dispatch_preserves_noise_state_and_filter_history() {
        let mut decoder = NoiseReceiver::default();
        let mut synthesis = SynthesisFilter::default();
        decoder
            .observe_good_speech(
                &super::super::spectral::INITIAL_LSF_Q15,
                &[0; FRAME_SAMPLES],
            )
            .unwrap();
        let before = decoder.clone();
        let before_filter = synthesis.clone();
        assert_eq!(
            decoder.receive(Input::NoData, &mut synthesis),
            Err(ReceiveError::SpeechRequired)
        );
        assert_eq!(decoder, before);
        assert_eq!(synthesis, before_filter);
    }
}
