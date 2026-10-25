use super::gain_concealment::AttenuationStage;
const MAX_LOSS_STAGE: u8 = 6;
const RECOVERY_STAGE: u8 = 5;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpeechQuality {
    #[default]
    Good,
    Erased,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLoss {
    pub quality: SpeechQuality,
    pub attenuation: AttenuationStage,
    pub previous_frame_erased: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpeechLossState {
    stage: u8,
    previous_quality: SpeechQuality,
}
impl SpeechLossState {
    pub(super) fn resume_noise(&mut self, muted: bool) {
        self.stage = MAX_LOSS_STAGE;
        self.previous_quality = if muted {
            SpeechQuality::Erased
        } else {
            SpeechQuality::Good
        };
    }

    pub(super) fn after_initial_good_speech(&mut self) {
        // TS 26.073 Decoder_amr starts the first speech frame after initial comfort noise at stage five.
        self.stage = RECOVERY_STAGE;
        self.previous_quality = SpeechQuality::Good;
    }

    /// Starts an established continuous-speech interval. Leaving comfort noise requires separate DTX state transitions.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// Advance once per frame and reuse the returned attenuation and recovery flag for all four subframes.
    pub fn advance(&mut self, quality: SpeechQuality) -> FrameLoss {
        let previous_frame_erased = self.previous_quality == SpeechQuality::Erased;
        // TS 26.073 Decoder_amr keeps stage five for the first good frame after stage-six loss.
        self.stage = match quality {
            SpeechQuality::Erased => (self.stage + 1).min(MAX_LOSS_STAGE),
            SpeechQuality::Good if self.stage == MAX_LOSS_STAGE => RECOVERY_STAGE,
            SpeechQuality::Good => 0,
        };
        self.previous_quality = quality;
        FrameLoss {
            quality,
            attenuation: AttenuationStage::try_new(self.stage)
                .expect("speech loss stages remain in zero through six"),
            previous_frame_erased,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_twelve_frame_loss_sequence_matches_established_native_speech_state() {
        const SEQUENCE_FRAMES: u32 = 12;
        let mut hash = 0xcbf29ce484222325u64;
        for sequence in 0..(1u32 << SEQUENCE_FRAMES) {
            let mut state = SpeechLossState::default();
            for frame in 0..SEQUENCE_FRAMES {
                let quality = if sequence & (1 << frame) == 0 {
                    SpeechQuality::Good
                } else {
                    SpeechQuality::Erased
                };
                let output = state.advance(quality);
                assert_eq!(
                    output.attenuation,
                    AttenuationStage::try_new(state.stage).unwrap()
                );
                for value in [
                    i16::from(state.stage),
                    i16::from(output.previous_frame_erased),
                    i16::from(quality == SpeechQuality::Erased),
                ] {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            state.reset();
            assert_eq!(state, SpeechLossState::default());
        }
        assert_eq!(hash, 0xe517ac3967a214ea);
    }
    #[test]
    fn long_loss_recovers_through_stage_five_before_returning_to_zero() {
        let mut state = SpeechLossState::default();
        for _ in 0..1000 {
            state.advance(SpeechQuality::Erased);
        }
        let recovery = state.advance(SpeechQuality::Good);
        assert_eq!(
            recovery.attenuation,
            AttenuationStage::try_new(RECOVERY_STAGE).unwrap()
        );
        assert!(recovery.previous_frame_erased);
        let next = state.advance(SpeechQuality::Good);
        assert_eq!(next.attenuation, AttenuationStage::try_new(0).unwrap());
        assert!(!next.previous_frame_erased);
    }
}
