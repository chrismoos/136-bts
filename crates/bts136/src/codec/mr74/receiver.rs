use super::{
    Parameters,
    decoder::{Decoder, FRAME_SAMPLES, PCM_LOW_BITS_MASK},
    dtx::{Mode, ReceivedFrame},
    noise_receiver::{self, NoiseReceiver, ReceiveError},
    postfilter::Postfilter,
    postprocess::Postprocess,
    sid::SidParameters,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    SpeechGood(Parameters),
    SpeechDegraded(Parameters),
    SpeechBad(Parameters),
    SidFirst,
    SidUpdate(SidParameters),
    SidBad,
    NoData,
    Onset,
}
impl Input {
    fn classification(self) -> ReceivedFrame {
        match self {
            Self::SpeechGood(_) => ReceivedFrame::SpeechGood,
            Self::SpeechDegraded(_) => ReceivedFrame::SpeechDegraded,
            Self::SpeechBad(_) => ReceivedFrame::SpeechBad,
            Self::SidFirst => ReceivedFrame::SidFirst,
            Self::SidUpdate(_) => ReceivedFrame::SidUpdate,
            Self::SidBad => ReceivedFrame::SidBad,
            Self::NoData => ReceivedFrame::NoData,
            Self::Onset => ReceivedFrame::Onset,
        }
    }
    fn noise_input(self) -> Option<noise_receiver::Input> {
        match self {
            Self::SpeechGood(_) | Self::SpeechDegraded(_) => None,
            Self::SpeechBad(_) => Some(noise_receiver::Input::SpeechBad),
            Self::SidFirst => Some(noise_receiver::Input::SidFirst),
            Self::SidUpdate(parameters) => Some(noise_receiver::Input::SidUpdate(parameters)),
            Self::SidBad => Some(noise_receiver::Input::SidBad),
            Self::NoData => Some(noise_receiver::Input::NoData),
            Self::Onset => Some(noise_receiver::Input::Onset),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub samples_q0: [i16; FRAME_SAMPLES],
    pub mode: Mode,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Receiver {
    speech: Decoder,
    noise: NoiseReceiver,
    postfilter: Postfilter,
    output: Postprocess,
}
impl Receiver {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn receive(&mut self, input: Input) -> Result<Frame, ReceiveError> {
        if let Some(noise_input) = input.noise_input() {
            match self.noise.receive(noise_input, &mut self.speech.synthesis) {
                Ok(mut received) => {
                    self.postfilter.process(
                        &mut received.frame.samples_q0,
                        &received.frame.coefficients_q12,
                    );
                    self.output.process(&mut received.frame.samples_q0);
                    return Ok(Frame {
                        samples_q0: received
                            .frame
                            .samples_q0
                            .map(|sample| sample & !PCM_LOW_BITS_MASK),
                        mode: received.decision.mode,
                    });
                }
                Err(ReceiveError::SpeechRequired) => {}
                Err(error) => return Err(error),
            }
        }
        let previous = self.noise.mode();
        if previous != Mode::Speech {
            let (spectrum, energy) = self.noise.resume_state();
            self.speech
                .resume_noise(spectrum, energy, previous == Mode::Muted);
        }
        let mut frame = match input {
            Input::SpeechGood(parameters) | Input::SpeechDegraded(parameters) => {
                self.speech.decode_good(&parameters)
            }
            Input::SpeechBad(parameters) => self
                .speech
                .decode_bad(&parameters)
                .expect("speech is established before bad-frame dispatch"),
            Input::NoData | Input::Onset => self
                .speech
                .conceal()
                .expect("speech is established before missing-frame dispatch"),
            _ => unreachable!("SID classifications always select comfort noise"),
        };
        self.noise.observe_speech(
            input.classification(),
            &frame.spectrum.lsf_q15,
            &frame.samples_q0,
        )?;
        self.postfilter
            .process(&mut frame.samples_q0, &frame.coefficients_q12);
        self.output.process(&mut frame.samples_q0);
        Ok(Frame {
            samples_q0: frame.samples_q0.map(|sample| sample & !PCM_LOW_BITS_MASK),
            mode: Mode::Speech,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::{LSF_INDICES, PARAMETER_COUNT, SUBFRAMES};
    fn speech_parameters(frame: usize) -> Parameters {
        let mut values = [0; PARAMETER_COUNT];
        values[..LSF_INDICES].copy_from_slice(&[
            (frame % 256) as u16,
            ((frame * 73 + frame / 512) % 512) as u16,
            ((frame * 197 + frame / 256) % 512) as u16,
        ]);
        for sf in 0..SUBFRAMES {
            let step = frame * SUBFRAMES + sf;
            let pitch = if sf % 2 == 1 {
                (frame * 7 + sf) % 32
            } else if sf == 0 {
                frame % 256
            } else {
                (frame * 53 + 197) % 256
            };
            let start = LSF_INDICES + sf * 4;
            values[start..start + 4].copy_from_slice(&[
                pitch as u16,
                ((step * 73) % 8192) as u16,
                (step % 16) as u16,
                (step % 128) as u16,
            ]);
        }
        Parameters::try_new(values).unwrap()
    }
    fn input(frame: usize, pattern: usize) -> Input {
        let cycle = [64, 96, 128, 256][pattern];
        let phase = frame % cycle;
        let speech = if pattern == 1 {
            phase >= cycle / 2
        } else {
            phase < cycle / 2
        };
        let start = if speech {
            if pattern == 1 { cycle / 2 } else { 0 }
        } else if pattern == 1 {
            0
        } else {
            cycle / 2
        };
        if speech {
            if phase == start {
                return Input::SpeechGood(speech_parameters(frame));
            }
            if frame % 11 == 6 {
                return Input::NoData;
            }
            if frame % 17 == 7 {
                return Input::SpeechBad(speech_parameters(frame));
            }
            if frame % 23 == 9 {
                return Input::Onset;
            }
            if frame % 19 == 10 {
                return Input::SpeechDegraded(speech_parameters(frame));
            }
            return Input::SpeechGood(speech_parameters(frame));
        }
        let offset = phase - start;
        if offset == 0 {
            return Input::SidFirst;
        }
        if offset == if pattern == 3 { 80 } else { 1 }
            || offset % if pattern == 3 { 128 } else { 8 } == 3
        {
            return Input::SidUpdate(
                SidParameters::try_new([
                    (frame % 8) as u16,
                    ((frame * 17) % 256) as u16,
                    ((frame * 73) % 512) as u16,
                    ((frame * 197) % 512) as u16,
                    (frame % 64) as u16,
                ])
                .unwrap(),
            );
        }
        if frame % 17 == 7 {
            return Input::SidBad;
        }
        if frame % 29 == 11 {
            return Input::Onset;
        }
        if frame % 23 == 9 {
            return Input::SpeechBad(speech_parameters(frame));
        }
        Input::NoData
    }
    #[test]
    fn unified_speech_noise_handoff_and_pcm_match_complete_native_interface() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..4 {
            let mut receiver = Receiver::default();
            for frame in 0..512 {
                let output = receiver.receive(input(frame, pattern)).unwrap();
                let mode = match output.mode {
                    Mode::Speech => 0,
                    Mode::ComfortNoise => 1,
                    Mode::Muted => 2,
                };
                for value in [mode].into_iter().chain(output.samples_q0) {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            receiver.reset();
            assert_eq!(receiver, Receiver::default());
        }
        assert_eq!(hash, 0x8e18d3d04b2c502a);
    }
}
