use super::*;
const MAXIMUM_AGE_MS: u64 = 640;
fn block(tick: u64, segment: u64, continuity: Continuity) -> Block {
    Block {
        samples_read: 10,
        time_ticks: tick,
        first_sample: 0,
        segment,
        continuity,
        hardware_overflow: false,
    }
}
#[test]
fn clock_rates_offsets_and_exact_age_boundary_use_integer_time() {
    let mut window = ReceiveWindow::try_new(1000, 2000, 0, MAXIMUM_AGE_MS).unwrap();
    assert_eq!(
        window
            .admit(&block(2000, 0, Continuity::First), 1639)
            .unwrap(),
        Admission::Accept {
            segment: 0,
            reset: false
        }
    );
    assert_eq!(
        window
            .admit(&block(2000, 0, Continuity::Continuous), 1640)
            .unwrap(),
        Admission::Discard {
            age_ms: MAXIMUM_AGE_MS,
            reset: true
        }
    );
    let mut positive = ReceiveWindow::try_new(1000, 2000, 1_000_000, MAXIMUM_AGE_MS).unwrap();
    assert!(matches!(
        positive
            .admit(&block(2000, 0, Continuity::First), 1640)
            .unwrap(),
        Admission::Accept { .. }
    ));
    let mut negative = ReceiveWindow::try_new(1000, 2000, -1_000_000, MAXIMUM_AGE_MS).unwrap();
    assert!(matches!(
        negative
            .admit(&block(2000, 0, Continuity::First), 1639)
            .unwrap(),
        Admission::Discard {
            age_ms: MAXIMUM_AGE_MS,
            ..
        }
    ));
}
#[test]
fn a_backlog_run_discards_once_then_changes_the_downstream_segment() {
    let mut window = ReceiveWindow::try_new(1000, 1000, 0, MAXIMUM_AGE_MS).unwrap();
    assert!(matches!(
        window
            .admit(&block(1000, 0, Continuity::First), 1000)
            .unwrap(),
        Admission::Accept {
            segment: 0,
            reset: false
        }
    ));
    assert!(matches!(
        window
            .admit(&block(1001, 0, Continuity::Continuous), 2000)
            .unwrap(),
        Admission::Discard { reset: true, .. }
    ));
    assert!(matches!(
        window
            .admit(&block(1002, 0, Continuity::Continuous), 2000)
            .unwrap(),
        Admission::Discard { reset: false, .. }
    ));
    assert_eq!(
        window
            .admit(&block(2000, 0, Continuity::Continuous), 2001)
            .unwrap(),
        Admission::Accept {
            segment: 1,
            reset: false
        }
    );
    assert_eq!(
        window
            .admit(&block(2001, 0, Continuity::Continuous), 2002)
            .unwrap(),
        Admission::Accept {
            segment: 1,
            reset: false
        }
    );
}
#[test]
fn hardware_gaps_and_local_discards_share_one_monotonic_decoder_generation() {
    let mut window = ReceiveWindow::try_new(1000, 1000, 0, MAXIMUM_AGE_MS).unwrap();
    window
        .admit(&block(1000, 20, Continuity::First), 1000)
        .unwrap();
    assert_eq!(
        window
            .admit(&block(1001, 21, Continuity::UnknownGap), 1001)
            .unwrap(),
        Admission::Accept {
            segment: 1,
            reset: true
        }
    );
    assert!(matches!(
        window
            .admit(
                &block(
                    1002,
                    22,
                    Continuity::Gap {
                        missing_samples: 10
                    }
                ),
                2000
            )
            .unwrap(),
        Admission::Discard { reset: true, .. }
    ));
    assert_eq!(
        window
            .admit(&block(2000, 23, Continuity::UnknownGap), 2000)
            .unwrap(),
        Admission::Accept {
            segment: 2,
            reset: false
        }
    );
}
#[test]
fn future_samples_and_large_timestamps_do_not_wrap_into_old_input() {
    let mut window = ReceiveWindow::try_new(24_883_200, 24_883_200, 0, MAXIMUM_AGE_MS).unwrap();
    assert!(matches!(
        window
            .admit(&block(u64::MAX, 0, Continuity::First), u64::MAX - 1)
            .unwrap(),
        Admission::Accept { .. }
    ));
    assert!(matches!(
        window
            .admit(
                &block(u64::MAX - 24_883_200, 0, Continuity::Continuous),
                u64::MAX
            )
            .unwrap(),
        Admission::Discard { age_ms: 1000, .. }
    ));
    assert_eq!(
        ReceiveWindow::try_new(0, 1, 0, MAXIMUM_AGE_MS).err(),
        Some(Error::Limits)
    );
    assert_eq!(
        ReceiveWindow::try_new(1, 0, 0, MAXIMUM_AGE_MS).err(),
        Some(Error::Limits)
    );
    assert_eq!(
        ReceiveWindow::try_new(1, 1, 0, 0).err(),
        Some(Error::Limits)
    );
    assert_eq!(
        ReceiveWindow::try_new(u64::MAX, u64::MAX, 0, MAXIMUM_AGE_MS).err(),
        Some(Error::ClockRange)
    );
}
#[test]
fn rejected_range_does_not_partially_change_the_current_generation() {
    let mut window = ReceiveWindow::try_new(1, 1, 0, 1).unwrap();
    window.segment = u64::MAX;
    assert_eq!(
        window.admit(&block(0, 0, Continuity::First), 1).err(),
        Some(Error::SegmentRange)
    );
    assert!(window.source_segment.is_none());
    assert!(!window.dropping);
    assert_eq!(window.segment, u64::MAX);
}

#[test]
fn nanosecond_offsets_preserve_the_submillisecond_expiry_boundary() {
    let mut window =
        ReceiveWindow::try_new(1_000_000_000, 1_000_000_000, 1, MAXIMUM_AGE_MS).unwrap();
    assert!(matches!(
        window
            .admit(&block(0, 0, Continuity::First), 640_000_000)
            .unwrap(),
        Admission::Accept { .. }
    ));
    let mut window =
        ReceiveWindow::try_new(1_000_000_000, 1_000_000_000, -1, MAXIMUM_AGE_MS).unwrap();
    assert!(matches!(
        window
            .admit(&block(0, 0, Continuity::First), 639_999_999)
            .unwrap(),
        Admission::Discard {
            age_ms: MAXIMUM_AGE_MS,
            ..
        }
    ));
}
