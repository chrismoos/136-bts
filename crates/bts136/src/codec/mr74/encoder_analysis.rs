use super::{
    analysis_spectrum::from_predictor,
    decoder::{FRAME_SAMPLES, PCM_LOW_BITS_MASK},
    levinson::Levinson,
    lpc::{COEFFICIENTS, INITIAL_LSP_Q15},
    preprocess::Preprocess,
    spectral::{
        ConversionError, LPC_ORDER,
        quantization::{QuantizedSpectrum, Quantizer},
    },
};

const BUFFER_SAMPLES: usize = FRAME_SAMPLES * 2;
const WINDOW_SAMPLES: usize = super::autocorrelation::WINDOW_SAMPLES;
const LOOKAHEAD_SAMPLES: usize = 40;
const WINDOW_START: usize = BUFFER_SAMPLES - WINDOW_SAMPLES;
const PRESENT_START: usize = FRAME_SAMPLES - LOOKAHEAD_SAMPLES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzedFrame {
    /// The 160-sample coding interval follows preprocessing and precedes the 40-sample lookahead.
    pub samples_q0: [i16; FRAME_SAMPLES],
    pub coefficients_q12: [i16; COEFFICIENTS],
    pub lsp_q15: [i16; LPC_ORDER],
    pub quantized: QuantizedSpectrum,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analyzer {
    preprocessing: Preprocess,
    buffer: [i16; BUFFER_SAMPLES],
    predictor: Levinson,
    previous_lsp_q15: [i16; LPC_ORDER],
    quantizer: Quantizer,
}
impl Default for Analyzer {
    fn default() -> Self {
        Self {
            preprocessing: Preprocess::default(),
            buffer: [0; BUFFER_SAMPLES],
            predictor: Levinson::default(),
            previous_lsp_q15: INITIAL_LSP_Q15,
            quantizer: Quantizer::default(),
        }
    }
}
impl Analyzer {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 Speech_Encode_Frame and cod_amr: 13-bit input precision and a zero-initialized 40-sample analysis delay.
    pub fn push(
        &mut self,
        samples: &[i16; FRAME_SAMPLES],
    ) -> Result<AnalyzedFrame, ConversionError> {
        let mut next = self.clone();
        let output = next.analyze(samples)?;
        *self = next;
        Ok(output)
    }
    fn analyze(
        &mut self,
        samples: &[i16; FRAME_SAMPLES],
    ) -> Result<AnalyzedFrame, ConversionError> {
        let mut filtered = samples.map(|s| s & !PCM_LOW_BITS_MASK);
        self.preprocessing.process(&mut filtered);
        self.buffer[FRAME_SAMPLES..].copy_from_slice(&filtered);
        let window: &[i16; WINDOW_SAMPLES] = self.buffer[WINDOW_START..]
            .try_into()
            .expect("fixed LPC analysis interval");
        let predictor = self.predictor.analyze(window);
        let lsp_q15 = from_predictor(&predictor.coefficients_q12, &self.previous_lsp_q15);
        let quantized = self.quantizer.quantize(&lsp_q15)?;
        self.previous_lsp_q15 = lsp_q15;
        let samples_q0 = self.buffer[PRESENT_START..PRESENT_START + FRAME_SAMPLES]
            .try_into()
            .expect("fixed speech coding interval");
        self.buffer.copy_within(FRAME_SAMPLES.., 0);
        Ok(AnalyzedFrame {
            samples_q0,
            coefficients_q12: predictor.coefficients_q12,
            lsp_q15,
            quantized,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consecutive_pcm_analysis_matches_fixed_point_reference_and_reset() {
        let mut hash = 0xcbf29ce484222325u64;
        for pattern in 0..7 {
            let mut analyzer = Analyzer::default();
            for frame in 0..16 {
                let samples = std::array::from_fn(|i| {
                    let i = frame * FRAME_SAMPLES + i;
                    match pattern {
                        0 => 0,
                        1 => {
                            if i == 0 {
                                30000
                            } else {
                                0
                            }
                        }
                        2 => {
                            if i.is_multiple_of(2) {
                                i16::MAX
                            } else {
                                i16::MIN
                            }
                        }
                        3 => i16::MAX,
                        4 => i16::MIN,
                        5 => ((i * 347 + 997) % 65536) as i16,
                        _ => ((i * 73) % 1024) as i16 - 512,
                    }
                });
                let result = analyzer.push(&samples).unwrap();
                for value in result
                    .samples_q0
                    .into_iter()
                    .chain(result.coefficients_q12)
                    .chain(result.lsp_q15)
                    .chain(result.quantized.indices.map(|i| i as i16))
                    .chain(result.quantized.spectrum.lsp_q15)
                {
                    for byte in value.to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
            }
            analyzer.reset();
            assert_eq!(analyzer, Analyzer::default());
        }
        assert_eq!(hash, 0x379f0c011cce46be);
    }
}
