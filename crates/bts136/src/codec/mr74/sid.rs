use std::{error::Error, fmt};
pub const PARAMETERS: usize = 5;
pub const CODEC_BITS: usize = 35;
// TS 26.073 bitno_MRDTX. This codec representation does not define D-AMPS SID channel framing.
const WIDTHS: [u32; PARAMETERS] = [3, 8, 9, 9, 6];
const RESIDUAL_PARAMETER: usize = 0;
const LSF_START_PARAMETER: usize = 1;
const LSF_END_PARAMETER: usize = 4;
const ENERGY_PARAMETER: usize = 4;
const ENERGY_SCALE_SHIFT: u32 = 9;
const ENERGY_OFFSET_Q11: i16 = 5120;
const PREDICTION_OFFSET_Q10: i16 = 9000;
const MIN_PREDICTION_Q10: i16 = -14436;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidParameterError {
    pub index: usize,
    pub value: u16,
    pub maximum: u16,
}
impl fmt::Display for SidParameterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "reference SID parameter {} value {} exceeds {}",
            self.index, self.value, self.maximum
        )
    }
}
impl Error for SidParameterError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidParameters {
    values: [u16; PARAMETERS],
}
impl SidParameters {
    pub fn try_new(values: [u16; PARAMETERS]) -> Result<Self, SidParameterError> {
        for (index, (&value, &width)) in values.iter().zip(&WIDTHS).enumerate() {
            let maximum = (1 << width) - 1;
            if value > maximum {
                return Err(SidParameterError {
                    index,
                    value,
                    maximum,
                });
            }
        }
        Ok(Self { values })
    }
    pub fn values(&self) -> &[u16; PARAMETERS] {
        &self.values
    }
    pub fn residual_index(&self) -> u16 {
        self.values[RESIDUAL_PARAMETER]
    }
    pub fn lsf_indices(&self) -> [u16; 3] {
        self.values[LSF_START_PARAMETER..LSF_END_PARAMETER]
            .try_into()
            .expect("SID contains three split spectral indices")
    }
    pub fn log_energy_q11(&self) -> i16 {
        if self.values[ENERGY_PARAMETER] == 0 {
            i16::MIN
        } else {
            ((self.values[ENERGY_PARAMETER] as i16) << ENERGY_SCALE_SHIFT)
                .saturating_sub(ENERGY_OFFSET_Q11)
        }
    }
    pub fn prediction_energy_q10(&self) -> i16 {
        prediction_energy(self.log_energy_q11())
    }
    pub fn codec_bits(&self) -> [bool; CODEC_BITS] {
        let mut output = [false; CODEC_BITS];
        let mut position = 0;
        for (&value, &width) in self.values.iter().zip(&WIDTHS) {
            for bit in (0..width).rev() {
                output[position] = value & (1 << bit) != 0;
                position += 1;
            }
        }
        output
    }
    pub fn from_codec_bits(bits: &[bool; CODEC_BITS]) -> Self {
        let mut position = 0;
        let values = std::array::from_fn(|index| {
            let mut value = 0;
            for _ in 0..WIDTHS[index] {
                value = (value << 1) | u16::from(bits[position]);
                position += 1;
            }
            value
        });
        Self { values }
    }
}
pub(super) fn prediction_energy(log_energy_q11: i16) -> i16 {
    (log_energy_q11 >> 1)
        .saturating_sub(PREDICTION_OFFSET_Q10)
        .clamp(MIN_PREDICTION_Q10, 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mr74::spectral::SpectralDecoder;
    #[test]
    fn sid_spectra_energy_and_serialization_match_reference_for_all_initializations_and_split_entries()
     {
        let mut hash = 0xcbf29ce484222325u64;
        for initial in 0..8 {
            for (part, entries) in [256, 512, 512].into_iter().enumerate() {
                for index in 0..entries {
                    let mut values = [
                        initial,
                        0,
                        0,
                        0,
                        ((index + usize::from(initial) + part) % 64) as u16,
                    ];
                    values[part + 1] = index as u16;
                    let parameters = SidParameters::try_new(values).unwrap();
                    let bits = parameters.codec_bits();
                    assert_eq!(SidParameters::from_codec_bits(&bits), parameters);
                    let spectrum = SpectralDecoder::default().decode_reference_sid(&parameters);
                    for value in spectrum
                        .lsf_q15
                        .into_iter()
                        .chain(spectrum.lsp_q15)
                        .chain([
                            parameters.log_energy_q11(),
                            parameters.prediction_energy_q10(),
                        ])
                        .chain(bits.map(i16::from))
                    {
                        for byte in value.to_le_bytes() {
                            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                        }
                    }
                }
            }
        }
        assert_eq!(hash, 0xfd151ee0725e8826);
    }
    #[test]
    fn every_sid_field_rejects_out_of_width_values() {
        for (index, &width) in WIDTHS.iter().enumerate() {
            let mut values = [0; PARAMETERS];
            values[index] = 1 << width;
            assert_eq!(
                SidParameters::try_new(values),
                Err(SidParameterError {
                    index,
                    value: values[index],
                    maximum: values[index] - 1
                })
            );
        }
        let silence = SidParameters::try_new([0; PARAMETERS]).unwrap();
        assert_eq!(silence.log_energy_q11(), i16::MIN);
        assert_eq!(silence.prediction_energy_q10(), MIN_PREDICTION_Q10);
    }
    #[test]
    fn sid_application_clears_residual_prediction_before_good_speech() {
        use crate::codec::mr74::{PARAMETER_COUNT, Parameters};
        let mut values = [0; PARAMETER_COUNT];
        values[..3].copy_from_slice(&[255, 511, 511]);
        let speech = Parameters::try_new(values).unwrap();
        let mut decoder = SpectralDecoder::default();
        decoder.decode(&speech);
        decoder.decode_reference_sid(&SidParameters::try_new([7, 255, 511, 511, 63]).unwrap());
        assert_eq!(
            decoder.decode(&speech),
            SpectralDecoder::default().decode(&speech)
        );
    }
}
