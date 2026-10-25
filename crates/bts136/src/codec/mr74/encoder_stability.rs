use super::{gain::quantization::GainLimit, spectral::LPC_ORDER};
const HISTORY_GAINS: usize = 7;
const GAIN_AVERAGE_SHIFT: u32 = 3;
const REQUIRED_RESONANT_FRAMES: u8 = 12;
const HIGH_FREQUENCY_GAP: i16 = 1500;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Stability {
    count: u8,
    gains_q11: [i16; HISTORY_GAINS],
}
impl Stability {
    pub(super) fn observe_spectrum(&mut self, lsp: &[i16; LPC_ORDER]) -> bool {
        const UPPER_COSINE: i16 = 32000;
        const MIDDLE_COSINE: i16 = 30500;
        let upper = (3..LPC_ORDER - 2)
            .map(|i| lsp[i].saturating_sub(lsp[i + 1]))
            .min()
            .expect("five high-frequency gaps");
        let lower = (1..3)
            .map(|i| lsp[i].saturating_sub(lsp[i + 1]))
            .min()
            .expect("two low-frequency gaps");
        let threshold = if lsp[1] > UPPER_COSINE {
            600
        } else if lsp[1] > MIDDLE_COSINE {
            800
        } else {
            1100
        };
        if upper < HIGH_FREQUENCY_GAP || lower < threshold {
            self.count = self.count.saturating_add(1).min(REQUIRED_RESONANT_FRAMES);
        } else {
            self.count = 0;
        }
        self.count >= REQUIRED_RESONANT_FRAMES
    }
    pub(super) fn limit(&self, resonant: bool, pitch_gain_q14: i16) -> GainLimit {
        let clipped = GainLimit::Resonance.pitch_q14();
        let sum = self
            .gains_q11
            .iter()
            .fold(pitch_gain_q14 >> GAIN_AVERAGE_SHIFT, |sum, &gain| {
                sum.saturating_add(gain)
            });
        if resonant && pitch_gain_q14 > clipped && sum > clipped {
            GainLimit::Resonance
        } else {
            GainLimit::Unrestricted
        }
    }
    pub(super) fn commit_gain(&mut self, pitch_gain_q14: i16) {
        self.gains_q11.rotate_left(1);
        self.gains_q11[HISTORY_GAINS - 1] = pitch_gain_q14 >> GAIN_AVERAGE_SHIFT;
    }
}
