use super::*;
fn observation(slot: u64, units: u8, arrival: f64, early: i32, late: i32) -> Observation {
    Observation {
        slot,
        segment: 0,
        arrival_symbols: arrival,
        confirmed_units: units,
        profile: ChannelProfile {
            earliest_symbol: early,
            latest_symbol: late,
            main_path_power: 1.0,
            residual_ratio: 0.0,
        },
    }
}
fn pair(
    tracker: &mut Tracker,
    interval: u64,
    first: Observation,
    following: Observation,
    manual: bool,
) -> Option<Decision> {
    assert!(
        tracker
            .observe(
                0,
                Observation {
                    slot: interval * 3 + 1,
                    ..first
                },
                manual
            )
            .is_none()
    );
    tracker.observe(
        1,
        Observation {
            slot: interval * 3 + 2,
            ..following
        },
        manual,
    )
}
#[test]
fn delayed_echo_corrects_preceding_phone_without_creating_an_earlier_boundary_overlap() {
    let mut tracker = Tracker::default();
    let first = observation(1, 4, 0.0, 0, 5);
    let following = observation(2, 7, 0.0, 0, 0);
    for interval in 0..2 {
        assert!(pair(&mut tracker, interval, first, following, false).is_none());
    }
    let Some(Decision::Targets {
        units,
        arrival_symbols,
    }) = pair(&mut tracker, 2, first, following, false)
    else {
        panic!("missing correction")
    };
    assert_eq!(units, [9, 7]);
    assert_eq!(arrival_symbols, [-2.5, 0.0]);
    for interval in 3..8 {
        let decision = pair(
            &mut tracker,
            interval,
            observation(1, 9, -2.5, 0, 5),
            following,
            false,
        );
        if interval < 5 {
            assert!(decision.is_none());
        } else {
            assert!(matches!(
                decision,
                Some(Decision::Targets { units: [9, 7], .. })
            ));
        }
    }
    assert_eq!(tracker.overlap_symbols, Some(0.0));
    assert!(tracker.armed);
}
#[test]
fn uncorrectable_spread_attempts_limits_before_releasing_and_manual_mode_never_releases() {
    let mut tracker = Tracker::default();
    let first = observation(1, 4, 0.0, 0, 5);
    let following = observation(2, 7, 0.0, 0, 5);
    for interval in 0..2 {
        pair(&mut tracker, interval, first, following, false);
    }
    let Some(Decision::Targets {
        units,
        arrival_symbols,
    }) = pair(&mut tracker, 2, first, following, false)
    else {
        panic!()
    };
    assert_eq!(units, [9, 12]);
    for interval in 3..6 {
        assert!(
            pair(
                &mut tracker,
                interval,
                observation(1, 9, arrival_symbols[0], 0, 5),
                observation(2, 12, arrival_symbols[1], 0, 5),
                true
            )
            .is_none()
        );
    }
    for interval in 6..8 {
        assert!(
            pair(
                &mut tracker,
                interval,
                observation(1, 9, arrival_symbols[0], 0, 5),
                observation(2, 12, arrival_symbols[1], 0, 5),
                false
            )
            .is_none()
        );
    }
    assert_eq!(
        pair(
            &mut tracker,
            8,
            observation(1, 9, arrival_symbols[0], 0, 5),
            observation(2, 12, arrival_symbols[1], 0, 5),
            false
        ),
        Some(Decision::Uncorrectable)
    );
}
#[test]
fn single_paths_stale_slots_segments_and_bearer_order_cannot_fabricate_overlap() {
    let mut tracker = Tracker::default();
    for interval in 0..6 {
        assert!(
            pair(
                &mut tracker,
                interval,
                observation(1, 4, 0.0, 0, 0),
                observation(2, 7, 0.0, 0, 0),
                false
            )
            .is_none()
        );
    }
    assert_eq!(tracker.detections, 0);
    assert!(
        tracker
            .observe(0, observation(200, 4, 0.0, 0, 10), false)
            .is_none()
    );
    assert!(
        tracker
            .observe(
                1,
                Observation {
                    segment: 1,
                    ..observation(201, 7, 0.0, 0, 0)
                },
                false
            )
            .is_none()
    );
    assert_eq!(tracker.detections, 0);
    let mut tracker = Tracker::default();
    for interval in 0..3 {
        assert!(
            tracker
                .observe(1, observation(interval * 3 + 1, 4, 0.0, 0, 5), false)
                .is_none()
        );
        let decision = tracker.observe(0, observation(interval * 3 + 2, 7, 0.0, 0, 0), false);
        if interval == 2 {
            assert!(matches!(
                decision,
                Some(Decision::Targets { units: [7, 9], .. })
            ));
        }
    }
}

#[test]
fn spread_disappearance_restores_nominal_arrival_without_losing_propagation() {
    let mut tracker = Tracker::default();
    for interval in 0..3 {
        pair(
            &mut tracker,
            interval,
            observation(1, 4, 0.0, 0, 5),
            observation(2, 7, 0.0, 0, 0),
            false,
        );
    }
    for interval in 3..6 {
        pair(
            &mut tracker,
            interval,
            observation(1, 9, -2.5, 0, 5),
            observation(2, 7, 0.0, 0, 0),
            false,
        );
    }
    let mut final_decision = None;
    for interval in 6..9 {
        final_decision = pair(
            &mut tracker,
            interval,
            observation(1, 9, -2.5, 0, 0),
            observation(2, 7, 0.0, 0, 0),
            false,
        );
    }
    assert_eq!(
        final_decision,
        Some(Decision::Targets {
            units: [4, 7],
            arrival_symbols: [0.0, 0.0]
        })
    );
}
