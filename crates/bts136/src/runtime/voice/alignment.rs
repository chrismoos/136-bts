use crate::l3::legacy_traffic::MAX_ALIGNMENT_UNITS;
const OBSERVATIONS: usize = 5;
const TARGET_RESET_SYMBOLS: f64 = 0.25;
const HALF_SYMBOLS_PER_SYMBOL: f64 = 2.0;
pub(super) const ALIGNMENT_TOLERANCE_SYMBOLS: f64 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InitialAlignment {
    Shortened,
    Commanded,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimingAction {
    Adjust(u8),
    OutOfRange,
}

#[derive(Default)]
pub(super) struct TimingTracker {
    samples: [f64; OBSERVATIONS],
    count: usize,
    confirmed_units: Option<u8>,
    segment: Option<u64>,
    pub manual: bool,
    pub target_symbols: f64,
    pub residual_symbols: Option<f64>,
    pub saturated: bool,
}
impl TimingTracker {
    pub fn set_manual(&mut self, manual: bool) {
        self.manual = manual;
        self.count = 0;
        self.residual_symbols = None;
        self.saturated = false;
    }
    pub fn set_target(&mut self, target: f64) {
        if (self.target_symbols - target).abs() > TARGET_RESET_SYMBOLS {
            self.count = 0;
        }
        self.target_symbols = target;
    }
    pub fn initial_units(residual_symbols: f64) -> u8 {
        (residual_symbols * HALF_SYMBOLS_PER_SYMBOL)
            .round()
            .clamp(0.0, f64::from(MAX_ALIGNMENT_UNITS)) as u8
    }
    pub fn observe(
        &mut self,
        residual_symbols: f64,
        confirmed_units: u8,
        segment: u64,
    ) -> Option<TimingAction> {
        if !residual_symbols.is_finite() {
            return None;
        }
        if self.confirmed_units != Some(confirmed_units) || self.segment != Some(segment) {
            self.count = 0;
            self.confirmed_units = Some(confirmed_units);
            self.segment = Some(segment);
        }
        self.samples[self.count] = residual_symbols;
        self.count += 1;
        if self.count < OBSERVATIONS {
            return None;
        }
        self.count = 0;
        let mut ordered = self.samples;
        ordered.sort_unstable_by(f64::total_cmp);
        let residual = ordered[OBSERVATIONS / 2];
        self.residual_symbols = Some(residual);
        self.saturated = false;
        let error = residual - self.target_symbols;
        if self.manual || error.abs() <= ALIGNMENT_TOLERANCE_SYMBOLS {
            return None;
        }
        let desired = f64::from(confirmed_units) + error * HALF_SYMBOLS_PER_SYMBOL;
        self.saturated = !(0.0..=f64::from(MAX_ALIGNMENT_UNITS)).contains(&desired);
        let units = desired.round().clamp(0.0, f64::from(MAX_ALIGNMENT_UNITS)) as u8;
        if units != confirmed_units {
            Some(TimingAction::Adjust(units))
        } else if self.saturated {
            Some(TimingAction::OutOfRange)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timing_tracking_uses_stable_absolute_offsets_and_rejects_outliers() {
        let mut tracker = TimingTracker::default();
        for sample in [2.0, 2.1, 15.0, 1.9] {
            assert_eq!(tracker.observe(sample, 4, 0), None);
        }
        assert_eq!(tracker.observe(2.0, 4, 0), Some(TimingAction::Adjust(8)));
        for _ in 0..OBSERVATIONS {
            assert_eq!(tracker.observe(0.2, 8, 0), None);
        }
        for _ in 0..OBSERVATIONS - 1 {
            assert_eq!(tracker.observe(-2.0, 8, 0), None);
        }
        assert_eq!(tracker.observe(-2.0, 8, 0), Some(TimingAction::Adjust(4)));
    }
    #[test]
    fn timing_tracking_resets_on_segment_or_ack_and_respects_manual_mode_and_limits() {
        let mut tracker = TimingTracker::default();
        for _ in 0..4 {
            assert_eq!(tracker.observe(3.0, 0, 0), None);
        }
        assert_eq!(tracker.observe(3.0, 0, 1), None);
        assert_eq!(tracker.observe(3.0, 2, 1), None);
        tracker.manual = true;
        for _ in 0..OBSERVATIONS {
            assert_eq!(tracker.observe(3.0, 2, 1), None);
        }
        tracker.set_manual(false);
        for _ in 0..OBSERVATIONS - 1 {
            assert_eq!(tracker.observe(1.0, 2, 1), None);
        }
        assert_eq!(tracker.observe(1.0, 2, 1), Some(TimingAction::Adjust(4)));
        let mut tracker = TimingTracker::default();
        for _ in 0..OBSERVATIONS - 1 {
            assert_eq!(tracker.observe(2.0, 30, 0), None);
        }
        assert_eq!(tracker.observe(2.0, 30, 0), Some(TimingAction::OutOfRange));
        assert!(tracker.saturated);
        assert_eq!(TimingTracker::initial_units(2.25), 5);
        assert_eq!(TimingTracker::initial_units(20.0), 30);
        assert_eq!(TimingTracker::initial_units(-1.0), 0);
    }
}

#[cfg(test)]
mod range_tests {
    use super::*;
    #[test]
    fn range_failure_requires_confirmed_limit_and_stable_measurements() {
        let mut tracker = TimingTracker::default();
        for _ in 0..OBSERVATIONS - 1 {
            assert_eq!(tracker.observe(3.0, 28, 0), None);
        }
        assert_eq!(tracker.observe(3.0, 28, 0), Some(TimingAction::Adjust(30)));
        for _ in 0..OBSERVATIONS - 1 {
            assert_eq!(tracker.observe(2.0, 30, 0), None);
        }
        assert_eq!(tracker.observe(2.0, 30, 0), Some(TimingAction::OutOfRange));
        tracker.set_manual(true);
        for _ in 0..OBSERVATIONS {
            assert_eq!(tracker.observe(2.0, 30, 0), None);
        }
        tracker.set_manual(false);
        for _ in 0..OBSERVATIONS - 1 {
            assert_eq!(tracker.observe(-2.0, 0, 0), None);
        }
        assert_eq!(tracker.observe(-2.0, 0, 0), Some(TimingAction::OutOfRange));
    }
}

#[cfg(test)]
mod overlap_target_tests {
    use super::*;
    #[test]
    fn intentional_early_arrival_is_retained_while_new_drift_is_corrected() {
        let mut tracker = TimingTracker::default();
        tracker.set_target(-2.5);
        for _ in 0..OBSERVATIONS {
            assert_eq!(tracker.observe(-2.5, 9, 0), None);
        }
        for _ in 0..OBSERVATIONS - 1 {
            assert_eq!(tracker.observe(-0.5, 9, 0), None);
        }
        assert_eq!(tracker.observe(-0.5, 9, 0), Some(TimingAction::Adjust(13)));
    }
}
