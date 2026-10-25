use super::*;
const SAMPLE_RATE_HZ: u64 = 194400;
#[test]
fn absolute_tick_mapping_bounds_rounding_and_does_not_accumulate_drift() {
    let clock = FrameClock::try_new(1000, 1000003, SAMPLE_RATE_HZ).unwrap();
    for index in 0..300 {
        let timing = clock.at(index).unwrap();
        let next = clock.at(index + 1).unwrap();
        assert_eq!(timing.end_tick, next.start_tick);
        let rounded = u128::from(timing.start_tick - 1000) * 150;
        let exact = u128::from(index) * 1000003;
        assert!(rounded >= exact && rounded - exact < 150);
        assert_eq!(timing.first_sample, index * 1296);
        assert_eq!(timing.sample_count, 1296);
        assert_eq!(timing.frame_number, index / 6);
        assert_eq!(u64::from(timing.slot.number()), index % 6 + 1);
    }
    let later = clock.at(1500000).unwrap();
    assert_eq!(later.start_tick, 1000 + 1000003 * 10000);
    assert_eq!(later.first_sample, 194400 * 10000);
}
#[test]
fn full_rate_pairs_map_every_other_half_frame_with_twenty_millisecond_cadence() {
    let clock = FrameClock::try_new(0, 1000000, SAMPLE_RATE_HZ).unwrap();
    for (assignment, phase) in [
        (AssignedSlots::Full14, 0),
        (AssignedSlots::Full25, 1),
        (AssignedSlots::Full36, 2),
    ] {
        let pair = FullRatePair::try_new(assignment).unwrap();
        for interval in 0..100 {
            let timing = pair.timing(&clock, interval).unwrap();
            let next = pair.timing(&clock, interval + 1).unwrap();
            assert_eq!(timing.slot_index, interval * 3 + phase);
            assert_eq!(next.start_tick - timing.start_tick, 20000);
            assert_eq!(next.first_sample - timing.first_sample, 3888);
        }
    }
}
#[test]
fn all_eight_preferred_sync_usage_rows_match_the_standard_with_unused_slots() {
    let clock = FrameClock::try_new(0, 1000000, SAMPLE_RATE_HZ).unwrap();
    let assignments = [
        AssignedSlots::Full14,
        AssignedSlots::Full25,
        AssignedSlots::Full36,
    ];
    let rows = [
        [
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Three,
            SyncWord::Four,
            SyncWord::Five,
            SyncWord::Six,
        ],
        [
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Three,
            SyncWord::One,
            SyncWord::Five,
            SyncWord::Six,
        ],
        [
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Three,
            SyncWord::Four,
            SyncWord::Two,
            SyncWord::Six,
        ],
        [
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Three,
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Six,
        ],
        [
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Three,
            SyncWord::Four,
            SyncWord::Five,
            SyncWord::Three,
        ],
        [
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Three,
            SyncWord::One,
            SyncWord::Five,
            SyncWord::Three,
        ],
        [
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Three,
            SyncWord::Four,
            SyncWord::Two,
            SyncWord::Three,
        ],
        [
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Three,
            SyncWord::One,
            SyncWord::Two,
            SyncWord::Three,
        ],
    ];
    for (mask, row) in rows.iter().enumerate() {
        let traffic: Vec<_> = assignments
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, assignment)| *assignment)
            .collect();
        let layout = CarrierLayout::try_new(&[], &traffic).unwrap();
        for (index, expected) in row.iter().enumerate() {
            let sync = match layout.slot(&clock, index as u64).unwrap().role {
                SlotRole::Traffic { sync, .. } | SlotRole::Unassigned { sync } => sync,
                SlotRole::Reserved => panic!("no reserved slot"),
            };
            assert_eq!(sync, *expected);
        }
    }
}
#[test]
fn reserved_slots_and_arbitrary_bearer_order_keep_ownership_and_intervals_distinct() {
    let clock = FrameClock::try_new(0, 1000000, SAMPLE_RATE_HZ).unwrap();
    let layout = CarrierLayout::try_new(
        &[AssignedSlots::Full14],
        &[AssignedSlots::Full36, AssignedSlots::Full25],
    )
    .unwrap();
    for index in 0..12 {
        let role = layout.slot(&clock, index).unwrap().role;
        match index % 3 {
            0 => assert_eq!(role, SlotRole::Reserved),
            1 => assert_eq!(
                role,
                SlotRole::Traffic {
                    bearer: 1,
                    interval: index / 3,
                    sync: SyncWord::Two
                }
            ),
            _ => assert_eq!(
                role,
                SlotRole::Traffic {
                    bearer: 0,
                    interval: index / 3,
                    sync: SyncWord::Three
                }
            ),
        }
    }
}
#[test]
fn conflicting_assignments_unsupported_rates_and_overflow_are_rejected() {
    assert!(matches!(
        CarrierLayout::try_new(&[AssignedSlots::Full14], &[AssignedSlots::Full14]),
        Err(FrameError::Overlap(AssignedSlots::Full14))
    ));
    assert!(matches!(
        CarrierLayout::try_new(&[], &[AssignedSlots::Full25, AssignedSlots::Full25]),
        Err(FrameError::Overlap(AssignedSlots::Full25))
    ));
    assert!(matches!(
        CarrierLayout::try_new(&[], &[AssignedSlots::Half1]),
        Err(FrameError::FullRate(AssignedSlots::Half1))
    ));
    assert!(matches!(
        FrameClock::try_new(0, 149, SAMPLE_RATE_HZ),
        Err(FrameError::TickRate(149))
    ));
    assert!(matches!(
        FrameClock::try_new(0, 1000000, 194401),
        Err(FrameError::SampleRate(194401))
    ));
    assert!(matches!(
        PhysicalSlot::try_new(0),
        Err(FrameError::PhysicalSlot(0))
    ));
    assert!(matches!(
        PhysicalSlot::try_new(7),
        Err(FrameError::PhysicalSlot(7))
    ));
    let clock = FrameClock::try_new(u64::MAX, 1000000, SAMPLE_RATE_HZ).unwrap();
    assert_eq!(clock.at(0), Err(FrameError::Range));
    let clock = FrameClock::try_new(0, 1000000, SAMPLE_RATE_HZ).unwrap();
    assert_eq!(clock.at(u64::MAX), Err(FrameError::Range));
    assert_eq!(
        FullRatePair::try_new(AssignedSlots::Full14)
            .unwrap()
            .timing(&clock, u64::MAX),
        Err(FrameError::Range)
    );
}
