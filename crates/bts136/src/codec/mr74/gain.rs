pub mod quantization;
use super::{SUBFRAME_SAMPLES, logarithm};
use std::{error::Error, fmt};
const GAIN_ENTRIES: usize = 128;
const HISTORY_TAPS: usize = 4;
const AVERAGE_SHIFT: u32 = 2;
pub(super) const INITIAL_ENERGY_Q10: i16 = -14336;
const PREDICTOR_Q13: [i16; HISTORY_TAPS] = [5571, 4751, 2785, 1556];
const LOG_ENERGY_FACTOR_Q13: i16 = -24660;
const MEAN_ENERGY_Q14: i32 = 32588 * 32 * 2;
const ENERGY_SCALE_SHIFT: u32 = 10;
const WORD_SHIFT: u32 = 16;
const FRACTION_SHIFT: u32 = 15;
const PREDICTED_GAIN_SCALE_SHIFT: u32 = 8;
const PREDICTED_GAIN_EXPONENT: u8 = 14;
const CODE_GAIN_SCALE_SHIFT: i16 = 10;
// TS 26.073 gc_pred uses 5439 for MR74 to preserve IS-641 bit exactness.
const GAIN_LOG_FACTOR_Q15: i16 = 5439;
// TS 26.073 table_gain_highrates columns are pitch Q14, correction Q12 and IS-641 energy Q10.
const TABLE: [[i16; 3]; GAIN_ENTRIES] = [
    [577, 662, -16214],
    [806, 1836, -7135],
    [3109, 1052, -12086],
    [4181, 1387, -9629],
    [2373, 1425, -9394],
    [3248, 1985, -6442],
    [1827, 2320, -5056],
    [941, 3314, -1885],
    [2351, 2977, -2838],
    [3616, 2420, -4681],
    [3451, 3096, -2490],
    [2955, 4301, 434],
    [1848, 4500, 836],
    [3884, 5416, 2484],
    [1187, 7210, 5030],
    [3083, 9000, 7002],
    [7384, 883, -13647],
    [5962, 1506, -8900],
    [5155, 2134, -5800],
    [7944, 2009, -6335],
    [6507, 2250, -5327],
    [7670, 2752, -3537],
    [5952, 3016, -2724],
    [4898, 3764, -751],
    [6989, 3588, -1177],
    [8174, 3978, -260],
    [6064, 4404, 645],
    [7709, 5087, 1928],
    [5523, 6021, 3426],
    [7769, 7126, 4926],
    [6060, 7938, 5885],
    [5594, 11487, 9172],
    [10581, 1356, -9831],
    [9049, 1597, -8380],
    [9794, 2035, -6220],
    [8946, 2415, -4700],
    [10296, 2584, -4099],
    [9407, 2734, -3595],
    [8700, 3218, -2144],
    [9757, 3395, -1669],
    [10177, 3892, -454],
    [9170, 4528, 891],
    [10152, 5004, 1781],
    [9114, 5735, 2993],
    [10500, 6266, 3782],
    [10110, 7631, 5534],
    [8844, 8727, 6728],
    [8956, 12496, 9921],
    [12924, 976, -12753],
    [11435, 1755, -7539],
    [12138, 2328, -5024],
    [11388, 2368, -4872],
    [10700, 3064, -2580],
    [12332, 2861, -3192],
    [11722, 3327, -1848],
    [11270, 3700, -904],
    [10861, 4413, 663],
    [12082, 4533, 902],
    [11283, 5205, 2132],
    [11960, 6305, 3837],
    [11167, 7534, 5420],
    [12128, 8329, 6312],
    [10969, 10777, 8604],
    [10300, 17376, 12853],
    [13899, 1681, -7921],
    [12580, 2045, -6179],
    [13265, 2439, -4610],
    [14033, 2989, -2802],
    [13452, 3098, -2482],
    [12396, 3658, -1006],
    [13510, 3780, -713],
    [12880, 4272, 374],
    [13533, 4861, 1523],
    [12667, 5457, 2552],
    [13854, 6106, 3551],
    [13031, 6483, 4084],
    [13557, 7721, 5639],
    [12957, 9311, 7304],
    [13714, 11551, 9221],
    [12591, 15206, 11667],
    [15113, 1540, -8700],
    [15072, 2333, -5007],
    [14527, 2511, -4352],
    [14692, 3199, -2197],
    [15382, 3560, -1247],
    [14133, 3960, -300],
    [15102, 4236, 298],
    [14332, 4824, 1454],
    [14846, 5451, 2542],
    [15306, 6083, 3518],
    [14329, 6888, 4623],
    [15060, 7689, 5602],
    [14406, 9426, 7413],
    [15387, 9741, 7706],
    [14824, 14271, 11102],
    [13600, 24939, 16067],
    [16396, 1969, -6517],
    [16817, 2832, -3283],
    [15713, 2843, -3248],
    [16104, 3336, -1825],
    [16384, 3963, -294],
    [16940, 4579, 992],
    [15711, 4599, 1030],
    [16222, 5448, 2537],
    [16832, 6382, 3945],
    [15745, 7141, 4944],
    [16326, 7469, 5343],
    [16611, 8624, 6622],
    [17028, 10418, 8303],
    [15905, 11817, 9423],
    [16878, 14690, 11360],
    [16515, 20870, 14483],
    [18142, 2083, -6013],
    [19401, 3178, -2257],
    [17508, 3426, -1589],
    [20054, 4027, -151],
    [18069, 4249, 326],
    [18952, 5066, 1890],
    [17711, 5402, 2461],
    [19835, 6192, 3676],
    [17950, 7014, 4784],
    [21318, 7877, 5816],
    [17910, 9289, 7283],
    [19144, 9290, 7284],
    [20517, 11381, 9089],
    [18075, 14485, 11234],
    [19999, 17882, 13108],
    [18842, 32764, 18494],
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gains {
    pub pitch_q14: i16,
    pub code_q1: i16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GainIndexError(pub u16);
impl fmt::Display for GainIndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MR74 gain index {} exceeds 127", self.0)
    }
}
impl Error for GainIndexError {}
#[derive(Debug, Clone, Copy)]
struct Prediction {
    exponent: i16,
    fraction: u16,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GainDecoder {
    energies_q10: [i16; HISTORY_TAPS],
}
impl Default for GainDecoder {
    fn default() -> Self {
        Self {
            energies_q10: [INITIAL_ENERGY_Q10; HISTORY_TAPS],
        }
    }
}
impl GainDecoder {
    pub(super) fn resume_noise_energy(&mut self, energy_q10: i16) {
        self.energies_q10 = [energy_q10; HISTORY_TAPS];
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub(super) fn conceal_energy(&mut self) {
        // TS 26.073 gc_pred_average_limited saturates the 16-bit sum before averaging.
        let average = (self
            .energies_q10
            .iter()
            .fold(0i16, |sum, &energy| sum.saturating_add(energy))
            >> AVERAGE_SHIFT)
            .max(INITIAL_ENERGY_Q10);
        self.energies_q10.rotate_right(1);
        self.energies_q10[0] = average;
    }
    /// Innovation includes pitch sharpening before gain prediction, as in TS 26.073 Decoder_amr.
    pub fn decode(
        &mut self,
        index: u16,
        innovation_q13: &[i16; SUBFRAME_SAMPLES],
    ) -> Result<Gains, GainIndexError> {
        let [pitch, correction, energy] =
            *TABLE.get(usize::from(index)).ok_or(GainIndexError(index))?;
        let Prediction { exponent, fraction } = self.predict(innovation_q13);
        let base_gain = logarithm::pow2(PREDICTED_GAIN_EXPONENT, fraction)
            .expect("extracted fractional logarithm is Q15") as i16;
        let gain = mult(correction, base_gain);
        let shift = CODE_GAIN_SCALE_SHIFT - exponent;
        let scaled = if shift >= 0 {
            gain >> (shift as u32).min(i32::BITS - 1)
        } else {
            shift_left(gain, u32::from(shift.unsigned_abs()))
        };
        self.energies_q10.rotate_right(1);
        self.energies_q10[0] = energy;
        Ok(Gains {
            pitch_q14: pitch,
            code_q1: (scaled >> WORD_SHIFT) as i16,
        })
    }
    fn predict(&self, innovation_q13: &[i16; SUBFRAME_SAMPLES]) -> Prediction {
        let mut energy_sum = 0i32;
        for &sample in innovation_q13 {
            energy_sum = mac(energy_sum, sample, sample);
        }
        let logarithm = logarithm::log2(energy_sum);
        let log_fraction = ((i32::from(logarithm.fraction_q15) * i32::from(LOG_ENERGY_FACTOR_Q13))
            >> FRACTION_SHIFT) as i16;
        let mut predicted = mac(
            mult(i16::from(logarithm.exponent), LOG_ENERGY_FACTOR_Q13),
            log_fraction,
            1,
        );
        predicted = predicted.saturating_add(MEAN_ENERGY_Q14);
        predicted = shift_left(predicted, ENERGY_SCALE_SHIFT);
        for (&coefficient, &past_energy) in PREDICTOR_Q13.iter().zip(&self.energies_q10) {
            predicted = mac(predicted, coefficient, past_energy);
        }
        let predicted_gain = (predicted >> WORD_SHIFT) as i16;
        let predicted_log = mult(predicted_gain, GAIN_LOG_FACTOR_Q15) >> PREDICTED_GAIN_SCALE_SHIFT;
        let exponent = (predicted_log >> WORD_SHIFT) as i16;
        let fraction = ((predicted_log >> 1) - (i32::from(exponent) << FRACTION_SHIFT)) as u16;
        Prediction { exponent, fraction }
    }
}
fn mult(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn mac(sum: i32, left: i16, right: i16) -> i32 {
    sum.saturating_add(mult(left, right))
}
fn shift_left(value: i32, shift: u32) -> i32 {
    if shift >= i32::BITS {
        return match value.cmp(&0) {
            std::cmp::Ordering::Less => i32::MIN,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => i32::MAX,
        };
    }
    (i64::from(value) << shift).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consecutive_gains_and_energy_updates_match_reference() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..7 {
            let mut decoder = GainDecoder::default();
            for step in 0..512 {
                let innovation = std::array::from_fn(|index| match pattern {
                    0 => 0,
                    1 => {
                        if index == 0 {
                            8191
                        } else {
                            0
                        }
                    }
                    2 => {
                        if index % 2 == 0 {
                            i16::MAX
                        } else {
                            i16::MIN
                        }
                    }
                    3 => i16::MAX,
                    4 => i16::MIN,
                    6 => {
                        if index < 4 {
                            if step & (1 << index) != 0 {
                                8191
                            } else {
                                -8192
                            }
                        } else {
                            0
                        }
                    }
                    _ => (((index * 347 + step * 997) % 65536) as i32 - 32768) as i16,
                });
                let gain = decoder
                    .decode((step % GAIN_ENTRIES) as u16, &innovation)
                    .unwrap();
                for value in [gain.pitch_q14, gain.code_q1]
                    .into_iter()
                    .chain(decoder.energies_q10)
                {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
        assert_eq!(hash, 0x3798eca312ffb4e7);
    }
    #[test]
    fn rejected_index_preserves_history_and_reset_restores_prediction() {
        let mut decoder = GainDecoder::default();
        let innovation = [8191; SUBFRAME_SAMPLES];
        let first = decoder.decode(0, &innovation).unwrap();
        let before = decoder.clone();
        assert_eq!(
            decoder.decode(GAIN_ENTRIES as u16, &innovation),
            Err(GainIndexError(GAIN_ENTRIES as u16))
        );
        assert_eq!(decoder, before);
        decoder.reset();
        assert_eq!(decoder.decode(0, &innovation).unwrap(), first);
    }
}
