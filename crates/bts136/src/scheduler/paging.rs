use super::broadcast::{BroadcastClock, Superframe};
use crate::{
    l3::broadcast::{DcchStructure, MandatoryBroadcast, SlotConfiguration},
    link::{
        rach::MobileIdentity,
        spach::{
            SpachError,
            paging::{PageBody, PageFrame},
        },
    },
    phy::control::INFORMATION_BITS,
};
use std::{error::Error as StdError, fmt};
const CONTROL_PHASES: u16 = 32;
const DCCH_SLOTS: u16 = 2;
const PERMANENT_ID_LOW_BITS: u32 = 16;
const MIN_BITS: u32 = 34;
const IMSI_BITS: u32 = 50;
#[derive(Debug)]
pub enum Error {
    Arrangement,
    Identity,
    Phase,
    Primary,
    Range,
    Page(SpachError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "paging schedule failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug, Clone, Copy)]
pub struct PagingAllocation {
    first_phase: u16,
    phase_count: u16,
}
impl PagingAllocation {
    pub fn try_new(structure: &DcchStructure) -> Result<Self, Error> {
        MandatoryBroadcast::DcchStructure(structure.clone())
            .encode()
            .map_err(|_| Error::Arrangement)?;
        if structure.slot_configuration != SlotConfiguration::OneFullRate
            || structure.pch_displacement != 0
        {
            return Err(Error::Arrangement);
        }
        let first_phase = u16::from(structure.fbcch_slots)
            + u16::from(structure.ebcch_slots)
            + u16::from(structure.sbcch_slots)
            + u16::from(structure.reserved_slots);
        let excluded = u16::from(structure.non_pch_subchannel_code) * DCCH_SLOTS;
        let phase_count = CONTROL_PHASES
            .checked_sub(first_phase + excluded)
            .filter(|count| *count > 0)
            .ok_or(Error::Arrangement)?;
        Ok(Self {
            first_phase,
            phase_count,
        })
    }
    // IS-136.1-A §§4.10 and 4.10.2. TMSI registration does not change the permanent paging identity.
    pub fn phase(&self, permanent_identity: MobileIdentity) -> Result<u8, Error> {
        let value = match permanent_identity {
            MobileIdentity::Min34(value) if value > 0 && value < 1 << MIN_BITS => value,
            MobileIdentity::Imsi50(value) if value > 0 && value < 1 << IMSI_BITS => value,
            _ => return Err(Error::Identity),
        };
        let allocation_id = (value & ((1 << PERMANENT_ID_LOW_BITS) - 1)) as u16;
        Ok(((allocation_id / DCCH_SLOTS) % self.phase_count + self.first_phase) as u8)
    }
    fn contains(&self, phase: u8) -> bool {
        (self.first_phase..self.first_phase + self.phase_count).contains(&u16::from(phase))
    }
    // TIA-136-121-A §7 advances by two logical SPACH slots and skips secondary superframes.
    pub(crate) fn continuation(&self, superframe: u64, phase: u8) -> Option<(u64, u8)> {
        if !superframe.is_multiple_of(2)
            || u16::from(phase) < self.first_phase
            || u16::from(phase) >= CONTROL_PHASES
        {
            return None;
        }
        let phase = u16::from(phase) + DCCH_SLOTS;
        if phase < CONTROL_PHASES {
            Some((superframe, phase as u8))
        } else {
            Some((
                superframe.checked_add(2)?,
                (self.first_phase + phase - CONTROL_PHASES) as u8,
            ))
        }
    }
    pub(crate) fn continuation_span(
        &self,
        superframe: u64,
        phase: u8,
        frames: usize,
    ) -> Option<usize> {
        if frames == 0 {
            return None;
        }
        let start = (superframe, phase);
        let mut end = start;
        for _ in 1..frames {
            end = self.continuation(end.0, end.1)?;
        }
        let slots = u128::from(end.0 - start.0) * u128::from(CONTROL_PHASES - self.first_phase)
            + u128::from(end.1)
            + 1
            - u128::from(start.1);
        usize::try_from(slots).ok()
    }
}
pub struct PageRepeat {
    primary_index: u64,
    secondary_index: u64,
    phase: u8,
    information: [bool; INFORMATION_BITS],
}
impl PageRepeat {
    /// The baseline uses paging class one with no displacement or paging-frame modifier.
    pub fn try_new(
        clock: &BroadcastClock,
        primary_index: u64,
        allocation: PagingAllocation,
        phase: u8,
        page: &PageFrame,
    ) -> Result<Self, Error> {
        if clock.at(primary_index).superframe != Superframe::Primary {
            return Err(Error::Primary);
        }
        if !allocation.contains(phase) || page.flags.paging_frame_modifier {
            return Err(Error::Phase);
        }
        if let PageBody::TripleMin34(identities) = &page.body {
            for identity in identities.iter().flatten() {
                if allocation.phase(MobileIdentity::Min34(*identity))? != phase {
                    return Err(Error::Phase);
                }
            }
        }
        let secondary_index = primary_index.checked_add(1).ok_or(Error::Range)?;
        let information = page.encode().map_err(Error::Page)?;
        Ok(Self {
            primary_index,
            secondary_index,
            phase,
            information,
        })
    }
    // IS-136.1-A §4.6 repeats primary PCH information at the same phase in the secondary superframe.
    pub fn information(
        &self,
        superframe_index: u64,
        phase: u8,
    ) -> Option<&[bool; INFORMATION_BITS]> {
        (phase == self.phase
            && (superframe_index == self.primary_index || superframe_index == self.secondary_index))
            .then_some(&self.information)
    }
}
#[cfg(test)]
mod tests;
