use super::{Parameters, WIDTHS};
const INITIAL_SEED: i16 = 21845;
const SEED_MULTIPLIER: i16 = 31821;
const SEED_INCREMENT: i16 = 13849;
const WINDOW_INDEX_MASK: u16 = 0x7f;
const WINDOW_SAMPLES: usize = 240;
// TS 26.073 build_CN_param selects masked quantized parameters from window_200_40.
const WINDOW: [u16; WINDOW_SAMPLES] = [
    2621, 2623, 2629, 2638, 2651, 2668, 2689, 2713, 2741, 2772, 2808, 2847, 2890, 2936, 2986, 3040,
    3097, 3158, 3223, 3291, 3363, 3438, 3517, 3599, 3685, 3774, 3867, 3963, 4063, 4166, 4272, 4382,
    4495, 4611, 4731, 4853, 4979, 5108, 5240, 5376, 5514, 5655, 5800, 5947, 6097, 6250, 6406, 6565,
    6726, 6890, 7057, 7227, 7399, 7573, 7750, 7930, 8112, 8296, 8483, 8672, 8863, 9057, 9252, 9450,
    9650, 9852, 10055, 10261, 10468, 10677, 10888, 11101, 11315, 11531, 11748, 11967, 12187, 12409,
    12632, 12856, 13082, 13308, 13536, 13764, 13994, 14225, 14456, 14688, 14921, 15155, 15389,
    15624, 15859, 16095, 16331, 16568, 16805, 17042, 17279, 17516, 17754, 17991, 18228, 18465,
    18702, 18939, 19175, 19411, 19647, 19882, 20117, 20350, 20584, 20816, 21048, 21279, 21509,
    21738, 21967, 22194, 22420, 22644, 22868, 23090, 23311, 23531, 23749, 23965, 24181, 24394,
    24606, 24816, 25024, 25231, 25435, 25638, 25839, 26037, 26234, 26428, 26621, 26811, 26999,
    27184, 27368, 27548, 27727, 27903, 28076, 28247, 28415, 28581, 28743, 28903, 29061, 29215,
    29367, 29515, 29661, 29804, 29944, 30081, 30214, 30345, 30472, 30597, 30718, 30836, 30950,
    31062, 31170, 31274, 31376, 31474, 31568, 31659, 31747, 31831, 31911, 31988, 32062, 32132,
    32198, 32261, 32320, 32376, 32428, 32476, 32521, 32561, 32599, 32632, 32662, 32688, 32711,
    32729, 32744, 32755, 32763, 32767, 32767, 32741, 32665, 32537, 32359, 32129, 31850, 31521,
    31143, 30716, 30242, 29720, 29151, 28538, 27879, 27177, 26433, 25647, 24821, 23957, 23055,
    22117, 21145, 20139, 19102, 18036, 16941, 15820, 14674, 13505, 12315, 11106, 9879, 8637, 7381,
    6114, 4838, 3554, 2264, 971,
];
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingParameters {
    seed: i16,
}
impl Default for MissingParameters {
    fn default() -> Self {
        Self { seed: INITIAL_SEED }
    }
}
impl MissingParameters {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 Decoder_amr uses generated parameters for RX_NO_DATA and RX_ONSET before speech concealment.
    pub fn generate(&mut self) -> Parameters {
        self.seed = self
            .seed
            .wrapping_mul(SEED_MULTIPLIER)
            .wrapping_add(SEED_INCREMENT);
        let start = usize::from((self.seed as u16) & WINDOW_INDEX_MASK);
        let values =
            std::array::from_fn(|index| WINDOW[start + index] & ((1u16 << WIDTHS[index]) - 1));
        Parameters::try_new(values)
            .expect("generated parameters are masked to every MR74 field width")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_seed_period_matches_reference_parameters_and_state() {
        let mut generator = MissingParameters::default();
        let mut hash = 0xcbf29ce484222325u64;
        let mut visited = [false; (WINDOW_INDEX_MASK as usize) + 1];
        for _ in 0..=u16::MAX {
            let parameters = generator.generate();
            visited[usize::from((generator.seed as u16) & WINDOW_INDEX_MASK)] = true;
            for value in parameters
                .values()
                .iter()
                .copied()
                .chain([generator.seed as u16])
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        assert_eq!(hash, 0xbfff4b01448edf25);
        assert_eq!(generator.seed, INITIAL_SEED);
        assert!(visited.into_iter().all(|value| value));
    }
    #[test]
    fn reset_restores_missing_parameter_sequence() {
        let mut generator = MissingParameters::default();
        let first = generator.generate();
        generator.generate();
        generator.reset();
        assert_eq!(generator.generate(), first);
    }
}
