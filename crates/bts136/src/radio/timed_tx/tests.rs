use super::*;
use std::sync::Mutex;
#[derive(Default)]
struct Device {
    tick: u64,
    health: TxRadioHealth,
    enabled: bool,
    stops: usize,
    backend_stops: usize,
    writes: Vec<(u64, usize)>,
    fail_write: bool,
    write_error: Option<(sdr_hal::error::ErrorKind, Option<usize>)>,
    fail_stop: usize,
    fail_clock: bool,
    fail_enable: bool,
    rate_hz: Option<u64>,
    burst_ends: usize,
    clock_reads: usize,
    continuous_next_tick: Option<u64>,
    effective_starts: Vec<u64>,
    fail_burst_end: bool,
}
struct Fake(Arc<Mutex<Device>>);
impl RadioTx for Fake {
    fn capabilities(&self) -> sdr_hal::TxCapabilities {
        sdr_hal::TxCapabilities::from([
            sdr_hal::TxCapability::TimedSamples,
            sdr_hal::TxCapability::BurstEnd,
        ])
    }
    fn end_burst(&mut self) -> Result<(), HalError> {
        let mut state = self.0.lock().unwrap();
        if state.fail_burst_end {
            return Err("burst marker failed".into());
        }
        state.burst_ends += 1;
        state.continuous_next_tick = None;
        Ok(())
    }
    fn tx_sample_rate_hz(&self) -> f64 {
        194_400.0
    }
    fn supports_timed_tx(&self) -> bool {
        true
    }
    fn tick_rate(&self) -> u64 {
        self.0.lock().unwrap().rate_hz.unwrap_or(1000000)
    }
    fn get_hardware_time(&self) -> Result<u64, HalError> {
        let mut state = self.0.lock().unwrap();
        state.clock_reads += 1;
        if state.fail_clock {
            return Err("clock failed".into());
        }
        Ok(state.tick)
    }
    fn transmit(&mut self, _samples: &[Complex32]) -> Result<(), HalError> {
        Err("untimed write forbidden".into())
    }
    fn transmit_at(&mut self, samples: &[Complex32], tick: Option<u64>) -> Result<(), HalError> {
        let mut state = self.0.lock().unwrap();
        state.writes.push((tick.unwrap(), samples.len()));
        if state.fail_write {
            return Err("partial write".into());
        }
        if let Some((kind, accepted)) = state.write_error {
            state.tick = tick.unwrap() + 1;
            let error = HalError::new(kind, "simulated backend rejection");
            return Err(match accepted {
                Some(count) => error.with_accepted_samples(count),
                None => error,
            });
        }
        let actual = state.continuous_next_tick.unwrap_or(tick.unwrap());
        state.effective_starts.push(actual);
        state.continuous_next_tick =
            Some(actual + (samples.len() as u64 * 1_000_000).div_ceil(194400));
        Ok(())
    }
    fn enable_transmit(&mut self, enable: bool) -> Result<(), HalError> {
        let mut state = self.0.lock().unwrap();
        if enable {
            state.enabled = true;
            if state.fail_enable {
                return Err("enable failed after activation".into());
            }
        } else {
            state.stops += 1;
            if state.fail_stop > 0 {
                state.fail_stop -= 1;
                return Err("stop failed".into());
            }
            state.enabled = false;
        }
        Ok(())
    }
    fn stop(&mut self) -> Result<(), HalError> {
        self.0.lock().unwrap().backend_stops += 1;
        self.enable_transmit(false)
    }
    fn tx_health(&mut self) -> Result<TxRadioHealth, HalError> {
        Ok(self.0.lock().unwrap().health)
    }
}
fn stream() -> (TimedTx, Arc<Mutex<Device>>) {
    let state = Arc::new(Mutex::new(Device::default()));
    let tx = TimedTx::try_new(
        Box::new(Fake(state.clone())),
        194400,
        Limits {
            max_queued_blocks: 4,
            max_block_samples: 1296,
        },
    )
    .unwrap();
    (tx, state)
}
fn samples() -> Vec<Complex32> {
    vec![Complex32::new(0.1, -0.1); 1296]
}
#[test]
fn successful_write_stays_pending_until_clock_elapsed_and_absolute_timestamps_do_not_drift() {
    let (mut tx, state) = stream();
    tx.start(1000, 100000).unwrap();
    let first = tx.submit(&samples()).unwrap();
    let second = tx.submit(&samples()).unwrap();
    let third = tx.submit(&samples()).unwrap();
    assert_eq!(third.end_tick(), 21000);
    assert_eq!((first.start_tick(), first.end_tick()), (1000, 7667));
    assert_eq!((second.start_tick(), second.end_tick()), (7667, 14334));
    assert_eq!(tx.pending_blocks(), 3);
    let mut elapsed = Vec::with_capacity(4);
    state.lock().unwrap().tick = 7666;
    tx.poll(&mut elapsed).unwrap();
    assert!(elapsed.is_empty());
    state.lock().unwrap().tick = 7667;
    tx.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed, [first]);
    assert_eq!(tx.pending_blocks(), 2);
    state.lock().unwrap().tick = 14334;
    tx.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed.len(), 2);
    assert_eq!(elapsed[1], second);
    assert_eq!(
        state.lock().unwrap().writes,
        [(1000, 1296), (7667, 1296), (14334, 1296)]
    );
    state.lock().unwrap().tick = 21000;
    tx.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed[2], third);
    tx.stop().unwrap();
    assert!(!state.lock().unwrap().enabled);
}
#[test]
fn partial_write_stops_tx_clears_uncertain_receipts_and_is_never_resent() {
    let (mut tx, state) = stream();
    tx.start(1000, 100000).unwrap();
    tx.submit(&samples()).unwrap();
    state.lock().unwrap().fail_write = true;
    let error = tx.submit(&samples()).unwrap_err();
    assert!(matches!(error.failure, Failure::Hal(_)));
    assert_eq!(tx.state(), State::Failed);
    assert_eq!(tx.pending_blocks(), 0);
    assert!(tx.stop_acknowledged());
    assert!(!state.lock().unwrap().enabled);
    assert_eq!(state.lock().unwrap().writes.len(), 2);
    assert!(matches!(
        tx.submit(&samples()).unwrap_err().failure,
        Failure::State
    ));
    assert_eq!(state.lock().unwrap().writes.len(), 2);
}
#[test]
fn late_submission_is_rejected_without_stopping_but_clock_and_transport_faults_stop() {
    for fault in 0..4 {
        let (mut tx, state) = stream();
        tx.start(1000, 100000).unwrap();
        tx.submit(&samples()).unwrap();
        {
            let mut device = state.lock().unwrap();
            match fault {
                0 => device.tick = 8000,
                1 => {
                    device.tick = 0;
                }
                2 => device.health.sequence_errors = 1,
                _ => device.fail_clock = true,
            }
        }
        if fault == 0 {
            assert!(matches!(
                tx.submit(&samples()).unwrap_err().failure,
                Failure::Late { .. }
            ));
            assert_eq!(tx.state(), State::Running);
            assert_eq!(tx.pending_blocks(), 1);
            assert!(state.lock().unwrap().enabled);
            continue;
        } else if fault == 1 {
            state.lock().unwrap().tick = 1;
            let mut elapsed = Vec::with_capacity(4);
            tx.poll(&mut elapsed).unwrap();
            state.lock().unwrap().tick = 0;
            assert!(matches!(
                tx.poll(&mut elapsed).unwrap_err().failure,
                Failure::ClockRegression { .. }
            ));
        } else {
            let mut elapsed = Vec::with_capacity(4);
            assert!(tx.poll(&mut elapsed).is_err());
            assert!(elapsed.is_empty());
        }
        assert_eq!(tx.state(), State::Failed);
        assert_eq!(tx.pending_blocks(), 0);
        assert!(!state.lock().unwrap().enabled);
    }
}
#[test]
fn polling_at_the_stop_deadline_requests_stop_and_never_schedules_samples_beyond_the_bound() {
    let (mut tx, state) = stream();
    tx.start(1000, 7667).unwrap();
    let receipt = tx.submit(&samples()).unwrap();
    assert!(matches!(
        tx.submit(&samples()).unwrap_err().failure,
        Failure::Deadline { .. }
    ));
    state.lock().unwrap().tick = 7667;
    let mut elapsed = Vec::with_capacity(4);
    let observed = tx.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed, [receipt]);
    assert_eq!(observed.state, State::Stopped);
    assert!(tx.stop_acknowledged());
    assert!(!state.lock().unwrap().enabled);
}
#[test]
fn queue_and_output_capacity_errors_apply_backpressure_without_allocating_or_advancing() {
    let (mut tx, state) = stream();
    tx.start(1000, 100000).unwrap();
    for _ in 0..4 {
        tx.submit(&samples()).unwrap();
    }
    assert!(matches!(
        tx.submit(&samples()).unwrap_err().failure,
        Failure::QueueFull
    ));
    assert_eq!(state.lock().unwrap().writes.len(), 4);
    state.lock().unwrap().tick = 30000;
    let mut small = Vec::new();
    assert!(matches!(
        tx.poll(&mut small).unwrap_err().failure,
        Failure::OutputCapacity
    ));
    assert_eq!(tx.pending_blocks(), 4);
    let mut elapsed = Vec::with_capacity(4);
    tx.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed.len(), 4);
}
#[test]
fn failed_stop_is_reported_and_drop_retries_it_instead_of_claiming_shutdown() {
    let (mut tx, state) = stream();
    tx.start(1000, 100000).unwrap();
    state.lock().unwrap().fail_stop = 2;
    let error = tx.stop().unwrap_err();
    assert!(error.stop_error.is_some());
    assert!(!tx.stop_acknowledged());
    assert_eq!(tx.state(), State::Failed);
    assert!(state.lock().unwrap().enabled);
    drop(tx);
    assert!(!state.lock().unwrap().enabled);
    assert_eq!(state.lock().unwrap().stops, 4);
}
#[test]
fn failed_enable_is_stopped_and_invalid_samples_do_not_reach_the_backend() {
    let (mut tx, state) = stream();
    state.lock().unwrap().fail_enable = true;
    assert!(tx.start(1000, 100000).is_err());
    assert!(!state.lock().unwrap().enabled);
    let (mut tx, state) = stream();
    tx.start(1000, 100000).unwrap();
    assert!(matches!(
        tx.submit(&[]).unwrap_err().failure,
        Failure::SampleCount(0)
    ));
    assert!(matches!(
        tx.submit(&[Complex32::new(f32::NAN, 0.0)])
            .unwrap_err()
            .failure,
        Failure::Sample(0)
    ));
    assert!(state.lock().unwrap().writes.is_empty());
}

#[test]
fn every_reported_fault_or_counter_reset_invalidates_queue_even_after_the_scheduled_end() {
    for fault in 0..4 {
        let (mut tx, state) = stream();
        state.lock().unwrap().health.burst_acks = 1;
        tx.start(1000, 100000).unwrap();
        tx.submit(&samples()).unwrap();
        {
            let mut state = state.lock().unwrap();
            state.tick = 9000;
            match fault {
                0 => state.health.sequence_errors = 1,
                1 => state.health.dropped_packets = 1,
                2 => state.health.unknown_events = 1,
                _ => state.health.burst_acks = 0,
            }
        }
        let mut elapsed = Vec::with_capacity(4);
        assert!(matches!(
            tx.poll(&mut elapsed).unwrap_err().failure,
            Failure::Health { .. }
        ));
        assert!(elapsed.is_empty());
        assert!(!state.lock().unwrap().enabled);
    }
    let (mut tx, state) = stream();
    tx.start(1000, 100000).unwrap();
    tx.submit(&samples()).unwrap();
    {
        let mut state = state.lock().unwrap();
        state.tick = 9000;
        state.health.burst_acks = 1;
    }
    let mut elapsed = Vec::with_capacity(4);
    tx.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed.len(), 1);
    assert_eq!(tx.state(), State::Running);
}
#[test]
fn unsupported_clock_resolution_and_changed_clock_rate_cannot_activate_or_continue_tx() {
    let state = Arc::new(Mutex::new(Device {
        rate_hz: Some(1000),
        ..Device::default()
    }));
    assert!(matches!(
        TimedTx::try_new(
            Box::new(Fake(state.clone())),
            194400,
            Limits {
                max_queued_blocks: 4,
                max_block_samples: 1296
            }
        ),
        Err(Error {
            failure: Failure::ClockRate { .. },
            ..
        })
    ));
    assert!(!state.lock().unwrap().enabled);
    let (mut tx, state) = stream();
    tx.start(1000, 100000).unwrap();
    tx.submit(&samples()).unwrap();
    state.lock().unwrap().rate_hz = Some(2000000);
    let mut elapsed = Vec::with_capacity(4);
    assert!(matches!(
        tx.poll(&mut elapsed).unwrap_err().failure,
        Failure::ClockRateChanged { .. }
    ));
    assert!(elapsed.is_empty());
    assert!(!state.lock().unwrap().enabled);
}

#[test]
fn construction_and_shutdown_dispatch_the_backend_stop_override() {
    let (mut tx, state) = stream();
    assert_eq!(state.lock().unwrap().backend_stops, 1);
    tx.start(1000, 100000).unwrap();
    tx.stop().unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.backend_stops, 2);
    assert!(!state.enabled);
    assert!(tx.stop_acknowledged());
}

#[test]
fn underflows_preserve_pending_receipts_and_allow_subsequent_transmission() {
    let (mut tx, state) = stream();
    tx.start(1000, 100000).unwrap();
    let first = tx.submit(&samples()).unwrap();
    tx.submit(&samples()).unwrap();
    state.lock().unwrap().health.underflows = 1;
    let mut elapsed = Vec::with_capacity(4);
    tx.poll(&mut elapsed).unwrap();
    assert!(elapsed.is_empty());
    assert_eq!(tx.pending_blocks(), 2);
    state.lock().unwrap().tick = first.end_tick();
    tx.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed, [first]);
    state.lock().unwrap().health.underflows = 2;
    state.lock().unwrap().health.late_packets = 1;
    tx.submit(&samples()).unwrap();
    assert_eq!(tx.state(), State::Running);
    assert_eq!(tx.pending_blocks(), 2);
    assert!(state.lock().unwrap().enabled);
    tx.stop().unwrap();
    assert!(tx.stop_acknowledged());
}

#[test]
fn skipping_expired_blocks_preserves_the_absolute_epoch_and_never_writes_old_iq() {
    let (mut tx, state) = stream();
    tx.start(1000, 100000).unwrap();
    state.lock().unwrap().tick = 8000;
    assert!(tx.skip_expired_block(samples().len(), 0).unwrap());
    assert!(tx.skip_expired_block(samples().len(), 0).unwrap());
    assert!(!tx.skip_expired_block(samples().len(), 0).unwrap());
    assert!(state.lock().unwrap().writes.is_empty());
    assert_eq!(tx.pending_blocks(), 0);
    let receipt = tx.submit(&samples()).unwrap();
    assert_eq!(receipt.start_tick(), 14334);
    assert_eq!(tx.state(), State::Running);
    assert!(state.lock().unwrap().enabled);
}

#[test]
fn backend_lateness_recovers_only_when_no_samples_were_accepted() {
    for accepted in [Some(0), Some(1), None] {
        let (mut tx, device) = stream();
        tx.start(1000, 100000).unwrap();
        device.lock().unwrap().write_error = Some((sdr_hal::error::ErrorKind::Late, accepted));
        let error = tx.submit(&samples()).unwrap_err();
        assert_eq!(tx.pending_blocks(), 0);
        if accepted == Some(0) {
            assert!(matches!(
                error.failure,
                Failure::Late {
                    start_tick: 1000,
                    current_tick: 1001
                }
            ));
            assert_eq!(tx.state(), State::Running);
            assert!(device.lock().unwrap().enabled);
            assert!(tx.skip_expired_block(samples().len(), 0).unwrap());
            device.lock().unwrap().write_error = None;
            let receipt = tx.submit(&samples()).unwrap();
            assert!(receipt.start_tick() > 1001);
            assert_eq!(receipt.sequence(), 0);
        } else {
            assert!(matches!(error.failure, Failure::Hal(_)));
            assert_eq!(tx.state(), State::Failed);
            assert!(!device.lock().unwrap().enabled);
        }
    }
}

#[test]
fn skipped_slot_ends_continuous_burst_before_a_future_timestamp_is_reused() {
    let (mut tx, device) = stream();
    tx.start(10000, 100000).unwrap();
    tx.submit(&samples()).unwrap();
    device.lock().unwrap().tick = 12000;
    assert!(tx.skip_expired_block(samples().len(), 5000).unwrap());
    assert_eq!(device.lock().unwrap().burst_ends, 1);
    assert!(!tx.skip_expired_block(samples().len(), 5000).unwrap());
    let receipt = tx.submit(&samples()).unwrap();
    let state = device.lock().unwrap();
    assert_eq!(state.effective_starts, vec![10000, receipt.start_tick()]);
    assert_eq!(receipt.start_tick(), 23334);
    assert_eq!(state.burst_ends, 1);
    assert!(state.enabled);
}
#[test]
fn failed_gap_burst_boundary_stops_uncertain_transmission() {
    let (mut tx, device) = stream();
    tx.start(10000, 100000).unwrap();
    tx.submit(&samples()).unwrap();
    {
        let mut state = device.lock().unwrap();
        state.tick = 12000;
        state.fail_burst_end = true;
    }
    assert!(matches!(
        tx.skip_expired_block(samples().len(), 5000)
            .unwrap_err()
            .failure,
        Failure::Hal(_)
    ));
    assert_eq!(tx.state(), State::Failed);
    assert_eq!(tx.pending_blocks(), 0);
    assert!(!device.lock().unwrap().enabled);
}

#[test]
fn bulk_recovery_skips_expired_blocks_with_one_clock_query_and_one_burst_boundary() {
    let (mut tx, device) = stream();
    tx.start(10000, 10000000).unwrap();
    tx.submit(&samples()).unwrap();
    let reads = {
        let mut state = device.lock().unwrap();
        state.tick = 1000000;
        state.clock_reads
    };
    assert_eq!(
        tx.skip_expired_blocks(samples().len(), 5000, 512).unwrap(),
        149
    );
    {
        let state = device.lock().unwrap();
        assert_eq!(state.clock_reads, reads + 1);
        assert_eq!(state.burst_ends, 1);
        assert_eq!(state.writes.len(), 1);
    }
    let receipt = tx.submit(&samples()).unwrap();
    assert_eq!(receipt.start_tick(), 1010000);
    assert_eq!(
        device.lock().unwrap().effective_starts,
        vec![10000, 1010000]
    );
}
#[test]
fn cached_hardware_time_reads_the_device_once_per_refresh_and_never_runs_backwards() {
    let (mut tx, state) = stream();
    tx.set_hardware_time_refresh(Duration::from_secs(3600));
    tx.start(1000, 100000).unwrap();
    let reads = state.lock().unwrap().clock_reads;
    let mut elapsed = Vec::with_capacity(4);
    let mut previous = 0;
    for _ in 0..100 {
        let observed = tx.poll(&mut elapsed).unwrap();
        assert!(observed.hardware_tick >= previous);
        previous = observed.hardware_tick;
    }
    tx.submit(&samples()).unwrap();
    assert_eq!(state.lock().unwrap().clock_reads, reads);
    tx.set_hardware_time_refresh(Duration::ZERO);
    tx.poll(&mut elapsed).unwrap();
    assert_eq!(state.lock().unwrap().clock_reads, reads + 1);
}
