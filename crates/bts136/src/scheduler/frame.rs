use std::{error::Error, fmt};

use crate::{l3::designation::AssignedSlots, phy::sync::SyncWord};

// IS-136.2-A Figure 1.2-1 defines six 162-symbol slots per 40 ms frame.
const SLOTS_PER_FRAME: u64 = 6;
const SLOTS_PER_SECOND: u64 = 150;
const FULL_RATE_SLOT_PERIOD: u64 = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalSlot(u8);
impl PhysicalSlot {
    pub fn try_new(number: u8) -> Result<Self, FrameError> {
        if number == 0 || u64::from(number) > SLOTS_PER_FRAME {
            return Err(FrameError::PhysicalSlot(number));
        }
        Ok(Self(number))
    }
    pub fn number(self) -> u8 {
        self.0
    }
    fn index(self) -> usize {
        usize::from(self.0 - 1)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotTiming {
    pub slot_index: u64,
    pub frame_number: u64,
    pub slot: PhysicalSlot,
    pub start_tick: u64,
    pub end_tick: u64,
    pub first_sample: u64,
    pub sample_count: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    TickRate(u64),
    SampleRate(u64),
    PhysicalSlot(u8),
    FullRate(AssignedSlots),
    Overlap(AssignedSlots),
    Range,
}
impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "carrier frame mapping failed: {self:?}")
    }
}
impl Error for FrameError {}
#[derive(Debug, Clone, Copy)]
pub struct FrameClock {
    epoch_tick: u64,
    tick_rate_hz: u64,
    samples_per_slot: u64,
}
impl FrameClock {
    /// Epoch is the nominal slot-one boundary. The streaming adapter must account for pulse-filter group delay and IQ pre-roll.
    pub fn try_new(
        epoch_tick: u64,
        tick_rate_hz: u64,
        sample_rate_hz: u64,
    ) -> Result<Self, FrameError> {
        if tick_rate_hz < SLOTS_PER_SECOND {
            return Err(FrameError::TickRate(tick_rate_hz));
        }
        if sample_rate_hz < SLOTS_PER_SECOND || !sample_rate_hz.is_multiple_of(SLOTS_PER_SECOND) {
            return Err(FrameError::SampleRate(sample_rate_hz));
        }
        Ok(Self {
            epoch_tick,
            tick_rate_hz,
            samples_per_slot: sample_rate_hz / SLOTS_PER_SECOND,
        })
    }
    /// Tick boundaries round upward from the absolute epoch, bounding quantization below one tick without cumulative drift.
    pub fn at(&self, slot_index: u64) -> Result<SlotTiming, FrameError> {
        let next = slot_index.checked_add(1).ok_or(FrameError::Range)?;
        let first_sample = slot_index
            .checked_mul(self.samples_per_slot)
            .ok_or(FrameError::Range)?;
        first_sample
            .checked_add(self.samples_per_slot)
            .ok_or(FrameError::Range)?;
        Ok(SlotTiming {
            slot_index,
            frame_number: slot_index / SLOTS_PER_FRAME,
            slot: PhysicalSlot((slot_index % SLOTS_PER_FRAME + 1) as u8),
            start_tick: self.tick_at(slot_index)?,
            end_tick: self.tick_at(next)?,
            first_sample,
            sample_count: self.samples_per_slot,
        })
    }
    fn tick_at(&self, index: u64) -> Result<u64, FrameError> {
        let offset = (u128::from(index) * u128::from(self.tick_rate_hz))
            .div_ceil(u128::from(SLOTS_PER_SECOND));
        self.epoch_tick
            .checked_add(u64::try_from(offset).map_err(|_| FrameError::Range)?)
            .ok_or(FrameError::Range)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullRatePair(AssignedSlots);
impl FullRatePair {
    pub fn try_new(assignment: AssignedSlots) -> Result<Self, FrameError> {
        if !matches!(
            assignment,
            AssignedSlots::Full14 | AssignedSlots::Full25 | AssignedSlots::Full36
        ) {
            return Err(FrameError::FullRate(assignment));
        }
        Ok(Self(assignment))
    }
    pub fn assignment(self) -> AssignedSlots {
        self.0
    }
    pub fn timing(self, clock: &FrameClock, interval: u64) -> Result<SlotTiming, FrameError> {
        let slot_index = interval
            .checked_mul(FULL_RATE_SLOT_PERIOD)
            .and_then(|index| index.checked_add(self.phase()))
            .ok_or(FrameError::Range)?;
        clock.at(slot_index)
    }
    fn phase(self) -> u64 {
        match self.0 {
            AssignedSlots::Full14 => 0,
            AssignedSlots::Full25 => 1,
            AssignedSlots::Full36 => 2,
            _ => unreachable!("full-rate assignment validated in constructor"),
        }
    }
    fn sync(self) -> SyncWord {
        SyncWord::ALL[self.phase() as usize]
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRole {
    Reserved,
    Unassigned {
        sync: SyncWord,
    },
    Traffic {
        bearer: usize,
        interval: u64,
        sync: SyncWord,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedSlot {
    pub timing: SlotTiming,
    pub role: SlotRole,
}
#[derive(Debug)]
pub struct CarrierLayout {
    reserved_mask: u8,
    pairs: Vec<FullRatePair>,
    owners: [Option<usize>; SLOTS_PER_FRAME as usize],
}
impl CarrierLayout {
    pub fn try_new(
        reserved: &[AssignedSlots],
        traffic: &[AssignedSlots],
    ) -> Result<Self, FrameError> {
        let mut reserved_mask = 0;
        for assignment in reserved {
            let mask = assignment.slot_mask();
            if reserved_mask & mask != 0 {
                return Err(FrameError::Overlap(*assignment));
            }
            reserved_mask |= mask;
        }
        let mut owners = [None; SLOTS_PER_FRAME as usize];
        let mut pairs = Vec::new();
        let mut occupied = reserved_mask;
        for (bearer, assignment) in traffic.iter().enumerate() {
            let pair = FullRatePair::try_new(*assignment)?;
            let mask = assignment.slot_mask();
            if occupied & mask != 0 {
                return Err(FrameError::Overlap(*assignment));
            }
            for (slot, owner) in owners.iter_mut().enumerate() {
                if mask & (1 << slot) != 0 {
                    *owner = Some(bearer);
                }
            }
            occupied |= mask;
            pairs.push(pair);
        }
        Ok(Self {
            reserved_mask,
            pairs,
            owners,
        })
    }
    // IS-136.2-A Table 1.2.4-1 repeats the assigned sync for a full-rate pair. Unassigned slots use half-rate identities.
    pub fn slot(&self, clock: &FrameClock, slot_index: u64) -> Result<PlannedSlot, FrameError> {
        let timing = clock.at(slot_index)?;
        let index = timing.slot.index();
        let role = if self.reserved_mask & (1 << index) != 0 {
            SlotRole::Reserved
        } else if let Some(bearer) = self.owners[index] {
            let pair = self.pairs[bearer];
            SlotRole::Traffic {
                bearer,
                interval: (slot_index - pair.phase()) / FULL_RATE_SLOT_PERIOD,
                sync: pair.sync(),
            }
        } else {
            SlotRole::Unassigned {
                sync: SyncWord::ALL[index],
            }
        };
        Ok(PlannedSlot { timing, role })
    }
}

#[cfg(test)]
mod tests;
