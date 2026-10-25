use crate::scheduler::broadcast::{BroadcastClock, CONTROL_SLOTS, Superframe};
use sdr_hal::TxRadioHealth;
use std::{error::Error as StdError, fmt};
const MAX_IMPLEMENTED_EBCCH_SLOTS: u8 = crate::scheduler::broadcast::MAX_EBCCH_SLOTS as u8;

#[derive(Debug)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid broadcast completion layout")
    }
}
impl StdError for Error {}
pub(crate) struct BroadcastCompletion {
    slots: SlotRangeCompletion,
}
pub(crate) struct NotificationCompletion {
    slots: SlotRangeCompletion,
    clock: BroadcastClock,
    primary: Option<u64>,
}
struct SlotRangeCompletion {
    first: u8,
    end: u8,
    superframe: Option<u64>,
    blocked_through: Option<u64>,
    next_phase: u8,
    invalid: bool,
}
impl BroadcastCompletion {
    pub fn try_new(fbcch: u8, ebcch: u8) -> Result<Self, Error> {
        Ok(Self {
            slots: SlotRangeCompletion::new(0, first_spach(fbcch, ebcch)?),
        })
    }
    pub fn invalidate_through(&mut self, superframe: u64) {
        self.slots.invalidate_through(superframe);
    }
    pub fn completed(&mut self, superframe: u64, phase: u8) -> bool {
        self.slots.completed(superframe, phase)
    }
}
impl NotificationCompletion {
    pub fn try_new(fbcch: u8, ebcch: u8, clock: BroadcastClock) -> Result<Self, Error> {
        Ok(Self {
            slots: SlotRangeCompletion::new(first_spach(fbcch, ebcch)?, CONTROL_SLOTS as u8),
            clock,
            primary: None,
        })
    }
    pub fn invalidate_through(&mut self, superframe: u64) {
        self.slots.invalidate_through(superframe);
        self.primary = None;
    }
    pub fn completed(&mut self, superframe: u64, phase: u8) -> bool {
        if !self.slots.completed(superframe, phase) {
            return false;
        }
        match self.clock.at(superframe).superframe {
            Superframe::Primary => {
                self.primary = Some(superframe);
                false
            }
            Superframe::Secondary => self
                .primary
                .take()
                .is_some_and(|primary| primary.checked_add(1) == Some(superframe)),
        }
    }
}
fn first_spach(fbcch: u8, ebcch: u8) -> Result<u8, Error> {
    if !(crate::link::broadcast::MIN_SLOTS..=crate::link::broadcast::MAX_SLOTS)
        .contains(&usize::from(fbcch))
        || !(1..=MAX_IMPLEMENTED_EBCCH_SLOTS).contains(&ebcch)
    {
        return Err(Error);
    }
    Ok(fbcch + ebcch)
}
impl SlotRangeCompletion {
    fn new(first: u8, end: u8) -> Self {
        Self {
            first,
            end,
            superframe: None,
            blocked_through: None,
            next_phase: first,
            invalid: false,
        }
    }
    pub fn invalidate_through(&mut self, superframe: u64) {
        let superframe = self
            .superframe
            .map_or(superframe, |current| current.max(superframe));
        self.blocked_through = Some(
            self.blocked_through
                .map_or(superframe, |previous| previous.max(superframe)),
        );
        self.superframe = None;
        self.next_phase = self.first;
        self.invalid = false;
    }
    pub fn completed(&mut self, superframe: u64, phase: u8) -> bool {
        if phase < self.first
            || phase >= self.end
            || self.blocked_through.is_some_and(|last| superframe <= last)
            || self.superframe.is_some_and(|current| superframe < current)
        {
            return false;
        }
        if self.superframe != Some(superframe) {
            self.superframe = Some(superframe);
            self.next_phase = self.first;
            self.invalid = false;
        }
        if self.invalid || phase < self.next_phase {
            return false;
        }
        if phase != self.next_phase {
            self.invalid = true;
            return false;
        }
        self.next_phase += 1;
        self.next_phase == self.end
    }
}
pub(crate) fn disrupted(previous: TxRadioHealth, current: TxRadioHealth) -> bool {
    previous.underflows != current.underflows
        || previous.late_packets != current.late_packets
        || previous.sequence_errors != current.sequence_errors
        || previous.dropped_packets != current.dropped_packets
        || previous.unknown_events != current.unknown_events
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIRST_SPACH_PHASE: u8 = 5;
    fn notifications(initial: Superframe) -> NotificationCompletion {
        NotificationCompletion::try_new(
            4,
            1,
            BroadcastClock::try_new(crate::scheduler::broadcast::Position {
                hyperframe_counter: 11,
                superframe: initial,
            })
            .unwrap(),
        )
        .unwrap()
    }
    fn notification_half(tracker: &mut NotificationCompletion, sf: u64) -> bool {
        let mut result = false;
        for phase in FIRST_SPACH_PHASE..CONTROL_SLOTS as u8 {
            result |= tracker.completed(sf, phase);
        }
        result
    }
    #[test]
    fn notification_proof_requires_all_spach_slots_in_both_adjacent_superframes() {
        let mut tracker = notifications(Superframe::Primary);
        for phase in 0..FIRST_SPACH_PHASE {
            assert!(!tracker.completed(0, phase));
        }
        assert!(!notification_half(&mut tracker, 0));
        for phase in FIRST_SPACH_PHASE..CONTROL_SLOTS as u8 {
            assert_eq!(
                tracker.completed(1, phase),
                phase == CONTROL_SLOTS as u8 - 1
            );
        }
        assert!(!tracker.completed(1, CONTROL_SLOTS as u8 - 1));
        assert!(!notification_half(&mut tracker, 0));
    }
    #[test]
    fn missing_notification_slots_do_not_combine_halves_from_different_hyperframes() {
        let mut tracker = notifications(Superframe::Primary);
        for phase in FIRST_SPACH_PHASE..CONTROL_SLOTS as u8 {
            if phase != 10 {
                assert!(!tracker.completed(0, phase));
            }
        }
        assert!(!notification_half(&mut tracker, 1));
        assert!(!notification_half(&mut tracker, 2));
        for phase in FIRST_SPACH_PHASE..CONTROL_SLOTS as u8 {
            if phase != 8 {
                assert!(!tracker.completed(3, phase));
            }
        }
        assert!(!notification_half(&mut tracker, 4));
        assert!(notification_half(&mut tracker, 5));
    }
    #[test]
    fn notification_recovery_respects_initial_secondary_rollover_and_queued_damage() {
        let mut tracker = notifications(Superframe::Secondary);
        assert!(!notification_half(&mut tracker, 0));
        assert!(!notification_half(&mut tracker, 1));
        assert_eq!(tracker.clock.at(1).hyperframe_counter, 0);
        assert!(notification_half(&mut tracker, 2));
        assert!(!notification_half(&mut tracker, 3));
        tracker.invalidate_through(4);
        assert!(!notification_half(&mut tracker, 4));
        assert!(!notification_half(&mut tracker, 5));
        assert!(notification_half(&mut tracker, 6));
        let mut wide = NotificationCompletion::try_new(
            10,
            1,
            BroadcastClock::try_new(crate::scheduler::broadcast::Position {
                hyperframe_counter: 0,
                superframe: Superframe::Primary,
            })
            .unwrap(),
        )
        .unwrap();
        for sf in 0..=1 {
            for phase in 0..CONTROL_SLOTS as u8 {
                assert_eq!(
                    wide.completed(sf, phase),
                    sf == 1 && phase == CONTROL_SLOTS as u8 - 1
                );
            }
        }
    }
    #[test]
    fn completion_requires_a_contiguous_cycle_and_cannot_be_replayed() {
        let mut tracker = BroadcastCompletion::try_new(4, 1).unwrap();
        for phase in [0, 0, 1, 2, 3] {
            assert!(!tracker.completed(0, phase));
        }
        assert!(tracker.completed(0, 4));
        assert!(!tracker.completed(0, 4));
        for phase in [0, 2, 1, 3, 4] {
            assert!(!tracker.completed(1, phase));
        }
        for phase in 0..5 {
            assert_eq!(tracker.completed(2, phase), phase == 4);
        }
        assert!(!tracker.completed(1, 4));
    }
    #[test]
    fn disruption_blocks_queued_cycles_until_a_fresh_complete_cycle() {
        let mut tracker = BroadcastCompletion::try_new(10, 1).unwrap();
        tracker.completed(0, 0);
        tracker.invalidate_through(2);
        tracker.invalidate_through(1);
        for sf in 0..=2 {
            for phase in 0..11 {
                assert!(!tracker.completed(sf, phase));
            }
        }
        for phase in 1..11 {
            assert!(!tracker.completed(3, phase));
        }
        for phase in 0..11 {
            assert_eq!(tracker.completed(4, phase), phase == 10);
        }
        tracker.invalidate_through(1);
        for phase in 0..11 {
            assert!(!tracker.completed(4, phase));
        }
        for phase in 0..11 {
            assert_eq!(tracker.completed(5, phase), phase == 10);
        }
    }
    #[test]
    fn healthy_burst_ack_changes_do_not_discard_broadcast_work() {
        let before = TxRadioHealth::default();
        let mut after = before;
        after.burst_acks += 1;
        assert!(!disrupted(before, after));
        for after in [
            TxRadioHealth {
                underflows: 1,
                ..before
            },
            TxRadioHealth {
                late_packets: 1,
                ..before
            },
            TxRadioHealth {
                sequence_errors: 1,
                ..before
            },
            TxRadioHealth {
                dropped_packets: 1,
                ..before
            },
            TxRadioHealth {
                unknown_events: 1,
                ..before
            },
        ] {
            assert!(disrupted(before, after));
            assert!(disrupted(after, before));
        }
        assert!(BroadcastCompletion::try_new(2, 1).is_err());
        assert!(BroadcastCompletion::try_new(11, 1).is_err());
        assert!(BroadcastCompletion::try_new(4, 9).is_err());
    }
}
