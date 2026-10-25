use super::{
    sid::SidParameters,
    spectral::{LPC_ORDER, SpectralDecoder},
};
const INITIAL_LOG_ENERGY_Q11: i16 = 3500;
const INITIAL_PERIOD_INVERSE_Q15: i16 = 8192;
const MIN_PERIOD_INVERSE_Q15: i16 = 16384;
const MUTE_FALLBACK_FRAMES: i16 = 8;
const MUTE_ENERGY_STEP_Q11: i16 = 256;
const MIN_PERIOD_FRAMES: i16 = 2;
const MAX_PERIOD_FRAMES: u16 = 32;
const Q10_ONE: i16 = 1024;
const Q14_ONE: i16 = 16384;
const PERIOD_SHIFT: u32 = 10;
const WEIGHT_SCALE_SHIFT: u32 = 4;
const Q15_SHIFT: u32 = 15;
// TS 26.073 dtx_dec_reset uses lsp_init_data and log energy 3500 for initial noise.
const INITIAL_LSP_Q15: [i16; LPC_ORDER] = [
    30000, 26000, 21000, 15000, 8000, 0, -8000, -15000, -21000, -26000,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Immediate,
    Interpolate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterpolatedSid {
    pub lsp_q15: [i16; LPC_ORDER],
    pub log_energy_q26: i32,
    pub weight_q14: i16,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidInterpolation {
    current_lsp_q15: [i16; LPC_ORDER],
    previous_lsp_q15: [i16; LPC_ORDER],
    current_log_energy_q11: i16,
    previous_log_energy_q11: i16,
    period_inverse_q15: i16,
}
impl Default for SidInterpolation {
    fn default() -> Self {
        Self {
            current_lsp_q15: INITIAL_LSP_Q15,
            previous_lsp_q15: INITIAL_LSP_Q15,
            current_log_energy_q11: INITIAL_LOG_ENERGY_Q11,
            previous_log_energy_q11: INITIAL_LOG_ENERGY_Q11,
            period_inverse_q15: INITIAL_PERIOD_INVERSE_Q15,
        }
    }
}
impl SidInterpolation {
    pub(super) fn current_log_energy_q11(&self) -> i16 {
        self.current_log_energy_q11
    }

    pub(super) fn replace_backward_descriptor(
        &mut self,
        spectrum: &super::spectral::Spectrum,
        log_energy_q11: i16,
    ) {
        self.current_lsp_q15 = spectrum.lsp_q15;
        self.current_log_energy_q11 = log_energy_q11;
        self.hold_current();
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 dtx_dec disables interpolation at startup and immediately after speech.
    pub fn apply(
        &mut self,
        parameters: &SidParameters,
        interval_frames: u16,
        transition: Transition,
    ) {
        self.hold_current();
        self.current_lsp_q15 = SpectralDecoder::default()
            .decode_reference_sid(parameters)
            .lsp_q15;
        self.current_log_energy_q11 = parameters.log_energy_q11();
        let interval = interval_frames.min(MAX_PERIOD_FRAMES) as i16;
        self.period_inverse_q15 = if interval < MIN_PERIOD_FRAMES {
            MIN_PERIOD_INVERSE_Q15
        } else {
            let denominator = saturating_shift(interval, PERIOD_SHIFT);
            ((i32::from(Q10_ONE) << Q15_SHIFT) / i32::from(denominator)).min(i32::from(i16::MAX))
                as i16
        };
        if transition == Transition::Immediate {
            self.hold_current();
        }
    }
    /// SID receipt without valid data still shifts the current reference parameters to the old interpolation endpoint.
    pub fn hold_current(&mut self) {
        self.previous_lsp_q15 = self.current_lsp_q15;
        self.previous_log_energy_q11 = self.current_log_energy_q11;
    }
    /// TS 26.073 dtx_dec applies muting after synthesis and resets the caller's SID age to zero.
    pub fn mute_after_render(&mut self, since_last_sid_frames: u16) {
        let interval = since_last_sid_frames.min(MAX_PERIOD_FRAMES) as i16;
        let interval = if interval == 0 {
            MUTE_FALLBACK_FRAMES
        } else {
            interval
        };
        let denominator = saturating_shift(interval, PERIOD_SHIFT);
        self.period_inverse_q15 = ((i32::from(Q10_ONE) << Q15_SHIFT) / i32::from(denominator))
            .min(i32::from(i16::MAX)) as i16;
        self.hold_current();
        self.current_log_energy_q11 = self
            .current_log_energy_q11
            .saturating_sub(MUTE_ENERGY_STEP_Q11);
    }
    pub fn at(&self, since_last_sid_frames: u16) -> InterpolatedSid {
        let age = since_last_sid_frames.min(i16::MAX as u16) as i16;
        let numerator = saturating_shift(age.saturating_add(1), PERIOD_SHIFT);
        let factor = multiply_q15(numerator, self.period_inverse_q15).min(Q10_ONE);
        let weight_q14 = saturating_shift(factor, WEIGHT_SCALE_SHIFT);
        let complement = Q14_ONE - weight_q14;
        let lsp_q15 = std::array::from_fn(|index| {
            multiply_q15(weight_q14, self.current_lsp_q15[index])
                .saturating_add(multiply_q15(complement, self.previous_lsp_q15[index]))
                .saturating_mul(2)
        });
        let log_energy_q26 = product(weight_q14, self.current_log_energy_q11)
            .saturating_add(product(complement, self.previous_log_energy_q11));
        InterpolatedSid {
            lsp_q15,
            log_energy_q26,
            weight_q14,
        }
    }
}
fn saturating_shift(value: i16, shift: u32) -> i16 {
    (i32::from(value) << shift).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
fn multiply_q15(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spectra_energy_and_period_interpolation_match_reference_arithmetic() {
        let mut state = SidInterpolation::default();
        let mut hash = 0xcbf29ce484222325u64;
        let mut include = |bytes: &[u8]| {
            for &byte in bytes {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        };
        for step in 0..1024 {
            let parameters = SidParameters::try_new([
                (step % 8) as u16,
                ((step * 17) % 256) as u16,
                ((step * 73) % 512) as u16,
                ((step * 197) % 512) as u16,
                (step % 64) as u16,
            ])
            .unwrap();
            state.apply(
                &parameters,
                ((step * 7) % 80) as u16,
                if step % 5 == 0 {
                    Transition::Immediate
                } else {
                    Transition::Interpolate
                },
            );
            if step % 7 == 4 {
                state.hold_current();
            }
            for age in [0, 1, 2, 3, 7, 15, 30, 31, 32, 49, 50, i16::MAX as u16] {
                let snapshot = state.at(age);
                for value in snapshot.lsp_q15 {
                    include(&value.to_le_bytes());
                }
                include(&snapshot.log_energy_q26.to_le_bytes());
                include(&snapshot.weight_q14.to_le_bytes());
                include(&state.period_inverse_q15.to_le_bytes());
            }
        }
        assert_eq!(hash, 0x1674a3c4339d491e);
    }
    #[test]
    fn interpolation_snapshots_do_not_advance_state_and_reset_restores_noise() {
        let mut state = SidInterpolation::default();
        let initial = state.at(0);
        state.apply(
            &SidParameters::try_new([7, 255, 511, 511, 63]).unwrap(),
            32,
            Transition::Interpolate,
        );
        let before = state.clone();
        state.at(u16::MAX);
        assert_eq!(state, before);
        state.reset();
        assert_eq!(state.at(0), initial);
    }
    #[test]
    fn muting_uses_reference_zero_age_fallback_and_saturates_energy_decay() {
        let mut state = SidInterpolation::default();
        state.mute_after_render(0);
        assert_eq!(state.period_inverse_q15, 4096);
        assert_eq!(
            state.current_log_energy_q11,
            INITIAL_LOG_ENERGY_Q11 - MUTE_ENERGY_STEP_Q11
        );
        state.mute_after_render(1);
        assert_eq!(state.period_inverse_q15, i16::MAX);
        for _ in 0..1000 {
            state.mute_after_render(1);
        }
        assert_eq!(state.current_log_energy_q11, i16::MIN);
    }
}
