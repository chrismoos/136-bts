pub mod adaptive;
pub mod analysis_spectrum;
pub mod autocorrelation;
pub mod codebook;
pub mod comfort_noise;
pub mod decoder;
pub mod dtx;
pub mod encoder;
pub mod encoder_analysis;
pub mod encoder_filtering;
mod encoder_stability;
pub mod energy;
pub mod excitation;
pub mod frame_loss;
pub mod gain;
pub mod gain_concealment;
pub mod levinson;
pub mod logarithm;
pub mod lpc;
pub mod missing_parameters;
pub mod noise_history;
pub mod noise_level;
pub mod noise_pcm;
pub mod noise_receiver;
pub mod noise_spectrum;
pub mod noise_synthesis;
pub mod pitch;
pub mod postfilter;
pub mod postprocess;
pub mod prediction_gain;
pub mod preprocess;
pub mod receiver;
pub mod sid;
pub mod sid_interpolation;
pub mod spectral;
pub mod synthesis;
use std::{error::Error, fmt};
pub const FRAME_BITS: usize = 148;
pub const PARAMETER_COUNT: usize = 19;
pub const LSF_INDICES: usize = 3;
pub const SUBFRAMES: usize = 4;
pub const SUBFRAME_SAMPLES: usize = 40;
const PARAMETERS_PER_SUBFRAME: usize = 4;
// TS 26.073 bitno_MR74 uses three LSF indices and four pitch, position, sign and gain groups.
const WIDTHS: [u8; PARAMETER_COUNT] = [8, 9, 9, 8, 13, 4, 7, 5, 13, 4, 7, 8, 13, 4, 7, 5, 13, 4, 7];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubframeParameters {
    pub pitch_index: u16,
    pub pulse_positions: u16,
    pub pulse_signs: u16,
    pub gain_index: u16,
}
impl SubframeParameters {
    pub fn fixed_codebook(&self) -> Result<codebook::Codebook, codebook::Error> {
        codebook::Codebook::try_new(self.pulse_positions, self.pulse_signs)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterError {
    pub parameter: usize,
    pub value: u16,
    pub maximum: u16,
}
impl fmt::Display for ParameterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MR74 parameter {} value {} exceeds {}",
            self.parameter, self.value, self.maximum
        )
    }
}
impl Error for ParameterError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameters {
    values: [u16; PARAMETER_COUNT],
}
impl Parameters {
    pub fn try_new(values: [u16; PARAMETER_COUNT]) -> Result<Self, ParameterError> {
        for (parameter, (&value, &width)) in values.iter().zip(&WIDTHS).enumerate() {
            let maximum = (1u16 << width) - 1;
            if value > maximum {
                return Err(ParameterError {
                    parameter,
                    value,
                    maximum,
                });
            }
        }
        Ok(Self { values })
    }
    pub fn values(&self) -> &[u16; PARAMETER_COUNT] {
        &self.values
    }
    pub fn lsf_indices(&self) -> [u16; LSF_INDICES] {
        self.values[..LSF_INDICES]
            .try_into()
            .expect("three LSF parameters precede subframes")
    }
    pub fn subframes(&self) -> [SubframeParameters; SUBFRAMES] {
        std::array::from_fn(|index| {
            let base = LSF_INDICES + index * PARAMETERS_PER_SUBFRAME;
            SubframeParameters {
                pitch_index: self.values[base],
                pulse_positions: self.values[base + 1],
                pulse_signs: self.values[base + 2],
                gain_index: self.values[base + 3],
            }
        })
    }
    /// Bits are in codec parameter order, MSB first. They are not an AMR interface frame or D-AMPS channel word.
    pub fn codec_bits(&self) -> [bool; FRAME_BITS] {
        let mut bits = [false; FRAME_BITS];
        let mut offset = 0;
        for (&value, &width) in self.values.iter().zip(&WIDTHS) {
            for bit in (0..width).rev() {
                bits[offset] = value & (1 << bit) != 0;
                offset += 1;
            }
        }
        bits
    }
    pub fn from_codec_bits(bits: &[bool; FRAME_BITS]) -> Self {
        let mut offset = 0;
        let values = std::array::from_fn(|parameter| {
            let width = usize::from(WIDTHS[parameter]);
            let value = bits[offset..offset + width]
                .iter()
                .fold(0u16, |value, bit| value * 2 + u16::from(*bit));
            offset += width;
            value
        });
        Self { values }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_parameter_widths_preserve_bits_and_reject_overflow() {
        assert_eq!(
            WIDTHS
                .iter()
                .map(|width| usize::from(*width))
                .sum::<usize>(),
            FRAME_BITS
        );
        let values = WIDTHS.map(|width| (1u16 << width) - 1);
        let parameters = Parameters::try_new(values).unwrap();
        assert_eq!(parameters.codec_bits(), [true; FRAME_BITS]);
        assert_eq!(Parameters::from_codec_bits(&[true; FRAME_BITS]), parameters);
        for parameter in 0..PARAMETER_COUNT {
            let mut overflow = values;
            overflow[parameter] += 1;
            let error = Parameters::try_new(overflow).unwrap_err();
            assert_eq!(error.parameter, parameter);
            assert_eq!(error.maximum, values[parameter]);
        }
    }
}
