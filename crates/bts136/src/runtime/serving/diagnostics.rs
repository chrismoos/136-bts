use std::time::{Duration, Instant};
const REPORT_INTERVAL: Duration = Duration::from_secs(5);
const SLOW_LOOP: Duration = Duration::from_nanos(
    (crate::phy::traffic::SLOT_BITS / 2) as u64 * 1_000_000_000
        / crate::phy::pulse::SYMBOL_RATE_HZ as u64,
);
const WARNING_INTERVAL: Duration = Duration::from_secs(10);
const SUBMISSION_GUARD: Duration = Duration::from_micros(super::SLOT_SUBMISSION_LEAD_US);
fn tx_budget(queue_depth: usize) -> Duration {
    SLOW_LOOP
        .saturating_mul(queue_depth.saturating_sub(1) as u32)
        .saturating_sub(SUBMISSION_GUARD)
}
fn exhausted_budget(elapsed: Duration, queue_depth: usize) -> bool {
    elapsed > SLOW_LOOP && elapsed >= tx_budget(queue_depth)
}
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Timing {
    pub poll_us: u64,
    pub completions_us: u64,
    pub maintenance_us: u64,
    pub idle_wait_us: u64,
    pub rx_processing_us: u64,
    pub protocol_us: u64,
    pub tx_refill_us: u64,
    pub publication_us: u64,
    pub between_loops_us: u64,
    pub total_us: u64,
}
impl Timing {
    fn maximum(&mut self, other: Self) {
        macro_rules! maximum { ($($field:ident),*) => { $(self.$field = self.$field.max(other.$field);)* }; }
        maximum!(
            poll_us,
            completions_us,
            maintenance_us,
            idle_wait_us,
            rx_processing_us,
            protocol_us,
            tx_refill_us,
            publication_us,
            between_loops_us,
            total_us
        );
    }
}
pub(super) fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}
pub(super) struct Diagnostics {
    pub previous: Timing,
    maximum: Timing,
    last_finish: Instant,
    next_report: Instant,
    last_warning: Option<Instant>,
    iterations: u64,
}
impl Diagnostics {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            previous: Timing::default(),
            maximum: Timing::default(),
            last_finish: now,
            next_report: now + REPORT_INTERVAL,
            last_warning: None,
            iterations: 0,
        }
    }
    pub fn begin(&self, now: Instant) -> Timing {
        Timing {
            between_loops_us: micros(now.duration_since(self.last_finish)),
            ..Default::default()
        }
    }
    pub fn finish(
        &mut self,
        started: Instant,
        mut timing: Timing,
        queue_depth: usize,
        skipped_slots: u64,
    ) {
        let now = Instant::now();
        timing.total_us = micros(now.duration_since(started));
        self.maximum.maximum(timing);
        self.iterations += 1;
        if exhausted_budget(now.duration_since(started), queue_depth)
            && self
                .last_warning
                .is_none_or(|last| now.duration_since(last) >= WARNING_INTERVAL)
        {
            tracing::warn!(
                ?timing,
                queue_depth,
                skipped_slots,
                estimated_tx_budget_us = micros(tx_budget(queue_depth)),
                "Serving loop consumed queued TX scheduling budget"
            );
            self.last_warning = Some(now);
        }
        if now >= self.next_report {
            tracing::debug!(iterations=self.iterations, maximum_us=?self.maximum, queue_depth, skipped_slots,
                "TX scheduling timing summary");
            self.maximum = Timing::default();
            self.iterations = 0;
            self.next_report = now + REPORT_INTERVAL;
        }
        self.previous = timing;
        self.last_finish = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn covered_loops_do_not_warn_and_low_headroom_remains_visible() {
        assert!(!exhausted_budget(Duration::from_millis(9), 4));
        assert!(exhausted_budget(Duration::from_millis(19), 4));
        assert!(exhausted_budget(Duration::from_millis(9), 1));
        assert!(!exhausted_budget(Duration::from_millis(2), 1));
    }
}
