use super::{
    Parameters, SUBFRAME_SAMPLES, SUBFRAMES, energy,
    excitation::{ExcitationDecoder, HistoryAdjustment},
    lpc::{COEFFICIENTS, LpcDecoder},
    synthesis::SynthesisFilter,
};
use super::{frame_loss::SpeechQuality, missing_parameters::MissingParameters};
use std::{error::Error, fmt};
pub const FRAME_SAMPLES: usize = SUBFRAMES * SUBFRAME_SAMPLES;
const SHARP_MAX_Q14: i16 = 13017;
const ENHANCEMENT_THRESHOLD_Q15: i16 = 16384;
const Q15_SHIFT: u32 = 15;
const WORD_SHIFT: u32 = 16;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
const OVERFLOW_RECOVERY_SHIFT: u32 = 2;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Samples precede the reference speech postfilter and output high-pass filter.
    pub samples_q0: [i16; FRAME_SAMPLES],
    pub coefficients_q12: [[i16; COEFFICIENTS]; SUBFRAMES],
    pub spectrum: super::spectral::Spectrum,
    pub overflow_retries: [bool; SUBFRAMES],
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SpeechState {
    #[default]
    AwaitingSpeech,
    Active,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decoder {
    lpc: LpcDecoder,
    excitation: ExcitationDecoder,
    pub(super) synthesis: SynthesisFilter,
    speech_state: SpeechState,
    missing: MissingParameters,
}
impl Decoder {
    pub(super) fn resume_noise(
        &mut self,
        spectrum: Option<super::spectral::Spectrum>,
        energy_q10: i16,
        muted: bool,
    ) {
        if let Some(spectrum) = spectrum {
            self.lpc.resume_noise_spectrum(spectrum);
        }
        self.excitation.resume_noise(energy_q10, muted);
        self.missing.reset();
        self.speech_state = SpeechState::Active;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn decode_good(&mut self, parameters: &Parameters) -> Frame {
        let frame = self.decode_quality(parameters, SpeechQuality::Good);
        if self.speech_state == SpeechState::AwaitingSpeech {
            self.excitation.after_initial_good_speech();
            self.speech_state = SpeechState::Active;
        }
        frame
    }
    /// Initial erasures require comfort-noise handling. This interface accepts erasures after good speech begins.
    pub fn decode_bad(&mut self, parameters: &Parameters) -> Result<Frame, SpeechNotEstablished> {
        if self.speech_state == SpeechState::AwaitingSpeech {
            return Err(SpeechNotEstablished);
        }
        Ok(self.decode_quality(parameters, SpeechQuality::Erased))
    }
    pub fn conceal(&mut self) -> Result<Frame, SpeechNotEstablished> {
        if self.speech_state == SpeechState::AwaitingSpeech {
            return Err(SpeechNotEstablished);
        }
        let parameters = self.missing.generate();
        self.decode_bad(&parameters)
    }
    fn decode_quality(&mut self, parameters: &Parameters, quality: SpeechQuality) -> Frame {
        let spectral_frame = match quality {
            SpeechQuality::Good => self.lpc.decode(parameters),
            SpeechQuality::Erased => self.lpc.conceal(),
        };
        let coefficients_q12 = spectral_frame.coefficients_q12;
        let mut samples_q0 = [0; FRAME_SAMPLES];
        let mut overflow_retries = [false; SUBFRAMES];
        let mut index = 0;
        self.excitation
            .decode_quality(parameters, quality, |subframe| {
                let sharpening = subframe
                    .gains
                    .pitch_q14
                    .min(SHARP_MAX_Q14)
                    .saturating_mul(2);
                let mut enhanced = subframe.feedback_q0;
                // TS 26.073 Decoder_amr applies pitch enhancement and agc2 above this Q15 threshold. MR74 bypasses phase dispersion.
                if sharpening > ENHANCEMENT_THRESHOLD_Q15 {
                    for (sample, output) in enhanced.iter_mut().enumerate() {
                        let adaptive = ((i32::from(subframe.adaptive_q0[sample])
                            * i32::from(sharpening))
                            >> Q15_SHIFT) as i16;
                        let contribution = ((i32::from(adaptive)
                            * i32::from(subframe.gains.pitch_q14))
                        .saturating_mul(2)
                        .saturating_add(ROUND_BIAS)
                            >> WORD_SHIFT) as i16;
                        *output = output.saturating_add(contribution);
                    }
                    energy::match_energy(&subframe.feedback_q0, &mut enhanced);
                }
                let overflow = self
                    .synthesis
                    .preview(&coefficients_q12[index], &enhanced)
                    .overflowed;
                if overflow {
                    enhanced = subframe
                        .feedback_q0
                        .map(|sample| sample >> OVERFLOW_RECOVERY_SHIFT);
                }
                let output = self.synthesis.filter(&coefficients_q12[index], &enhanced);
                samples_q0[index * SUBFRAME_SAMPLES..(index + 1) * SUBFRAME_SAMPLES]
                    .copy_from_slice(&output.samples_q0);
                overflow_retries[index] = overflow;
                index += 1;
                if overflow {
                    HistoryAdjustment::Quarter
                } else {
                    HistoryAdjustment::Preserve
                }
            });
        Frame {
            samples_q0,
            coefficients_q12,
            spectrum: spectral_frame.spectrum,
            overflow_retries,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechNotEstablished;
impl fmt::Display for SpeechNotEstablished {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MR74 good speech must begin before erased frames can be decoded")
    }
}
impl Error for SpeechNotEstablished {}
pub(super) const PCM_LOW_BITS_MASK: i16 = 7;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PcmDecoder {
    synthesis: Decoder,
    postfilter: super::postfilter::Postfilter,
    output: super::postprocess::Postprocess,
}
impl PcmDecoder {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn decode_good(&mut self, parameters: &Parameters) -> [i16; FRAME_SAMPLES] {
        let frame = self.synthesis.decode_good(parameters);
        self.output_frame(frame)
    }
    pub fn decode_bad(
        &mut self,
        parameters: &Parameters,
    ) -> Result<[i16; FRAME_SAMPLES], SpeechNotEstablished> {
        let frame = self.synthesis.decode_bad(parameters)?;
        Ok(self.output_frame(frame))
    }
    pub fn conceal(&mut self) -> Result<[i16; FRAME_SAMPLES], SpeechNotEstablished> {
        let frame = self.synthesis.conceal()?;
        Ok(self.output_frame(frame))
    }
    // TS 26.073 Speech_Decode_Frame clears the lowest three bits after both output filters.
    fn output_frame(&mut self, mut frame: Frame) -> [i16; FRAME_SAMPLES] {
        self.postfilter
            .process(&mut frame.samples_q0, &frame.coefficients_q12);
        self.output.process(&mut frame.samples_q0);
        frame.samples_q0.map(|sample| sample & !PCM_LOW_BITS_MASK)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn parameters(frame: usize) -> Parameters {
        let mut values = [0; super::super::PARAMETER_COUNT];
        values[..super::super::LSF_INDICES].copy_from_slice(&[
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
    fn good_frame_synthesis_matches_complete_reference_decoder() {
        let mut decoder = Decoder::default();
        let mut hash = 0xcbf29ce484222325u64;
        let mut retries = 0;
        for frame in 0..512 {
            let decoded = decoder.decode_good(&parameters(frame));
            retries += decoded
                .overflow_retries
                .into_iter()
                .filter(|retry| *retry)
                .count();
            for value in decoded.samples_q0 {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        assert_eq!(hash, 0x693e92860f030ad6);
        assert!(retries > 0);
    }
    #[test]
    fn reset_restores_every_good_frame_decoder_state() {
        let mut decoder = Decoder::default();
        let first = decoder.decode_good(&parameters(0));
        decoder.decode_good(&parameters(1));
        decoder.reset();
        assert_eq!(decoder.decode_good(&parameters(0)), first);
    }
    #[test]
    fn complete_good_frame_pcm_matches_reference_postfilter_and_output_processing() {
        let mut decoder = PcmDecoder::default();
        let mut hash = 0xcbf29ce484222325u64;
        let first = decoder.decode_good(&parameters(0));
        decoder.reset();
        for frame in 0..512 {
            for value in decoder.decode_good(&parameters(frame)) {
                assert_eq!(value & PCM_LOW_BITS_MASK, 0);
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        assert_eq!(hash, 0x44cdf18cdb1c377e);
        decoder.reset();
        assert_eq!(decoder.decode_good(&parameters(0)), first);
    }
    #[test]
    fn mixed_erased_and_missing_pcm_matches_complete_reference_interface() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..4 {
            let mut decoder = PcmDecoder::default();
            for frame in 0..512 {
                let bad = frame > 0
                    && match pattern {
                        0 => frame % 7 == 3,
                        1 => frame % 32 >= 4,
                        2 => frame % 128 < 127,
                        _ => frame < 256,
                    };
                let samples = if bad && frame % 2 == 0 {
                    decoder.conceal().unwrap()
                } else if bad {
                    decoder.decode_bad(&parameters(frame)).unwrap()
                } else {
                    decoder.decode_good(&parameters(frame))
                };
                for value in samples {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            decoder.reset();
            assert_eq!(decoder, PcmDecoder::default());
        }
        assert_eq!(hash, 0xf93e59639930c79f);
    }
    #[test]
    fn initial_erasure_rejection_preserves_all_decoder_histories() {
        let mut decoder = PcmDecoder::default();
        let before = decoder.clone();
        assert_eq!(decoder.conceal(), Err(SpeechNotEstablished));
        assert_eq!(decoder, before);
        assert_eq!(
            decoder.decode_bad(&parameters(0)),
            Err(SpeechNotEstablished)
        );
        assert_eq!(decoder, before);
        decoder.decode_good(&parameters(0));
        assert!(decoder.conceal().is_ok());
    }
}
