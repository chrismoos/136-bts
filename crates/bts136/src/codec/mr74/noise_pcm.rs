use super::{
    decoder::{FRAME_SAMPLES, PCM_LOW_BITS_MASK},
    dtx::Decision,
    noise_receiver::{Input, NoiseReceiver, ReceiveError},
    postfilter::Postfilter,
    postprocess::Postprocess,
    synthesis::SynthesisFilter,
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub samples_q0: [i16; FRAME_SAMPLES],
    pub decision: Decision,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoisePcmDecoder {
    receiver: NoiseReceiver,
    synthesis: SynthesisFilter,
    postfilter: Postfilter,
    output: Postprocess,
}
impl NoisePcmDecoder {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 Speech_Decode_Frame applies the MR74 postfilter, high-pass filter and 13-bit truncation to received comfort noise.
    pub fn receive(&mut self, input: Input) -> Result<Frame, ReceiveError> {
        let mut received = self.receiver.receive(input, &mut self.synthesis)?;
        self.postfilter.process(
            &mut received.frame.samples_q0,
            &received.frame.coefficients_q12,
        );
        self.output.process(&mut received.frame.samples_q0);
        Ok(Frame {
            samples_q0: received
                .frame
                .samples_q0
                .map(|sample| sample & !PCM_LOW_BITS_MASK),
            decision: received.decision,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::sid::SidParameters;
    #[test]
    fn complete_received_noise_pcm_matches_native_speech_decode_frame_interface() {
        let mut hash = 0xcbf29ce484222325u64;
        for (pattern, interval) in [8, 32, 64, 128].into_iter().enumerate() {
            let mut decoder = NoisePcmDecoder::default();
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
                for sample in decoder.receive(input).unwrap().samples_q0 {
                    assert_eq!(sample & PCM_LOW_BITS_MASK, 0);
                    for byte in sample.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            decoder.reset();
            assert_eq!(decoder, NoisePcmDecoder::default());
        }
        assert_eq!(hash, 0xd980db16a6050b1f);
    }
    #[test]
    fn reset_restores_initial_noise_pcm_and_all_output_histories() {
        let mut decoder = NoisePcmDecoder::default();
        let first = decoder.receive(Input::NoData).unwrap();
        for _ in 0..100 {
            decoder.receive(Input::NoData).unwrap();
        }
        decoder.reset();
        assert_eq!(decoder.receive(Input::NoData).unwrap(), first);
    }
}
