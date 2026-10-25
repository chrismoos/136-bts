use super::{
    FRAME_SAMPLES, ParameterError, SUBFRAME_SAMPLES, SUBFRAMES, VselpParameters, VselpSubframe,
    analysis::AnalysisError,
    excitation::{HistoryError, LongTermHistory},
    excitation_search, gain_search,
    gains::PitchPrediction,
    pitch,
    stream_analysis::StreamAnalyzer,
    synthesis::SynthesisError,
    weighting::{PerceptualFilter, WeightedSynthesisFilter},
};
use std::{error::Error, fmt};

const DISABLED_LAG_CODE: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    Analysis(AnalysisError),
    Parameters(ParameterError),
    Perceptual {
        subframe: usize,
        error: SynthesisError,
    },
    Synthesis {
        subframe: usize,
        error: SynthesisError,
    },
    Pitch {
        subframe: usize,
        error: pitch::PitchError,
    },
    Codebook {
        subframe: usize,
        error: excitation_search::SearchError,
    },
    Gain {
        subframe: usize,
        error: gain_search::SearchError,
    },
    History {
        subframe: usize,
        error: HistoryError,
    },
    NonFiniteTarget {
        subframe: usize,
        sample: usize,
    },
}
impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP frame encoding failed: {self:?}")
    }
}
impl Error for EncodeError {}

#[derive(Debug, Clone, PartialEq)]
pub struct Encoder {
    analyzer: StreamAnalyzer,
    previous: VselpParameters,
    long_term: LongTermHistory,
    perceptual: PerceptualFilter,
    synthesis: WeightedSynthesisFilter,
}
impl Encoder {
    /// Seeds predictor/energy interpolation from the preceding encoded frame with zero filter histories.
    pub fn new(previous: VselpParameters) -> Self {
        Self {
            analyzer: StreamAnalyzer::new(),
            previous,
            long_term: LongTermHistory::new(),
            perceptual: PerceptualFilter::new(),
            synthesis: WeightedSynthesisFilter::new(),
        }
    }
    pub fn reset(&mut self, previous: VselpParameters) {
        *self = Self::new(previous);
    }
    /// Accepts normalized 8 kHz PCM. The first frame buffers lookahead and later pushes encode the preceding frame.
    pub fn push(
        &mut self,
        samples: &[f64; FRAME_SAMPLES],
    ) -> Result<Option<VselpParameters>, EncodeError> {
        let mut next = self.clone();
        let output = next.encode(samples)?;
        *self = next;
        Ok(output)
    }
    fn encode(
        &mut self,
        samples: &[f64; FRAME_SAMPLES],
    ) -> Result<Option<VselpParameters>, EncodeError> {
        let Some(analyzed) = self.analyzer.push(samples).map_err(EncodeError::Analysis)? else {
            return Ok(None);
        };
        let mut frame = VselpParameters::try_new(
            analyzed.r0,
            analyzed.lpc,
            [VselpSubframe::default(); SUBFRAMES],
        )
        .map_err(EncodeError::Parameters)?;
        let predictors = frame.subframe_predictors(&self.previous);
        for (subframe, (speech, predictor)) in analyzed
            .samples
            .as_chunks::<SUBFRAME_SAMPLES>()
            .0
            .iter()
            .zip(predictors)
            .enumerate()
        {
            let weighted = self
                .perceptual
                .process(&predictor.coefficients, speech)
                .map_err(|error| EncodeError::Perceptual { subframe, error })?;
            let zero_input = self
                .synthesis
                .zero_input_response(&predictor.coefficients)
                .map_err(|error| EncodeError::Synthesis { subframe, error })?;
            let target = std::array::from_fn(|sample| weighted[sample] - zero_input[sample]);
            if let Some(sample) = target.iter().position(|sample| !sample.is_finite()) {
                return Err(EncodeError::NonFiniteTarget { subframe, sample });
            }
            let lag = pitch::search(&self.long_term, &predictor.coefficients, &target)
                .map_err(|error| EncodeError::Pitch { subframe, error })?;
            let codebooks =
                excitation_search::search(&predictor.coefficients, &target, &lag.weighted)
                    .map_err(|error| EncodeError::Codebook { subframe, error })?;
            let pitch = if lag.lag_code == DISABLED_LAG_CODE {
                PitchPrediction::Disabled
            } else {
                PitchPrediction::Enabled
            };
            let gains = gain_search::search(
                &predictor,
                pitch,
                &lag.adaptive,
                &codebooks.excitation,
                &target,
            )
            .map_err(|error| EncodeError::Gain { subframe, error })?;
            frame.subframes[subframe] =
                VselpSubframe::try_new(lag.lag_code, codebooks.code1, codebooks.code2, gains.gsp0)
                    .map_err(EncodeError::Parameters)?;
            // IS-54-B §2.1.3.3.2.6.6 advances both histories with reconstructed, quantized excitation.
            self.long_term
                .commit(&gains.excitation.samples)
                .map_err(|error| EncodeError::History { subframe, error })?;
            self.synthesis
                .process(&predictor.coefficients, &gains.excitation.samples)
                .map_err(|error| EncodeError::Synthesis { subframe, error })?;
        }
        self.previous = frame.clone();
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn samples() -> [f64; FRAME_SAMPLES] {
        std::array::from_fn(|sample| ((sample * 37 % 101) as f64 - 50.0) / 512.0)
    }
    #[test]
    fn reset_reproduces_codes_and_silent_streams_remain_silent() {
        let seed = VselpParameters::default();
        let input = samples();
        let mut encoder = Encoder::new(seed.clone());
        assert_eq!(encoder.push(&input).unwrap(), None);
        let first = encoder.push(&input).unwrap();
        let second = encoder.push(&input).unwrap();
        encoder.reset(seed.clone());
        assert_eq!(encoder.push(&input).unwrap(), None);
        assert_eq!(encoder.push(&input).unwrap(), first);
        assert_eq!(encoder.push(&input).unwrap(), second);
        encoder.reset(seed);
        let zero = [0.0; FRAME_SAMPLES];
        assert_eq!(encoder.push(&zero).unwrap(), None);
        let mut decoder = super::super::decoder::Decoder::new(VselpParameters::default());
        for _ in 0..3 {
            let frame = encoder.push(&zero).unwrap().unwrap();
            assert_eq!(frame.r0(), 0);
            assert_eq!(decoder.decode(&frame).unwrap(), zero);
        }
    }
    #[test]
    fn rejected_frame_preserves_every_filter_and_pending_lookahead_for_retry() {
        let mut encoder = Encoder::new(VselpParameters::default());
        let input = samples();
        encoder.push(&input).unwrap();
        let before = encoder.clone();
        let mut invalid = input;
        invalid[159] = f64::NAN;
        assert!(matches!(
            encoder.push(&invalid),
            Err(EncodeError::Analysis(AnalysisError::NonFiniteSample {
                sample: 159
            }))
        ));
        assert_eq!(encoder, before);
        let expected = before.clone().push(&input).unwrap();
        assert_eq!(encoder.push(&input).unwrap(), expected);
    }
    #[test]
    fn search_failure_after_analysis_and_perceptual_filtering_rolls_back_all_state() {
        let mut encoder = Encoder::new(VselpParameters::default());
        let input = samples();
        encoder.push(&input).unwrap();
        encoder
            .long_term
            .commit(&[f64::MAX; SUBFRAME_SAMPLES])
            .unwrap();
        let before = encoder.clone();
        assert!(matches!(
            encoder.push(&input),
            Err(EncodeError::Pitch { .. })
        ));
        assert_eq!(encoder, before);
    }
}
