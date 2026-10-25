use crate::{
    phy::forward::ForwardModulator,
    scheduler::frame::{FrameClock, FrameError, SlotTiming},
};
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineError {
    ClockRate,
    Range,
    Frame(FrameError),
}
impl fmt::Display for TimelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TX timeline mapping failed: {self:?}")
    }
}
impl Error for TimelineError {}

#[derive(Debug, Clone, Copy)]
pub struct TxTimeline {
    iq_epoch_tick: u64,
    tick_rate_hz: u64,
    sample_rate_hz: u64,
    delay_samples: u64,
    tail_samples: u64,
    samples_per_slot: u64,
    frame: FrameClock,
}
impl TxTimeline {
    /// Nominal epoch is the first symbol center after pulse-filter delay. Leading filter transients precede it.
    pub fn try_new(
        nominal_epoch_tick: u64,
        tick_rate_hz: u64,
        modulator: &ForwardModulator,
    ) -> Result<Self, TimelineError> {
        let sample_rate_hz = modulator.sample_rate_hz() as u64;
        if tick_rate_hz < sample_rate_hz {
            return Err(TimelineError::ClockRate);
        }
        let delay_samples = modulator.delay_samples() as u64;
        let delay_ticks = ticks(delay_samples, tick_rate_hz, sample_rate_hz)?;
        let iq_epoch_tick = nominal_epoch_tick
            .checked_sub(delay_ticks)
            .ok_or(TimelineError::Range)?;
        let frame = FrameClock::try_new(nominal_epoch_tick, tick_rate_hz, sample_rate_hz)
            .map_err(TimelineError::Frame)?;
        Ok(Self {
            iq_epoch_tick,
            tick_rate_hz,
            sample_rate_hz,
            delay_samples,
            tail_samples: modulator.tail_samples() as u64,
            samples_per_slot: modulator.slot_samples() as u64,
            frame,
        })
    }
    pub fn iq_epoch_tick(&self) -> u64 {
        self.iq_epoch_tick
    }
    pub fn frame_clock(&self) -> &FrameClock {
        &self.frame
    }
    pub fn slot(&self, index: u64) -> Result<SlotTiming, TimelineError> {
        self.frame.at(index).map_err(TimelineError::Frame)
    }
    /// Absolute sample rounding can differ by one tick from nominal slot rounding. Never sum rounded block durations.
    pub fn sample_tick(&self, sample_index: u64) -> Result<u64, TimelineError> {
        self.iq_epoch_tick
            .checked_add(ticks(sample_index, self.tick_rate_hz, self.sample_rate_hz)?)
            .ok_or(TimelineError::Range)
    }
    pub fn symbol_center_tick(&self, slot_index: u64) -> Result<u64, TimelineError> {
        let sample = slot_index
            .checked_mul(self.samples_per_slot)
            .and_then(|sample| sample.checked_add(self.delay_samples))
            .ok_or(TimelineError::Range)?;
        self.sample_tick(sample)
    }
    pub fn elapsed_slots_with_tail(
        &self,
        hardware_tick: u64,
        submitted_end_tick: u64,
        submitted_slots: u64,
    ) -> Result<u64, TimelineError> {
        let covered_tick = hardware_tick.min(submitted_end_tick);
        let mut low = 0;
        let mut high = submitted_slots;
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if self.end_tick_with_tail(middle)? <= covered_tick {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        Ok(low)
    }
    pub fn end_tick_with_tail(&self, slots: u64) -> Result<u64, TimelineError> {
        let sample = slots
            .checked_mul(self.samples_per_slot)
            .and_then(|sample| sample.checked_add(self.tail_samples))
            .ok_or(TimelineError::Range)?;
        self.sample_tick(sample)
    }
}
fn ticks(samples: u64, tick_rate_hz: u64, sample_rate_hz: u64) -> Result<u64, TimelineError> {
    u64::try_from(
        (u128::from(samples) * u128::from(tick_rate_hz)).div_ceil(u128::from(sample_rate_hz)),
    )
    .map_err(|_| TimelineError::Range)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_sample_mapping_compensates_delay_without_accumulated_drift() {
        for oversampling in [2, 8, 32] {
            let modulator = ForwardModulator::try_new(oversampling).unwrap();
            for tick_rate in [modulator.sample_rate_hz() as u64, 1_000_003, 122_880_000] {
                let timeline = TxTimeline::try_new(1_000_000, tick_rate, &modulator).unwrap();
                assert_eq!(timeline.symbol_center_tick(0).unwrap(), 1_000_000);
                for index in [0, 1, 2, 5, 6, 150, 1_500_001] {
                    let nominal = timeline.slot(index).unwrap();
                    let center = timeline.symbol_center_tick(index).unwrap();
                    assert!(center <= nominal.start_tick);
                    assert!(nominal.start_tick - center <= 1);
                    let sample =
                        index * modulator.slot_samples() as u64 + modulator.delay_samples() as u64;
                    let expected = timeline.iq_epoch_tick() as u128
                        + (sample as u128 * tick_rate as u128)
                            .div_ceil(modulator.sample_rate_hz() as u128);
                    assert_eq!(center as u128, expected);
                }
                assert_eq!(
                    timeline.end_tick_with_tail(2).unwrap(),
                    timeline
                        .sample_tick(
                            (2 * modulator.slot_samples() + modulator.tail_samples()) as u64
                        )
                        .unwrap()
                );
            }
        }
    }
    #[test]
    fn slot_completion_waits_for_tail_and_submitted_coverage_at_absolute_boundaries() {
        for oversampling in [2, 8, 32] {
            let modulator = ForwardModulator::try_new(oversampling).unwrap();
            for tick_rate_hz in [modulator.sample_rate_hz() as u64, 1_000_003, 122_880_000] {
                let timeline = TxTimeline::try_new(1_000_000, tick_rate_hz, &modulator).unwrap();
                for count in [1, 2, 3, 6, 150, 1_500_001] {
                    let end = timeline.end_tick_with_tail(count).unwrap();
                    assert_eq!(
                        timeline
                            .elapsed_slots_with_tail(end - 1, end, count)
                            .unwrap(),
                        count - 1
                    );
                    assert_eq!(
                        timeline.elapsed_slots_with_tail(end, end, count).unwrap(),
                        count
                    );
                    assert_eq!(
                        timeline
                            .elapsed_slots_with_tail(u64::MAX, end - 1, count)
                            .unwrap(),
                        count - 1
                    );
                    assert_eq!(
                        timeline
                            .elapsed_slots_with_tail(u64::MAX, u64::MAX, count)
                            .unwrap(),
                        count
                    );
                    let block_end = timeline
                        .sample_tick(count * modulator.slot_samples() as u64)
                        .unwrap();
                    assert_eq!(
                        timeline
                            .elapsed_slots_with_tail(block_end, block_end, count)
                            .unwrap(),
                        count - 1
                    );
                }
                assert_eq!(timeline.elapsed_slots_with_tail(0, 0, 0).unwrap(), 0);
            }
        }
    }
    #[test]
    fn impossible_epochs_and_ranges_reject() {
        let modulator = ForwardModulator::try_new(8).unwrap();
        assert!(matches!(
            TxTimeline::try_new(0, 1_000_000, &modulator),
            Err(TimelineError::Range)
        ));
        assert!(matches!(
            TxTimeline::try_new(1_000_000, 1, &modulator),
            Err(TimelineError::ClockRate)
        ));
        let timeline = TxTimeline::try_new(u64::MAX, 1_000_000, &modulator).unwrap();
        assert_eq!(timeline.end_tick_with_tail(1), Err(TimelineError::Range));
        assert_eq!(
            timeline.symbol_center_tick(u64::MAX),
            Err(TimelineError::Range)
        );
        assert_eq!(
            timeline.end_tick_with_tail(u64::MAX),
            Err(TimelineError::Range)
        );
        assert!(timeline.slot(u64::MAX).is_err());
    }
}
