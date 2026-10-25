use super::*;
use sdr_hal::RxReadResult;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
struct Read {
    tick: u64,
    count: usize,
    overflow: bool,
    nan: bool,
}
#[derive(Default)]
struct Device {
    reads: VecDeque<Read>,
    active: bool,
    stops: usize,
    fail_stop: usize,
    fail_read: bool,
    fail_activate: bool,
    rate_hz: Option<u64>,
    activation: Option<u64>,
}
struct Fake(Arc<Mutex<Device>>);
impl RadioRx for Fake {
    fn rx_sample_rate_hz(&self) -> f64 {
        194_400.0
    }
    fn tick_rate(&self) -> u64 {
        self.0.lock().unwrap().rate_hz.unwrap_or(1000000)
    }
    fn get_hardware_time(&self) -> Result<u64, HalError> {
        Ok(0)
    }
    fn rx_read(
        &mut self,
        buffer: &mut [Complex32],
        _timeout_us: i64,
    ) -> Result<RxReadResult, HalError> {
        let mut state = self.0.lock().unwrap();
        if state.fail_read {
            return Err("read failed".into());
        }
        let read = state.reads.pop_front().unwrap();
        buffer.fill(Complex32::new(f32::NAN, f32::NAN));
        for (index, sample) in buffer.iter_mut().take(read.count).enumerate() {
            *sample = Complex32::new(if read.nan { f32::NAN } else { index as f32 }, -0.1);
        }
        Ok(RxReadResult {
            samples_read: read.count,
            time_ticks: read.tick,
            overflow: read.overflow,
        })
    }
    fn rx_activate(&mut self, tick: Option<u64>) -> Result<(), HalError> {
        let mut state = self.0.lock().unwrap();
        state.active = true;
        state.activation = tick;
        if state.fail_activate {
            return Err("activation failed after starting".into());
        }
        Ok(())
    }
    fn stop(&mut self) -> Result<(), HalError> {
        let mut state = self.0.lock().unwrap();
        state.stops += 1;
        if state.fail_stop > 0 {
            state.fail_stop -= 1;
            return Err("stop failed".into());
        }
        state.active = false;
        Ok(())
    }
}
fn receiver(reads: Vec<Read>) -> (Receiver, Arc<Mutex<Device>>) {
    let state = Arc::new(Mutex::new(Device {
        reads: reads.into(),
        ..Device::default()
    }));
    let receiver = Receiver::try_new(
        Box::new(Fake(state.clone())),
        194400,
        Limits {
            max_block_samples: 32,
            timeout_us: 1000,
        },
    )
    .unwrap();
    (receiver, state)
}
fn read(tick: u64, count: usize) -> Read {
    Read {
        tick,
        count,
        overflow: false,
        nan: false,
    }
}
fn block(receiver: &mut Receiver) -> Block {
    let mut buffer = [Complex32::new(0.0, 0.0); 32];
    let ReadOutcome::Block(block) = receiver.read(&mut buffer).unwrap() else {
        panic!("expected data")
    };
    block
}
#[test]
fn actual_first_timestamp_and_variable_read_counts_define_epoch_without_false_fractional_clock_gaps()
 {
    let (mut rx, state) = receiver(vec![read(500, 1), read(505, 2), read(515, 3), read(531, 4)]);
    rx.start(Some(1000)).unwrap();
    let first = block(&mut rx);
    assert_eq!(first.continuity, Continuity::First);
    assert_eq!(rx.origin_tick(), Some(500));
    assert_eq!(state.lock().unwrap().activation, Some(1000));
    for expected in [1, 3, 6] {
        let current = block(&mut rx);
        assert_eq!(current.first_sample, expected);
        assert_eq!(current.continuity, Continuity::Continuous);
        assert_eq!(current.segment, 0);
    }
}
#[test]
fn forward_timestamp_gaps_are_counted_and_overflow_without_a_timestamp_gap_remains_unknown() {
    let mut third = read(531, 2);
    third.overflow = true;
    let (mut rx, _) = receiver(vec![read(500, 2), read(521, 2), third]);
    rx.start(None).unwrap();
    block(&mut rx);
    let gap = block(&mut rx);
    assert_eq!(gap.first_sample, 4);
    assert_eq!(gap.continuity, Continuity::Gap { missing_samples: 2 });
    assert_eq!(gap.segment, 1);
    let unknown = block(&mut rx);
    assert_eq!(unknown.first_sample, 6);
    assert_eq!(unknown.continuity, Continuity::UnknownGap);
    assert_eq!(unknown.segment, 2);
}
#[test]
fn empty_read_timestamps_are_ignored_but_overflow_is_retained_for_next_data() {
    let mut timeout = read(u64::MAX, 0);
    timeout.overflow = true;
    let (mut rx, _) = receiver(vec![read(500, 2), timeout, read(510, 2)]);
    rx.start(None).unwrap();
    block(&mut rx);
    let mut buffer = [Complex32::new(0.0, 0.0); 32];
    assert_eq!(
        rx.read(&mut buffer).unwrap(),
        ReadOutcome::Timeout {
            hardware_overflow: true
        }
    );
    let current = block(&mut rx);
    assert_eq!(current.continuity, Continuity::UnknownGap);
    assert!(current.hardware_overflow);
    assert_eq!(current.segment, 1);
}
#[test]
fn regressing_timestamps_invalid_counts_or_nonfinite_data_stop_the_stream() {
    for mode in [0, 2, 3] {
        let bad = match mode {
            0 => read(499, 2),
            2 => read(510, 33),
            _ => Read {
                nan: true,
                ..read(510, 2)
            },
        };
        let (mut rx, state) = receiver(vec![read(500, 2), bad]);
        rx.start(None).unwrap();
        block(&mut rx);
        let mut buffer = [Complex32::new(0.0, 0.0); 32];
        assert!(rx.read(&mut buffer).is_err());
        assert_eq!(rx.state(), State::Failed);
        assert!(!state.lock().unwrap().active);
    }
}
#[test]
fn failed_reads_deactivate_and_failed_deactivation_is_reported_and_retried_on_drop() {
    let (mut rx, state) = receiver(vec![]);
    rx.start(None).unwrap();
    state.lock().unwrap().fail_read = true;
    let mut buffer = [Complex32::new(0.0, 0.0); 32];
    assert!(matches!(
        rx.read(&mut buffer).unwrap_err().failure,
        Failure::Hal(_)
    ));
    assert!(!state.lock().unwrap().active);
    let (mut rx, state) = receiver(vec![]);
    rx.start(None).unwrap();
    state.lock().unwrap().fail_stop = 2;
    let error = rx.stop().unwrap_err();
    assert!(error.stop_error.is_some());
    assert!(!rx.stop_acknowledged());
    drop(rx);
    assert!(!state.lock().unwrap().active);
    assert_eq!(state.lock().unwrap().stops, 4);
}

#[test]
fn failed_activation_and_clock_rate_changes_deactivate_without_accepting_data() {
    let (mut rx, state) = receiver(vec![]);
    state.lock().unwrap().fail_activate = true;
    assert!(rx.start(None).is_err());
    assert_eq!(rx.state(), State::Failed);
    assert!(!state.lock().unwrap().active);
    let (mut rx, state) = receiver(vec![read(500, 2)]);
    rx.start(None).unwrap();
    state.lock().unwrap().rate_hz = Some(2000000);
    let mut buffer = [Complex32::new(0.0, 0.0); 32];
    assert!(matches!(
        rx.read(&mut buffer).unwrap_err().failure,
        Failure::ClockRateChanged { .. }
    ));
    assert!(!state.lock().unwrap().active);
    assert_eq!(state.lock().unwrap().reads.len(), 1);
}
#[test]
fn preepoch_timeout_overflow_is_unknown_and_invalid_buffers_do_not_consume_reads() {
    let mut timeout = read(u64::MAX, 0);
    timeout.overflow = true;
    let (mut rx, state) = receiver(vec![timeout, read(500, 2)]);
    rx.start(None).unwrap();
    assert!(matches!(
        rx.read(&mut []).unwrap_err().failure,
        Failure::Buffer(0)
    ));
    assert_eq!(state.lock().unwrap().reads.len(), 2);
    let mut buffer = [Complex32::new(0.0, 0.0); 32];
    assert!(matches!(
        rx.read(&mut buffer).unwrap(),
        ReadOutcome::Timeout {
            hardware_overflow: true
        }
    ));
    assert_eq!(rx.origin_tick(), None);
    let first = block(&mut rx);
    assert_eq!(first.continuity, Continuity::UnknownGap);
    assert_eq!(first.segment, 0);
    assert_eq!(first.first_sample, 0);
    assert_eq!(rx.origin_tick(), Some(500));
    let state = Arc::new(Mutex::new(Device {
        rate_hz: Some(300000),
        ..Device::default()
    }));
    assert!(matches!(
        Receiver::try_new(
            Box::new(Fake(state.clone())),
            194400,
            Limits {
                max_block_samples: 32,
                timeout_us: 1000
            }
        ),
        Err(Error {
            failure: Failure::ClockRate { .. },
            ..
        })
    ));
    assert!(!state.lock().unwrap().active);
}

#[test]
fn long_fractional_uhd_timestamp_stream_keeps_counts_and_detects_real_one_sample_gaps() {
    const NOMINAL_RATE: u64 = 388800;
    const ACTUAL_RATE: f64 = 388800.0039493336;
    const CLOCK: u64 = 24883200;
    const COUNT: usize = 1024;
    const BLOCKS: u64 = 50000;
    let timestamp = |sample: u64| (sample as f64 * CLOCK as f64 / ACTUAL_RATE).round() as u64;
    let mut reads: Vec<_> = (0..BLOCKS)
        .map(|index| read(timestamp(index * COUNT as u64), COUNT))
        .collect();
    reads.push(read(timestamp(BLOCKS * COUNT as u64 + 1), COUNT));
    let state = Arc::new(Mutex::new(Device {
        reads: reads.into(),
        rate_hz: Some(CLOCK),
        ..Device::default()
    }));
    let mut receiver = Receiver::try_new(
        Box::new(Fake(state)),
        NOMINAL_RATE,
        Limits {
            max_block_samples: COUNT,
            timeout_us: 1000,
        },
    )
    .unwrap();
    receiver.start(None).unwrap();
    let mut buffer = vec![Complex32::new(0.0, 0.0); COUNT];
    for index in 0..BLOCKS {
        let ReadOutcome::Block(block) = receiver.read(&mut buffer).unwrap() else {
            panic!()
        };
        assert_eq!(block.first_sample, index * COUNT as u64);
        assert_eq!(block.segment, 0);
        assert_eq!(
            block.continuity,
            if index == 0 {
                Continuity::First
            } else {
                Continuity::Continuous
            }
        );
    }
    let origin_projection = ((u128::from(timestamp((BLOCKS - 1) * COUNT as u64))
        * u128::from(NOMINAL_RATE)
        + u128::from(CLOCK) / 2)
        / u128::from(CLOCK)) as u64;
    assert!(origin_projection < (BLOCKS - 1) * COUNT as u64);
    let ReadOutcome::Block(block) = receiver.read(&mut buffer).unwrap() else {
        panic!()
    };
    assert_eq!(block.first_sample, BLOCKS * COUNT as u64 + 1);
    assert_eq!(block.continuity, Continuity::Gap { missing_samples: 1 });
    assert_eq!(block.segment, 1);
}

#[test]
fn partial_overlap_discards_the_prefix_and_continues_in_a_fresh_segment() {
    let (mut receiver, state) = receiver(vec![read(500, 2), read(505, 3), read(520, 2)]);
    receiver.start(None).unwrap();
    assert_eq!(block(&mut receiver).first_sample, 0);
    let mut buffer = [Complex32::new(0.0, 0.0); 32];
    let ReadOutcome::Block(overlap) = receiver.read(&mut buffer).unwrap() else {
        panic!()
    };
    assert_eq!(buffer[0].re, 1.0);
    assert_eq!(buffer[1].re, 2.0);
    assert_eq!(overlap.first_sample, 2);
    assert_eq!(overlap.samples_read, 2);
    assert_eq!(overlap.time_ticks, 510);
    assert_eq!(
        overlap.continuity,
        Continuity::Overlap {
            discarded_samples: 1
        }
    );
    assert_eq!(overlap.segment, 1);
    let next = block(&mut receiver);
    assert_eq!(next.first_sample, 4);
    assert_eq!(next.continuity, Continuity::Continuous);
    assert_eq!(next.segment, 1);
    assert!(state.lock().unwrap().active);
}
#[test]
fn entirely_repeated_block_is_dropped_and_next_fresh_data_resets_acquisition() {
    let (mut receiver, state) = receiver(vec![read(500, 2), read(500, 2), read(510, 2)]);
    receiver.start(None).unwrap();
    block(&mut receiver);
    let mut buffer = [Complex32::new(0.0, 0.0); 32];
    assert!(matches!(
        receiver.read(&mut buffer).unwrap(),
        ReadOutcome::Dropped {
            repeated_samples: 2,
            ..
        }
    ));
    let next = block(&mut receiver);
    assert_eq!(next.first_sample, 2);
    assert_eq!(next.continuity, Continuity::UnknownGap);
    assert_eq!(next.segment, 1);
    assert!(state.lock().unwrap().active);
}

#[test]
fn sample_counting_clock_is_accepted_and_keeps_reads_contiguous() {
    let state = Arc::new(Mutex::new(Device {
        reads: vec![read(1000, 32), read(1032, 32), read(1080, 32)].into(),
        rate_hz: Some(194400),
        ..Device::default()
    }));
    let mut rx = Receiver::try_new(
        Box::new(Fake(state)),
        194400,
        Limits {
            max_block_samples: 32,
            timeout_us: 1000,
        },
    )
    .unwrap();
    rx.start(None).unwrap();
    assert_eq!(block(&mut rx).continuity, Continuity::First);
    let second = block(&mut rx);
    assert_eq!(second.continuity, Continuity::Continuous);
    assert_eq!(second.first_sample, 32);
    let third = block(&mut rx);
    assert!(matches!(third.continuity, Continuity::Gap { .. }));
    assert_eq!(third.first_sample, 80);
}
