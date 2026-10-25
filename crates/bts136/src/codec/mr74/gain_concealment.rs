use super::gain::{GainDecoder, Gains};
const HISTORY: usize = 5;
const Q15_SHIFT: u32 = 15;
const PITCH_UNITY_Q14: i16 = 16384;
const INITIAL_PITCH: i16 = 1640;
// TS 26.073 ec_gain_pitch and ec_gain_code attenuation tables are indexed by the bad-frame state.
const PITCH_DOWN_Q15: [i16; 7] = [32767, 32112, 32112, 26214, 9830, 6553, 6553];
const CODE_DOWN_Q15: [i16; 7] = [32767, 32112, 32112, 32112, 32112, 32112, 22937];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttenuationStage(u8);
impl AttenuationStage {
    pub fn try_new(index: u8) -> Option<Self> {
        (usize::from(index) < PITCH_DOWN_Q15.len()).then_some(Self(index))
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GainConcealment {
    pitch_history: [i16; HISTORY],
    code_history: [i16; HISTORY],
    past: Gains,
    last_good: Gains,
}
impl Default for GainConcealment {
    fn default() -> Self {
        Self {
            pitch_history: [INITIAL_PITCH; HISTORY],
            code_history: [1; HISTORY],
            past: Gains {
                pitch_q14: 0,
                code_q1: 0,
            },
            last_good: Gains {
                pitch_q14: PITCH_UNITY_Q14,
                code_q1: 1,
            },
        }
    }
}
impl GainConcealment {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn conceal(&mut self, stage: AttenuationStage, predictor: &mut GainDecoder) -> Gains {
        let stage = usize::from(stage.0);
        let gains = Gains {
            pitch_q14: multiply(
                median(self.pitch_history).min(self.past.pitch_q14),
                PITCH_DOWN_Q15[stage],
            ),
            code_q1: multiply(
                median(self.code_history).min(self.past.code_q1),
                CODE_DOWN_Q15[stage],
            ),
        };
        predictor.conceal_energy();
        self.update(gains);
        gains
    }
    /// The previous-frame erasure flag applies to every subframe of the recovering good frame.
    pub fn update_good(&mut self, mut gains: Gains, previous_frame_erased: bool) -> Gains {
        if previous_frame_erased {
            gains.pitch_q14 = gains.pitch_q14.min(self.last_good.pitch_q14);
            gains.code_q1 = gains.code_q1.min(self.last_good.code_q1);
        }
        self.last_good = gains;
        self.update(gains);
        gains
    }
    fn update(&mut self, gains: Gains) {
        self.past = Gains {
            pitch_q14: gains.pitch_q14.min(PITCH_UNITY_Q14),
            code_q1: gains.code_q1,
        };
        self.pitch_history.rotate_left(1);
        self.pitch_history[HISTORY - 1] = self.past.pitch_q14;
        self.code_history.rotate_left(1);
        self.code_history[HISTORY - 1] = self.past.code_q1;
    }
}
fn multiply(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
fn median(mut history: [i16; HISTORY]) -> i16 {
    history.sort_unstable();
    history[HISTORY / 2]
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::SUBFRAME_SAMPLES;
    #[test]
    fn attenuation_recovery_and_predictor_updates_match_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..4 {
            let mut concealment = GainConcealment::default();
            let mut predictor = GainDecoder::default();
            let mut previous_bad = false;
            for step in 0..4096 {
                let bad = match pattern {
                    0 => step % 7 == 3,
                    1 => step % 32 >= 4,
                    2 => step % 128 < 127,
                    _ => step < 2048,
                };
                let mut innovation = [0; SUBFRAME_SAMPLES];
                innovation[step % SUBFRAME_SAMPLES] = 8191;
                let gains = if bad {
                    concealment.conceal(
                        AttenuationStage::try_new((step % 7) as u8).unwrap(),
                        &mut predictor,
                    )
                } else {
                    let gains = predictor.decode((step % 128) as u16, &innovation).unwrap();
                    concealment.update_good(gains, previous_bad)
                };
                previous_bad = bad;
                for value in [gains.pitch_q14, gains.code_q1]
                    .into_iter()
                    .chain(concealment.pitch_history)
                    .chain([concealment.past.pitch_q14, concealment.last_good.pitch_q14])
                    .chain(concealment.code_history)
                    .chain([concealment.past.code_q1, concealment.last_good.code_q1])
                {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            concealment.reset();
            assert_eq!(concealment, GainConcealment::default());
        }
        assert_eq!(hash, 0xf484b1f8c0c40891);
        assert!(AttenuationStage::try_new(7).is_none());
    }
}
