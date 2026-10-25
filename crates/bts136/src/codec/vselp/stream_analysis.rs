use super::{
    FRAME_SAMPLES, LPC_COEFFICIENTS, SUBFRAME_SAMPLES,
    analysis::{ANALYSIS_SAMPLES, AnalysisError, analyze_energy, analyze_predictor},
};

const ANALYSIS_START: usize = FRAME_SAMPLES - SUBFRAME_SAMPLES / 2 - ANALYSIS_SAMPLES / 2;
pub const LOOKAHEAD_SAMPLES: usize = ANALYSIS_START + ANALYSIS_SAMPLES - FRAME_SAMPLES;
const PENDING_ANALYSIS_SAMPLES: usize = ANALYSIS_SAMPLES - LOOKAHEAD_SAMPLES;

#[derive(Debug, Clone, PartialEq)]
pub struct AnalyzedFrame {
    pub samples: [f64; FRAME_SAMPLES],
    pub r0: u8,
    pub lpc: [u8; LPC_COEFFICIENTS],
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamAnalyzer {
    pending: Option<[f64; FRAME_SAMPLES]>,
}
impl StreamAnalyzer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        self.pending = None;
    }
    /// IS-54-B §§2.1.3.3.2.4–5 center analysis on subframe four, requiring 65 samples of lookahead.
    pub fn push(
        &mut self,
        input: &[f64; FRAME_SAMPLES],
    ) -> Result<Option<AnalyzedFrame>, AnalysisError> {
        if let Some(sample) = input.iter().position(|sample| !sample.is_finite()) {
            return Err(AnalysisError::NonFiniteSample { sample });
        }
        let output = if let Some(pending) = self.pending {
            let mut interval = [0.0; ANALYSIS_SAMPLES];
            interval[..PENDING_ANALYSIS_SAMPLES].copy_from_slice(&pending[ANALYSIS_START..]);
            interval[PENDING_ANALYSIS_SAMPLES..].copy_from_slice(&input[..LOOKAHEAD_SAMPLES]);
            Some(AnalyzedFrame {
                samples: pending,
                r0: analyze_energy(&interval)?,
                lpc: analyze_predictor(&interval)?,
            })
        } else {
            None
        };
        self.pending = Some(*input);
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consecutive_frames_use_exact_analysis_alignment_and_return_the_original_target_frame() {
        let all: [f64; FRAME_SAMPLES * 3] =
            std::array::from_fn(|sample| ((sample * 37 % 101) as f64 - 50.0) / 64.0);
        let mut analyzer = StreamAnalyzer::new();
        let frames = all.as_chunks::<FRAME_SAMPLES>().0;
        assert_eq!(analyzer.push(&frames[0]).unwrap(), None);
        for frame in 1..frames.len() {
            let output = analyzer.push(&frames[frame]).unwrap().unwrap();
            let start = (frame - 1) * FRAME_SAMPLES + 55;
            let expected: [f64; ANALYSIS_SAMPLES] = all[start..start + 170].try_into().unwrap();
            assert_eq!(output.samples, frames[frame - 1]);
            assert_eq!(output.r0, analyze_energy(&expected).unwrap());
            assert_eq!(output.lpc, analyze_predictor(&expected).unwrap());
        }
    }
    #[test]
    fn lookahead_includes_the_last_required_sample_and_excludes_the_following_sample() {
        let silence = [0.0; FRAME_SAMPLES];
        for (sample, expected_r0) in [(64, 20), (65, 0)] {
            let mut analyzer = StreamAnalyzer::new();
            analyzer.push(&silence).unwrap();
            let mut next = silence;
            next[sample] = 1.0;
            let result = analyzer.push(&next).unwrap().unwrap();
            assert_eq!(result.samples, silence);
            assert_eq!(result.r0, expected_r0);
        }
    }
    #[test]
    fn rejected_input_preserves_pending_frame_and_reset_discards_lookahead_history() {
        let first = [0.001; FRAME_SAMPLES];
        let second = [0.002; FRAME_SAMPLES];
        let mut analyzer = StreamAnalyzer::new();
        analyzer.push(&first).unwrap();
        let before = analyzer.clone();
        let mut invalid = second;
        invalid[159] = f64::NAN;
        assert_eq!(
            analyzer.push(&invalid),
            Err(AnalysisError::NonFiniteSample { sample: 159 })
        );
        assert_eq!(analyzer, before);
        assert_eq!(analyzer.push(&second).unwrap().unwrap().samples, first);
        analyzer.reset();
        assert_eq!(analyzer.push(&second).unwrap(), None);
        assert_eq!(analyzer.push(&first).unwrap().unwrap().samples, second);
    }
}
