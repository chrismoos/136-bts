pub mod analysis;
mod codebooks;
mod concealment;
pub use codebooks::CodebookExcitation;
pub mod decoder;
pub mod encoder;
pub mod excitation;
pub mod excitation_search;
pub mod gain_search;
pub mod gains;
mod interpolation;
pub mod pitch;
pub mod preprocessing;
mod quantization;
pub mod stream_analysis;
pub mod synthesis;
pub mod weighting;
use crate::phy::speech::{CLASS_ONE_BITS, CLASS_TWO_BITS, SpeechBits};
use std::{error::Error, fmt};

pub const LPC_COEFFICIENTS: usize = 10;
pub const SUBFRAMES: usize = 4;
pub const SUBFRAME_SAMPLES: usize = 40;
pub const FRAME_SAMPLES: usize = SUBFRAMES * SUBFRAME_SAMPLES;
pub const PCM_SAMPLE_RATE_HZ: usize = 8_000;
pub const MIN_LAG_SAMPLES: usize = 20;
pub const MAX_LAG_SAMPLES: usize = 146;
const LAG_CODE_OFFSET: usize = MIN_LAG_SAMPLES - 1;
const R0_BITS: u8 = 5;
const SUBFRAME_CODE_BITS: u8 = 7;
const LPC_WIDTHS: [u8; LPC_COEFFICIENTS] = [6, 5, 5, 4, 4, 3, 3, 3, 3, 2];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictorOrigin {
    Interpolated,
    PreviousFrame,
    CurrentFrame,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubframePredictor {
    pub coefficients: [f64; LPC_COEFFICIENTS],
    pub energy_full_scale_squared: f64,
    pub origin: PredictorOrigin,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterError {
    R0,
    Lpc { coefficient: usize },
    Lag,
    Code1,
    Code2,
}
impl fmt::Display for ParameterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP parameter outside encoded width: {self:?}")
    }
}
impl Error for ParameterError {}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VselpSubframe {
    lag: u8,
    code1: u8,
    code2: u8,
    gsp0: u8,
}
impl VselpSubframe {
    pub fn try_new(lag: u8, code1: u8, code2: u8, gsp0: u8) -> Result<Self, ParameterError> {
        if lag >= 1 << SUBFRAME_CODE_BITS {
            return Err(ParameterError::Lag);
        }
        if code1 >= 1 << SUBFRAME_CODE_BITS {
            return Err(ParameterError::Code1);
        }
        if code2 >= 1 << SUBFRAME_CODE_BITS {
            return Err(ParameterError::Code2);
        }
        Ok(Self {
            lag,
            code1,
            code2,
            gsp0,
        })
    }
    pub fn lag(&self) -> u8 {
        self.lag
    }
    /// IS-54-B §2.1.3.3.2.6.3.2: zero disables prediction, other codes add nineteen samples.
    pub fn pitch_lag_samples(&self) -> Option<usize> {
        if self.lag == 0 {
            None
        } else {
            Some(usize::from(self.lag) + LAG_CODE_OFFSET)
        }
    }
    pub fn code1(&self) -> u8 {
        self.code1
    }
    pub fn code2(&self) -> u8 {
        self.code2
    }
    pub fn codebook_excitation(&self) -> CodebookExcitation {
        codebooks::decode(self)
    }
    pub fn gain_parameters(&self) -> gains::GainParameters {
        gains::GainParameters::from_code(self.gsp0)
    }
    pub fn gsp0(&self) -> u8 {
        self.gsp0
    }
}
/// Encoded parameter indices from IS-54-B §2.1.3.3.2.1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VselpParameters {
    r0: u8,
    lpc: [u8; LPC_COEFFICIENTS],
    subframes: [VselpSubframe; SUBFRAMES],
}
impl VselpParameters {
    pub fn try_new(
        r0: u8,
        lpc: [u8; LPC_COEFFICIENTS],
        subframes: [VselpSubframe; SUBFRAMES],
    ) -> Result<Self, ParameterError> {
        if r0 >= 1 << R0_BITS {
            return Err(ParameterError::R0);
        }
        for (coefficient, (code, width)) in lpc.into_iter().zip(LPC_WIDTHS).enumerate() {
            if code >= 1 << width {
                return Err(ParameterError::Lpc { coefficient });
            }
        }
        Ok(Self { r0, lpc, subframes })
    }
    pub fn r0(&self) -> u8 {
        self.r0
    }
    pub fn lpc(&self) -> &[u8; LPC_COEFFICIENTS] {
        &self.lpc
    }
    pub fn subframes(&self) -> &[VselpSubframe; SUBFRAMES] {
        &self.subframes
    }
    pub fn reflection_coefficients(&self) -> [f64; LPC_COEFFICIENTS] {
        quantization::reflections(&self.lpc)
    }
    /// IS-54-B §2.1.3.3.2.4.1 uses the denominator 1 minus the predictor sum.
    pub fn predictor_coefficients(&self) -> [f64; LPC_COEFFICIENTS] {
        quantization::direct_form(&self.reflection_coefficients())
    }
    /// IS-54-B §2.1.3.3.2.5.2 energy relative to the square of maximum PCM amplitude.
    pub fn frame_energy_full_scale_squared(&self) -> f64 {
        quantization::frame_energy(self.r0)
    }
    pub fn subframe_predictors(&self, previous: &Self) -> [SubframePredictor; SUBFRAMES] {
        interpolation::interpolate(
            &previous.predictor_coefficients(),
            &self.predictor_coefficients(),
            previous.frame_energy_full_scale_squared(),
            self.frame_energy_full_scale_squared(),
        )
    }
    pub fn speech_bits(&self) -> SpeechBits {
        SpeechBits {
            class_one: CLASS_ONE.map(|(parameter, bit)| self.value(parameter) & (1 << bit) != 0),
            class_two: CLASS_TWO.map(|(parameter, bit)| self.value(parameter) & (1 << bit) != 0),
        }
    }
    pub fn from_speech_bits(bits: &SpeechBits) -> Self {
        let mut parameters = Self::default();
        for ((parameter, bit), value) in CLASS_ONE
            .into_iter()
            .zip(bits.class_one)
            .chain(CLASS_TWO.into_iter().zip(bits.class_two))
        {
            if value {
                *parameters.value_mut(parameter) |= 1 << bit;
            }
        }
        parameters
    }
    fn value(&self, parameter: Parameter) -> u8 {
        match parameter {
            Parameter::R0 => self.r0,
            Parameter::Lpc(i) => self.lpc[i],
            Parameter::Lag(i) => self.subframes[i].lag,
            Parameter::Gsp0(i) => self.subframes[i].gsp0,
            Parameter::Code1(i) => self.subframes[i].code1,
            Parameter::Code2(i) => self.subframes[i].code2,
        }
    }
    fn value_mut(&mut self, parameter: Parameter) -> &mut u8 {
        match parameter {
            Parameter::R0 => &mut self.r0,
            Parameter::Lpc(i) => &mut self.lpc[i],
            Parameter::Lag(i) => &mut self.subframes[i].lag,
            Parameter::Gsp0(i) => &mut self.subframes[i].gsp0,
            Parameter::Code1(i) => &mut self.subframes[i].code1,
            Parameter::Code2(i) => &mut self.subframes[i].code2,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parameter {
    R0,
    Lpc(usize),
    Lag(usize),
    Gsp0(usize),
    Code1(usize),
    Code2(usize),
}
use Parameter::*;
// IS-54-B Table 2.1.3.3.3.4-2, CL1[4..80], with CRC and tails excluded.
const CLASS_ONE: [(Parameter, u8); CLASS_ONE_BITS] = [
    (R0, 3),
    (R0, 2),
    (Lpc(2), 4),
    (Lpc(3), 3),
    (Lpc(0), 3),
    (Lpc(4), 3),
    (Lag(1), 6),
    (Lag(3), 6),
    (Lag(1), 5),
    (Lag(3), 5),
    (Lag(1), 4),
    (Lag(3), 4),
    (Lag(1), 3),
    (Lag(3), 3),
    (Gsp0(1), 7),
    (Gsp0(3), 7),
    (Lag(1), 2),
    (Lag(3), 2),
    (Lag(1), 1),
    (Lag(3), 1),
    (Lag(1), 0),
    (Lag(3), 0),
    (Gsp0(0), 6),
    (Gsp0(2), 6),
    (R0, 1),
    (Gsp0(0), 5),
    (Gsp0(2), 5),
    (Lpc(0), 2),
    (Lpc(2), 2),
    (Gsp0(1), 4),
    (Gsp0(3), 4),
    (Gsp0(1), 3),
    (Gsp0(3), 3),
    (Gsp0(1), 2),
    (Gsp0(3), 2),
    (Gsp0(1), 1),
    (Gsp0(3), 1),
    (Gsp0(1), 0),
    (Gsp0(2), 0),
    (Gsp0(0), 0),
    (Gsp0(2), 1),
    (Gsp0(0), 1),
    (Gsp0(2), 2),
    (Gsp0(0), 2),
    (Gsp0(2), 3),
    (Gsp0(0), 3),
    (Gsp0(2), 4),
    (Gsp0(0), 4),
    (Lpc(1), 2),
    (Gsp0(3), 5),
    (Gsp0(1), 5),
    (Lpc(3), 2),
    (Gsp0(3), 6),
    (Gsp0(1), 6),
    (Gsp0(3), 0),
    (Lag(2), 0),
    (Lag(0), 0),
    (Lag(2), 1),
    (Lag(0), 1),
    (Lag(2), 2),
    (Lag(0), 2),
    (Gsp0(2), 7),
    (Gsp0(0), 7),
    (Lag(2), 3),
    (Lag(0), 3),
    (Lag(2), 4),
    (Lag(0), 4),
    (Lag(2), 5),
    (Lag(0), 5),
    (Lag(2), 6),
    (Lag(0), 6),
    (Lpc(2), 3),
    (Lpc(1), 3),
    (Lpc(0), 4),
    (Lpc(1), 4),
    (Lpc(0), 5),
    (R0, 4),
];
// IS-54-B Table 2.1.3.3.4-1, CL2[0..81].
const CLASS_TWO: [(Parameter, u8); CLASS_TWO_BITS] = [
    (Code2(3), 0),
    (Code2(3), 1),
    (Code2(3), 2),
    (Code2(3), 3),
    (Code2(3), 4),
    (Code2(3), 5),
    (Code2(3), 6),
    (Code1(3), 0),
    (Code1(3), 1),
    (Code1(3), 2),
    (Code1(3), 3),
    (Code1(3), 4),
    (Code1(3), 5),
    (Code1(3), 6),
    (Code2(2), 0),
    (Code2(2), 1),
    (Code2(2), 2),
    (Code2(2), 3),
    (Code2(2), 4),
    (Code2(2), 5),
    (Code2(2), 6),
    (Code1(2), 0),
    (Code1(2), 1),
    (Code1(2), 2),
    (Code1(2), 3),
    (Code1(2), 4),
    (Code1(2), 5),
    (Code1(2), 6),
    (Lpc(5), 2),
    (Lpc(9), 0),
    (Lpc(9), 1),
    (Lpc(8), 0),
    (Lpc(8), 1),
    (Lpc(8), 2),
    (Lpc(7), 0),
    (Lpc(7), 1),
    (Lpc(7), 2),
    (Lpc(6), 0),
    (Lpc(6), 1),
    (Lpc(6), 2),
    (Lpc(5), 0),
    (Lpc(5), 1),
    (Lpc(4), 0),
    (Lpc(4), 1),
    (Lpc(4), 2),
    (Lpc(3), 0),
    (Lpc(3), 1),
    (Lpc(2), 0),
    (Lpc(2), 1),
    (Lpc(1), 0),
    (Lpc(1), 1),
    (Lpc(0), 0),
    (Lpc(0), 1),
    (R0, 0),
    (Code2(1), 0),
    (Code2(1), 1),
    (Code2(1), 2),
    (Code2(1), 3),
    (Code2(1), 4),
    (Code2(1), 5),
    (Code2(1), 6),
    (Code1(1), 0),
    (Code1(1), 1),
    (Code1(1), 2),
    (Code1(1), 3),
    (Code1(1), 4),
    (Code1(1), 5),
    (Code1(1), 6),
    (Code2(0), 0),
    (Code2(0), 1),
    (Code2(0), 2),
    (Code2(0), 3),
    (Code2(0), 4),
    (Code2(0), 5),
    (Code2(0), 6),
    (Code1(0), 0),
    (Code1(0), 1),
    (Code1(0), 2),
    (Code1(0), 3),
    (Code1(0), 4),
    (Code1(0), 5),
    (Code1(0), 6),
];

#[cfg(test)]
mod tests {
    use super::*;
    fn parameters() -> VselpParameters {
        VselpParameters::try_new(
            21,
            [42, 19, 13, 9, 6, 5, 3, 1, 7, 2],
            [
                VselpSubframe::try_new(17, 1, 23, 165).unwrap(),
                VselpSubframe::try_new(42, 37, 54, 60).unwrap(),
                VselpSubframe::try_new(71, 65, 89, 129).unwrap(),
                VselpSubframe::try_new(115, 127, 110, 126).unwrap(),
            ],
        )
        .unwrap()
    }
    #[test]
    fn fixed_parameter_vector_matches_both_standard_class_tables() {
        let expected = parameters();
        let bits = expected.speech_bits();
        assert_eq!(
            bits.class_one
                .map(|bit| if bit { '1' } else { '0' })
                .iter()
                .collect::<String>(),
            "01011001110110000011010001001111111010110001000001101001110101100010010100111"
        );
        assert_eq!(
            bits.class_two
                .map(|bit| if bit { '1' } else { '0' })
                .iter()
                .collect::<String>(),
            "0111011111111110011011000001101111100110100111010110110110110101001011101001000000"
        );
        assert_eq!(VselpParameters::from_speech_bits(&bits), expected);
    }
    #[test]
    fn all_159_parameter_bits_are_present_once_with_no_missing_or_duplicate_mapping() {
        let mapped = CLASS_ONE.into_iter().chain(CLASS_TWO).collect::<Vec<_>>();
        assert_eq!(mapped.len(), CLASS_ONE_BITS + CLASS_TWO_BITS);
        for (position, mapping) in mapped.iter().enumerate() {
            assert!(!mapped[..position].contains(mapping));
        }
        let mut expected = (0..R0_BITS).map(|bit| (R0, bit)).collect::<Vec<_>>();
        for (i, width) in LPC_WIDTHS.into_iter().enumerate() {
            expected.extend((0..width).map(|bit| (Lpc(i), bit)));
        }
        for i in 0..SUBFRAMES {
            for field in [Lag(i), Code1(i), Code2(i)] {
                expected.extend((0..SUBFRAME_CODE_BITS).map(|bit| (field, bit)));
            }
            expected.extend((0..u8::BITS as u8).map(|bit| (Gsp0(i), bit)));
        }
        for mapping in expected {
            assert!(mapped.contains(&mapping));
        }
        for index in 0..CLASS_ONE_BITS + CLASS_TWO_BITS {
            let mut bits = SpeechBits {
                class_one: [false; CLASS_ONE_BITS],
                class_two: [false; CLASS_TWO_BITS],
            };
            if index < CLASS_ONE_BITS {
                bits.class_one[index] = true;
            } else {
                bits.class_two[index - CLASS_ONE_BITS] = true;
            }
            assert_eq!(VselpParameters::from_speech_bits(&bits).speech_bits(), bits);
        }
    }
    #[test]
    fn parameter_widths_reject_overflow_and_accept_every_field_maximum() {
        assert!(matches!(
            VselpParameters::try_new(
                32,
                [0; LPC_COEFFICIENTS],
                [VselpSubframe::default(); SUBFRAMES]
            ),
            Err(ParameterError::R0)
        ));
        for (coefficient, width) in LPC_WIDTHS.into_iter().enumerate() {
            let mut lpc = [0; LPC_COEFFICIENTS];
            lpc[coefficient] = 1 << width;
            assert_eq!(
                VselpParameters::try_new(0, lpc, [VselpSubframe::default(); SUBFRAMES]),
                Err(ParameterError::Lpc { coefficient })
            );
        }
        assert_eq!(
            VselpSubframe::try_new(128, 0, 0, 0),
            Err(ParameterError::Lag)
        );
        assert_eq!(
            VselpSubframe::try_new(0, 128, 0, 0),
            Err(ParameterError::Code1)
        );
        assert_eq!(
            VselpSubframe::try_new(0, 0, 128, 0),
            Err(ParameterError::Code2)
        );
        let maximum = VselpParameters::try_new(
            31,
            LPC_WIDTHS.map(|width| (1 << width) - 1),
            [VselpSubframe::try_new(127, 127, 127, 255).unwrap(); SUBFRAMES],
        )
        .unwrap();
        assert!(
            maximum
                .speech_bits()
                .class_one
                .iter()
                .chain(&maximum.speech_bits().class_two)
                .all(|bit| *bit)
        );
    }
}
