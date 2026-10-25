use super::{
    LSF_INDICES, PARAMETER_COUNT, Parameters, SUBFRAME_SAMPLES, SUBFRAMES,
    adaptive::{self, HISTORY_SAMPLES},
    codebook::search,
    decoder::FRAME_SAMPLES,
    encoder_analysis::Analyzer,
    encoder_filtering::{self, WeightedSpeechFilter},
    encoder_stability::Stability,
    gain::quantization::{self, Quantizer},
    lpc::{self, COEFFICIENTS, INITIAL_LSP_Q15, LpcDecoder},
    pitch::{
        open_loop::Detector,
        search::{Search, Subframe},
    },
    spectral::{ConversionError, LPC_ORDER},
    synthesis::SynthesisFilter,
};
use std::{error::Error, fmt};
const SHARPENING_MAX_Q14: i16 = 13017;
const WORD_SHIFT: u32 = 16;
const ROUND_BIAS: i32 = 1 << (WORD_SHIFT - 1);
const PITCH_GAIN_SHIFT: u32 = 1;
const CODE_GAIN_SHIFT: u32 = 2;
const PARAMETERS_PER_SUBFRAME: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub parameters: Parameters,
    pub synthesis_q0: [i16; FRAME_SAMPLES],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    Analysis(ConversionError),
    Pitch(super::pitch::search::SearchError),
}
impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MR74 encoding failed: {self:?}")
    }
}
impl Error for EncodeError {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoder {
    analyzer: Analyzer,
    previous_lsp_q15: [i16; LPC_ORDER],
    quantized_lpc: LpcDecoder,
    weighted: WeightedSpeechFilter,
    open_loop: Detector,
    pitch: Search,
    gains: Quantizer,
    stability: Stability,
    speech_history_q0: [i16; LPC_ORDER],
    excitation_history_q0: [i16; HISTORY_SAMPLES],
    error_history_q0: [i16; LPC_ORDER],
    weighting_history_q0: [i16; LPC_ORDER],
    synthesis: SynthesisFilter,
    sharpening_q14: i16,
}
impl Default for Encoder {
    fn default() -> Self {
        Self {
            analyzer: Analyzer::default(),
            previous_lsp_q15: INITIAL_LSP_Q15,
            quantized_lpc: LpcDecoder::default(),
            weighted: WeightedSpeechFilter::default(),
            open_loop: Detector::default(),
            pitch: Search::default(),
            gains: Quantizer::default(),
            stability: Stability::default(),
            speech_history_q0: [0; LPC_ORDER],
            excitation_history_q0: [0; HISTORY_SAMPLES],
            error_history_q0: [0; LPC_ORDER],
            weighting_history_q0: [0; LPC_ORDER],
            synthesis: SynthesisFilter::default(),
            sharpening_q14: 0,
        }
    }
}
impl Encoder {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// TS 26.073 cod_amr in MR74 mode without DTX. Input is signed 8 kHz PCM and the coding interval has a 40-sample delay.
    pub fn encode(&mut self, samples: &[i16; FRAME_SAMPLES]) -> Result<EncodedFrame, EncodeError> {
        let mut next = self.clone();
        let frame = next.encode_frame(samples)?;
        *self = next;
        Ok(frame)
    }
    fn encode_frame(
        &mut self,
        samples: &[i16; FRAME_SAMPLES],
    ) -> Result<EncodedFrame, EncodeError> {
        let analyzed = self.analyzer.push(samples).map_err(EncodeError::Analysis)?;
        let mut values = [0u16; PARAMETER_COUNT];
        values[..LSF_INDICES].copy_from_slice(&analyzed.quantized.indices);
        let header =
            Parameters::try_new(values).expect("spectral quantization produces bounded indices");
        let quantized = self.quantized_lpc.decode(&header).coefficients_q12;
        let mut coefficients = interpolate(&self.previous_lsp_q15, &analyzed.lsp_q15);
        // TS 26.073 Int_lpc_1to3_2 preserves the directly analyzed fourth-subframe coefficients.
        coefficients[SUBFRAMES - 1] = analyzed.coefficients_q12;
        self.previous_lsp_q15 = analyzed.lsp_q15;
        let resonant = self.stability.observe_spectrum(&analyzed.lsp_q15);
        let mut speech = [0i16; LPC_ORDER + FRAME_SAMPLES];
        speech[..LPC_ORDER].copy_from_slice(&self.speech_history_q0);
        speech[LPC_ORDER..].copy_from_slice(&analyzed.samples_q0);
        let mut weighted = [0i16; FRAME_SAMPLES];
        for (sf, coefficient) in coefficients.iter().enumerate() {
            let start = sf * SUBFRAME_SAMPLES;
            let input = speech[start..start + encoder_filtering::INPUT_SAMPLES]
                .try_into()
                .expect("each subframe retains ten speech samples");
            weighted[start..start + SUBFRAME_SAMPLES]
                .copy_from_slice(&self.weighted.filter(input, coefficient));
        }
        let open_loop = std::array::from_fn(|half| {
            self.open_loop.push(
                weighted[half * (FRAME_SAMPLES / 2)..(half + 1) * (FRAME_SAMPLES / 2)]
                    .try_into()
                    .expect("fixed half-frame weighted speech interval"),
            )
        });
        let mut synthesis_q0 = [0; FRAME_SAMPLES];
        for (sf, subframe) in [
            Subframe::One,
            Subframe::Two,
            Subframe::Three,
            Subframe::Four,
        ]
        .into_iter()
        .enumerate()
        {
            let start = sf * SUBFRAME_SAMPLES;
            let input = speech[start..start + encoder_filtering::INPUT_SAMPLES]
                .try_into()
                .expect("subframe speech interval");
            let prepared = encoder_filtering::prepare(
                input,
                &coefficients[sf],
                &quantized[sf],
                &self.error_history_q0,
                &self.weighting_history_q0,
            );
            let mut excitation = [0; HISTORY_SAMPLES + SUBFRAME_SAMPLES];
            excitation[..HISTORY_SAMPLES].copy_from_slice(&self.excitation_history_q0);
            excitation[HISTORY_SAMPLES..].copy_from_slice(&prepared.residual_q0);
            let pitch = self
                .pitch
                .search(
                    &excitation,
                    &prepared.target_q0,
                    &prepared.impulse_q12,
                    open_loop,
                    subframe,
                )
                .map_err(EncodeError::Pitch)?;
            let adaptive = adaptive::predict(&self.excitation_history_q0, pitch.lag);
            let filtered = super::pitch::search::convolve(&adaptive, &prepared.impulse_q12);
            let gain = quantization::pitch_gain(&prepared.target_q0, &filtered);
            let limit = self.stability.limit(resonant, gain.gain_q14);
            let unquantized_pitch = gain.gain_q14.min(limit.pitch_q14());
            let target = std::array::from_fn(|i| {
                prepared.target_q0[i].saturating_sub(high(shift(
                    product(filtered[i], unquantized_pitch),
                    PITCH_GAIN_SHIFT,
                )))
            });
            let code = search::search(
                &target,
                &prepared.impulse_q12,
                pitch.lag,
                self.sharpening_q14,
            );
            let quantized_gain = self.gains.quantize(
                &prepared.target_q0,
                &filtered,
                &code.filtered_q12,
                &code.innovation_q13,
                limit,
            );
            let gains = quantized_gain.gains;
            self.stability.commit_gain(gains.pitch_q14);
            self.sharpening_q14 = gains.pitch_q14.min(SHARPENING_MAX_Q14);
            let combined = std::array::from_fn(|i| {
                let sum = product(adaptive[i], gains.pitch_q14)
                    .saturating_add(product(code.innovation_q13[i], gains.code_q1));
                rounded(shift(sum, PITCH_GAIN_SHIFT))
            });
            let synthesized = self.synthesis.filter(&quantized[sf], &combined).samples_q0;
            synthesis_q0[start..start + SUBFRAME_SAMPLES].copy_from_slice(&synthesized);
            for j in 0..LPC_ORDER {
                let i = SUBFRAME_SAMPLES - LPC_ORDER + j;
                self.error_history_q0[j] =
                    analyzed.samples_q0[start + i].saturating_sub(synthesized[i]);
                let adaptive = high(shift(
                    product(filtered[i], gains.pitch_q14),
                    PITCH_GAIN_SHIFT,
                ));
                let innovation = high(shift(
                    product(code.filtered_q12[i], gains.code_q1),
                    CODE_GAIN_SHIFT,
                ));
                self.weighting_history_q0[j] =
                    prepared.target_q0[i].saturating_sub(adaptive.saturating_add(innovation));
            }
            self.excitation_history_q0
                .copy_within(SUBFRAME_SAMPLES.., 0);
            self.excitation_history_q0[HISTORY_SAMPLES - SUBFRAME_SAMPLES..]
                .copy_from_slice(&combined);
            let offset = LSF_INDICES + sf * PARAMETERS_PER_SUBFRAME;
            values[offset..offset + PARAMETERS_PER_SUBFRAME].copy_from_slice(&[
                u16::from(pitch.index),
                code.pulse_positions,
                code.pulse_signs,
                quantized_gain.index,
            ]);
        }
        self.speech_history_q0
            .copy_from_slice(&analyzed.samples_q0[FRAME_SAMPLES - LPC_ORDER..]);
        Ok(EncodedFrame {
            parameters: Parameters::try_new(values)
                .expect("encoder search indices fit their parameter fields"),
            synthesis_q0,
        })
    }
}
fn interpolate(
    previous: &[i16; LPC_ORDER],
    current: &[i16; LPC_ORDER],
) -> [[i16; COEFFICIENTS]; SUBFRAMES] {
    std::array::from_fn(|sf| {
        let cosines = std::array::from_fn(|i| match sf {
            0 => (current[i] >> 2).saturating_add(previous[i].saturating_sub(previous[i] >> 2)),
            1 => (previous[i] >> 1).saturating_add(current[i] >> 1),
            2 => (previous[i] >> 2).saturating_add(current[i].saturating_sub(current[i] >> 2)),
            _ => current[i],
        });
        lpc::convert(&cosines)
    })
}
fn product(left: i16, right: i16) -> i32 {
    (i32::from(left) * i32::from(right)).saturating_mul(2)
}
fn shift(value: i32, shift: u32) -> i32 {
    (i64::from(value) << shift).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}
fn high(value: i32) -> i16 {
    (value >> WORD_SHIFT) as i16
}
fn rounded(value: i32) -> i16 {
    high(value.saturating_add(ROUND_BIAS))
}
#[cfg(test)]
mod tests;
