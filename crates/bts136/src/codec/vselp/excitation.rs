use super::{MAX_LAG_SAMPLES, SUBFRAME_SAMPLES, VselpSubframe};
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryError {
    NonFinite { sample: usize },
}
impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid long-term synthesis output: {self:?}")
    }
}
impl Error for HistoryError {}
#[derive(Debug, Clone, PartialEq)]
pub struct LongTermHistory {
    samples: [f64; MAX_LAG_SAMPLES],
}
impl Default for LongTermHistory {
    fn default() -> Self {
        Self {
            samples: [0.0; MAX_LAG_SAMPLES],
        }
    }
}
impl LongTermHistory {
    pub fn new() -> Self {
        Self::default()
    }
    // IS-54-B §2.1.3.3.2.6.3.1-15 uses only filter states present before the subframe.
    pub fn excitation(&self, subframe: &VselpSubframe) -> [f64; SUBFRAME_SAMPLES] {
        let Some(lag_samples) = subframe.pitch_lag_samples() else {
            return [0.0; SUBFRAME_SAMPLES];
        };
        std::array::from_fn(|sample| {
            let delay_samples = ((sample + lag_samples) / lag_samples) * lag_samples;
            self.samples[MAX_LAG_SAMPLES + sample - delay_samples]
        })
    }
    /// Stores long-term synthesis output before short-term filtering, in chronological sample order.
    pub fn commit(&mut self, output: &[f64; SUBFRAME_SAMPLES]) -> Result<(), HistoryError> {
        if let Some(sample) = output.iter().position(|sample| !sample.is_finite()) {
            return Err(HistoryError::NonFinite { sample });
        }
        self.samples.copy_within(SUBFRAME_SAMPLES.., 0);
        self.samples[MAX_LAG_SAMPLES - SUBFRAME_SAMPLES..].copy_from_slice(output);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vselp::{LAG_CODE_OFFSET, MIN_LAG_SAMPLES, SUBFRAMES};
    fn populated() -> LongTermHistory {
        let mut history = LongTermHistory::new();
        for subframe in 0..SUBFRAMES {
            history
                .commit(&std::array::from_fn(|sample| {
                    (subframe * SUBFRAME_SAMPLES + sample) as f64
                }))
                .unwrap();
        }
        history
    }
    fn subframe(lag_samples: usize) -> VselpSubframe {
        VselpSubframe::try_new((lag_samples - LAG_CODE_OFFSET) as u8, 0, 0, 0).unwrap()
    }
    #[test]
    fn every_lag_code_maps_to_disabled_or_the_exact_sample_delay() {
        assert_eq!(VselpSubframe::default().pitch_lag_samples(), None);
        for code in 1..=127 {
            assert_eq!(
                VselpSubframe::try_new(code, 0, 0, 0)
                    .unwrap()
                    .pitch_lag_samples(),
                Some(usize::from(code) + LAG_CODE_OFFSET)
            );
        }
        assert_eq!(subframe(MIN_LAG_SAMPLES).pitch_lag_samples(), Some(20));
        assert_eq!(subframe(MAX_LAG_SAMPLES).pitch_lag_samples(), Some(146));
    }
    #[test]
    fn short_lags_repeat_preexisting_state_and_long_lags_use_the_oldest_retained_samples() {
        let history = populated();
        let short = history.excitation(&subframe(MIN_LAG_SAMPLES));
        assert_eq!(
            short,
            std::array::from_fn(|sample| (140 + sample % 20) as f64)
        );
        let equal = history.excitation(&subframe(SUBFRAME_SAMPLES));
        assert_eq!(equal, std::array::from_fn(|sample| (120 + sample) as f64));
        let longest = history.excitation(&subframe(MAX_LAG_SAMPLES));
        assert_eq!(longest, std::array::from_fn(|sample| (14 + sample) as f64));
        assert_eq!(
            history.excitation(&VselpSubframe::default()),
            [0.0; SUBFRAME_SAMPLES]
        );
    }
    #[test]
    fn excitation_is_read_only_and_nonfinite_commit_preserves_all_history() {
        let mut history = populated();
        let before = history.clone();
        history.excitation(&subframe(MIN_LAG_SAMPLES));
        assert_eq!(history, before);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut output = [1.0; SUBFRAME_SAMPLES];
            output[3] = bad;
            assert_eq!(
                history.commit(&output),
                Err(HistoryError::NonFinite { sample: 3 })
            );
            assert_eq!(history, before);
        }
        history.commit(&[7.0; SUBFRAME_SAMPLES]).unwrap();
        assert_eq!(
            history.excitation(&subframe(MIN_LAG_SAMPLES)),
            [7.0; SUBFRAME_SAMPLES]
        );
    }
}
