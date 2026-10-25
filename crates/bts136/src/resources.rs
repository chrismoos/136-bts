use crate::{
    l3::designation::AssignedSlots,
    phy::channel::{self, Band, DuplexFrequencies},
};
use std::{error::Error, fmt, sync::Arc};

const FULL_RATE_PAIRS: [AssignedSlots; 3] = [
    AssignedSlots::Full14,
    AssignedSlots::Full25,
    AssignedSlots::Full36,
];
const PAIR_SIZE: usize = 2;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationError {
    Channel,
    Exhausted,
    StaleLease,
}
impl fmt::Display for AllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "traffic allocation failed: {self:?}")
    }
}
impl Error for AllocationError {}
#[derive(Debug)]
pub struct TrafficLease {
    channel: u16,
    assignment: AssignedSlots,
    token: Arc<()>,
}
impl TrafficLease {
    pub fn channel(&self) -> u16 {
        self.channel
    }
    pub fn assigned_slots(&self) -> AssignedSlots {
        self.assignment
    }
}
#[derive(Debug)]
pub struct TrafficResources {
    channel: u16,
    reserved_mask: u8,
    frequencies: DuplexFrequencies,
    occupied: [Option<Arc<()>>; FULL_RATE_PAIRS.len()],
}
impl TrafficResources {
    pub fn try_new(
        band: Band,
        channel: u16,
        reserved: &[AssignedSlots],
    ) -> Result<Self, AllocationError> {
        let frequencies =
            channel::frequencies(band, channel).map_err(|_| AllocationError::Channel)?;
        let reserved_mask = reserved
            .iter()
            .fold(0, |mask, slots| mask | slots.slot_mask());
        Ok(Self {
            channel,
            reserved_mask,
            frequencies,
            occupied: std::array::from_fn(|_| None),
        })
    }
    pub fn frequencies(&self) -> DuplexFrequencies {
        self.frequencies
    }

    pub fn allocate_full_rate(&mut self) -> Result<TrafficLease, AllocationError> {
        for (assignment, occupied) in FULL_RATE_PAIRS.into_iter().zip(&mut self.occupied) {
            if occupied.is_some() || assignment.slot_mask() & self.reserved_mask != 0 {
                continue;
            }
            let token = Arc::new(());
            *occupied = Some(Arc::clone(&token));
            return Ok(TrafficLease {
                channel: self.channel,
                assignment,
                token,
            });
        }
        Err(AllocationError::Exhausted)
    }
    pub fn allocate_full_rate_pair(
        &mut self,
    ) -> Result<[TrafficLease; PAIR_SIZE], AllocationError> {
        let indices = {
            let mut available = FULL_RATE_PAIRS
                .iter()
                .zip(&self.occupied)
                .enumerate()
                .filter(|(_, (assignment, occupied))| {
                    occupied.is_none() && assignment.slot_mask() & self.reserved_mask == 0
                })
                .map(|(index, _)| index);
            [
                available.next().ok_or(AllocationError::Exhausted)?,
                available.next().ok_or(AllocationError::Exhausted)?,
            ]
        };
        Ok(indices.map(|index| {
            let token = Arc::new(());
            self.occupied[index] = Some(Arc::clone(&token));
            TrafficLease {
                channel: self.channel,
                assignment: FULL_RATE_PAIRS[index],
                token,
            }
        }))
    }
    pub fn release_full_rate_pair(
        &mut self,
        leases: &[TrafficLease; PAIR_SIZE],
    ) -> Result<(), AllocationError> {
        let indices = [self.lease_index(&leases[0])?, self.lease_index(&leases[1])?];
        if indices[0] == indices[1] {
            return Err(AllocationError::StaleLease);
        }
        for index in indices {
            self.occupied[index] = None;
        }
        Ok(())
    }
    pub fn release(&mut self, lease: &TrafficLease) -> Result<(), AllocationError> {
        let index = self.lease_index(lease)?;
        self.occupied[index] = None;
        Ok(())
    }
    fn lease_index(&self, lease: &TrafficLease) -> Result<usize, AllocationError> {
        if lease.channel != self.channel {
            return Err(AllocationError::StaleLease);
        }
        for (index, (assignment, occupied)) in
            FULL_RATE_PAIRS.into_iter().zip(&self.occupied).enumerate()
        {
            if assignment != lease.assignment {
                continue;
            }
            if occupied
                .as_ref()
                .is_some_and(|token| Arc::ptr_eq(token, &lease.token))
            {
                return Ok(index);
            }
        }
        Err(AllocationError::StaleLease)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_pair_is_preserved_and_two_full_rate_calls_use_disjoint_slots() {
        let mut resources =
            TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap();
        let first = resources.allocate_full_rate().unwrap();
        let second = resources.allocate_full_rate().unwrap();
        assert_eq!(first.assigned_slots(), AssignedSlots::Full25);
        assert_eq!(second.assigned_slots(), AssignedSlots::Full36);
        assert_eq!(
            first.assigned_slots().slot_mask() & second.assigned_slots().slot_mask(),
            0
        );
        assert!(matches!(
            resources.allocate_full_rate(),
            Err(AllocationError::Exhausted)
        ));
        resources.release(&first).unwrap();
        let replacement = resources.allocate_full_rate().unwrap();
        assert_eq!(replacement.assigned_slots(), first.assigned_slots());
        assert_eq!(resources.release(&first), Err(AllocationError::StaleLease));
        assert!(matches!(
            resources.allocate_full_rate(),
            Err(AllocationError::Exhausted)
        ));
    }
    #[test]
    fn partial_reservations_and_foreign_leases_cannot_collide() {
        let mut resources = TrafficResources::try_new(
            Band::Cellular800,
            1,
            &[AssignedSlots::Half4, AssignedSlots::Half5],
        )
        .unwrap();
        let lease = resources.allocate_full_rate().unwrap();
        assert_eq!(lease.assigned_slots(), AssignedSlots::Full36);
        let mut other = TrafficResources::try_new(
            Band::Cellular800,
            1,
            &[AssignedSlots::Half4, AssignedSlots::Half5],
        )
        .unwrap();
        let own = other.allocate_full_rate().unwrap();
        assert_eq!(own.assigned_slots(), lease.assigned_slots());
        assert_eq!(other.release(&lease), Err(AllocationError::StaleLease));
        assert!(matches!(
            other.allocate_full_rate(),
            Err(AllocationError::Exhausted)
        ));
        assert!(matches!(
            resources.allocate_full_rate(),
            Err(AllocationError::Exhausted)
        ));
        resources.release(&lease).unwrap();
        assert_eq!(resources.release(&lease), Err(AllocationError::StaleLease));
        assert!(matches!(
            TrafficResources::try_new(Band::Cellular800, 2048, &[]),
            Err(AllocationError::Channel)
        ));
    }
}
#[test]
fn paired_allocation_and_release_do_not_partially_change_capacity_on_failure() {
    let mut resources =
        TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap();
    let first = resources.allocate_full_rate().unwrap();
    assert!(matches!(
        resources.allocate_full_rate_pair(),
        Err(AllocationError::Exhausted)
    ));
    let second = resources.allocate_full_rate().unwrap();
    assert_eq!(second.assigned_slots(), AssignedSlots::Full36);
    resources.release(&first).unwrap();
    resources.release(&second).unwrap();
    let pair = resources.allocate_full_rate_pair().unwrap();
    resources.release(&pair[1]).unwrap();
    let replacement = resources.allocate_full_rate().unwrap();
    assert_eq!(
        resources.release_full_rate_pair(&pair),
        Err(AllocationError::StaleLease)
    );
    assert!(matches!(
        resources.allocate_full_rate(),
        Err(AllocationError::Exhausted)
    ));
    let [first, _stale] = pair;
    resources
        .release_full_rate_pair(&[first, replacement])
        .unwrap();
    assert!(resources.allocate_full_rate_pair().is_ok());
}
#[test]
fn foreign_pair_release_preserves_both_existing_bearers() {
    let mut resources =
        TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap();
    let mut foreign =
        TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap();
    let own = resources.allocate_full_rate_pair().unwrap();
    let other = foreign.allocate_full_rate_pair().unwrap();
    assert_eq!(
        resources.release_full_rate_pair(&other),
        Err(AllocationError::StaleLease)
    );
    assert!(matches!(
        resources.allocate_full_rate_pair(),
        Err(AllocationError::Exhausted)
    ));
    resources.release_full_rate_pair(&own).unwrap();
    assert!(resources.allocate_full_rate_pair().is_ok());
}
