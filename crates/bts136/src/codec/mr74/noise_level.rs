use super::{logarithm, prediction_gain, spectral::LPC_ORDER};
const MR74_ENERGY_ADJUST_Q11: i16 = -440;
const SMOOTH_CURRENT_Q15: i16 = 29491;
const SMOOTH_UPDATE_Q15: i16 = 3277;
const ADJUSTMENT_SMOOTH_SHIFT: u32 = 5;
const Q15_SHIFT: u32 = 15;
const INTERPOLATED_ENERGY_SHIFT: u32 = 10;
const LOG_ENERGY_OFFSET_Q16: i32 = 4 << 16;
const LOG_PREDICTION_SHIFT: u32 = 4;
const WORD_SHIFT: u32 = 16;
const FRACTION_MASK: i32 = (1 << WORD_SHIFT) - 1;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    pub gain_q4: i16,
    pub prediction_gain_q12: i16,
    pub prediction_mean_q12: i16,
    pub energy_adjustment_q11: i16,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoiseLevel {
    prediction_mean_q12: i16,
    energy_adjustment_q11: i16,
}
impl NoiseLevel {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn prediction_mean_q12(&self) -> i16 {
        self.prediction_mean_q12
    }
    /// TS 26.073 dtx_dec restores the mode adjustment when backward hangover analysis supplies noise parameters.
    pub fn set_hangover_adjustment(&mut self) {
        self.energy_adjustment_q11 = MR74_ENERGY_ADJUST_Q11;
    }
    /// TS 26.073 dtx_dec combines interpolated log energy, instantaneous prediction gain and smoothed mode adjustment.
    pub fn calculate(&mut self, log_energy_q26: i32, coefficients_q12: &[i16; LPC_ORDER]) -> Level {
        let adjustment = multiply_q15(
            MR74_ENERGY_ADJUST_Q11 << ADJUSTMENT_SMOOTH_SHIFT,
            SMOOTH_UPDATE_Q15,
        ) >> ADJUSTMENT_SMOOTH_SHIFT;
        self.energy_adjustment_q11 =
            multiply_q15(self.energy_adjustment_q11, SMOOTH_CURRENT_Q15).saturating_add(adjustment);
        let prediction_gain_q12 = prediction_gain::prediction_gain(coefficients_q12).log_gain_q12;
        self.prediction_mean_q12 = multiply_q15(self.prediction_mean_q12, SMOOTH_CURRENT_Q15)
            .saturating_add(multiply_q15(prediction_gain_q12, SMOOTH_UPDATE_Q15));
        let log = (log_energy_q26 >> INTERPOLATED_ENERGY_SHIFT)
            .saturating_add(LOG_ENERGY_OFFSET_Q16)
            .saturating_sub(i32::from(prediction_gain_q12) << LOG_PREDICTION_SHIFT)
            .saturating_add(i32::from(self.energy_adjustment_q11) << ADJUSTMENT_SMOOTH_SHIFT);
        let exponent = (log >> WORD_SHIFT) as i16;
        let fraction = ((log & FRACTION_MASK) >> 1) as u16;
        let gain_q4 = logarithm::pow2_signed(exponent, fraction)
            .expect("fraction is extracted from the low word into Q15")
            as i16;
        Level {
            gain_q4,
            prediction_gain_q12,
            prediction_mean_q12: self.prediction_mean_q12,
            energy_adjustment_q11: self.energy_adjustment_q11,
        }
    }
}
fn multiply_q15(left: i16, right: i16) -> i16 {
    ((i32::from(left) * i32::from(right)) >> Q15_SHIFT)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::{
        LSF_INDICES, PARAMETER_COUNT, Parameters, lpc::LpcDecoder, sid::SidParameters,
    };
    #[test]
    fn noise_level_gain_and_smoothing_match_reference_arithmetic() {
        let mut level = NoiseLevel::default();
        let mut predictor = LpcDecoder::default();
        let mut hash = 0xcbf29ce484222325u64;
        for step in 0..4096 {
            let mut values = [0; PARAMETER_COUNT];
            values[..LSF_INDICES].copy_from_slice(&[
                (step % 256) as u16,
                ((step * 73 + step / 512) % 512) as u16,
                ((step * 197 + step / 256) % 512) as u16,
            ]);
            let frame = predictor.decode(&Parameters::try_new(values).unwrap());
            let coefficients = frame.coefficients_q12[3][1..].try_into().unwrap();
            if step % 17 == 0 {
                level.set_hangover_adjustment();
            }
            let energy = SidParameters::try_new([0, 0, 0, 0, (step % 64) as u16])
                .unwrap()
                .log_energy_q11();
            let old_energy = SidParameters::try_new([0, 0, 0, 0, ((step + 23) % 64) as u16])
                .unwrap()
                .log_energy_q11();
            let weight = (step * 73) % 16385;
            let interpolated = (weight * i32::from(energy) * 2)
                .saturating_add((16384 - weight) * i32::from(old_energy) * 2);
            let output = level.calculate(interpolated, coefficients);
            for value in [
                output.gain_q4,
                output.prediction_gain_q12,
                output.prediction_mean_q12,
                output.energy_adjustment_q11,
            ] {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        assert_eq!(hash, 0x4ba17cd04607f7e4);
        level.reset();
        assert_eq!(level, NoiseLevel::default());
    }
}
