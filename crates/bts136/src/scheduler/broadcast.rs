use crate::{
    l3::{
        broadcast::{MandatoryBroadcast, SlotConfiguration, WireError},
        designation::AssignedSlots,
        extended_broadcast::ExtendedBroadcast,
    },
    link::broadcast::{BroadcastChanges, BroadcastError, encode_ebcch_begin, encode_fbcch},
    phy::{
        control::{self, ControlError, INFORMATION_BITS, LogicalChannel, SharedFeedback},
        sync::SyncWord,
        traffic::SLOT_BITS,
    },
};
use std::{error::Error as StdError, fmt};
mod changes;
mod transition;
pub use crate::link::broadcast::BroadcastSignaling;
pub use changes::{
    BroadcastChange, BroadcastChangeError, BroadcastChangeState, BroadcastScope, ChangePhase,
};
pub use transition::{
    BroadcastContent, BroadcastContentStorageError, BroadcastTransition, BroadcastTransitionError,
};
pub(crate) const CONTROL_SLOTS: u64 = 32;
const CONTROL_PERIOD: u64 = 3;
pub const PHYSICAL_SLOTS: u64 = CONTROL_SLOTS * CONTROL_PERIOD;
#[derive(Debug)]
pub enum Error {
    Counter(u8),
    Messages,
    Arrangement,
    Slot,
    Spach,
    Wire(WireError),
    Framing(BroadcastError),
    Physical(ControlError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "broadcast superframe failed: {self:?}")
    }
}
impl StdError for Error {}
pub struct ControlSlot {
    pub channel: LogicalChannel,
    pub phase: u8,
    pub bits: [bool; SLOT_BITS],
}
pub(crate) const MAX_EBCCH_SLOTS: usize = 8;
const CLOCK_EBCCH_SLOTS: usize = 2;
pub struct BroadcastSuperframe {
    fbcch: Vec<[bool; INFORMATION_BITS]>,
    ebcch: Vec<[bool; INFORMATION_BITS]>,
    ebcch_slots: usize,
    ebcch_start: usize,
    dvcc: u8,
    bcch_notification: Option<bool>,
}
impl BroadcastSuperframe {
    /// The input F-BCCH slot count is a lower bound for the returned layout.
    pub fn fit_optional(
        pair: AssignedSlots,
        messages: &[MandatoryBroadcast; 5],
        optional: &[Vec<u8>],
        changes: BroadcastChanges,
    ) -> Result<[MandatoryBroadcast; 5], Error> {
        let MandatoryBroadcast::DcchStructure(structure) = &messages[0] else {
            return Err(Error::Messages);
        };
        let first = usize::from(structure.fbcch_slots);
        if !(crate::link::broadcast::MIN_SLOTS..=crate::link::broadcast::MAX_SLOTS).contains(&first)
        {
            return Err(Error::Arrangement);
        }
        for count in first..=crate::link::broadcast::MAX_SLOTS {
            let mut fitted = messages.clone();
            let MandatoryBroadcast::DcchStructure(structure) = &mut fitted[0] else {
                return Err(Error::Messages);
            };
            structure.fbcch_slots = u8::try_from(count).map_err(|_| Error::Arrangement)?;
            match Self::try_new_with_optional(pair, &fitted, optional, changes) {
                Ok(_) => return Ok(fitted),
                Err(Error::Framing(BroadcastError::Capacity)) => {}
                Err(error) => return Err(error),
            }
        }
        Err(Error::Framing(BroadcastError::Capacity))
    }

    /// Snapshot includes its hyperframe counter and primary/secondary indicator. Construct a new snapshot at the next boundary.
    pub fn try_new(
        pair: AssignedSlots,
        messages: &[MandatoryBroadcast; 5],
        changes: BroadcastChanges,
    ) -> Result<Self, Error> {
        Self::try_new_with_optional(pair, messages, &[], changes)
    }
    pub fn try_new_with_optional(
        pair: AssignedSlots,
        messages: &[MandatoryBroadcast; 5],
        optional: &[Vec<u8>],
        changes: BroadcastChanges,
    ) -> Result<Self, Error> {
        let [
            MandatoryBroadcast::DcchStructure(structure),
            MandatoryBroadcast::AccessParameters(_),
            MandatoryBroadcast::SelectionParameters(_),
            MandatoryBroadcast::RegistrationParameters(_),
            MandatoryBroadcast::SystemIdentity(_),
        ] = messages
        else {
            return Err(Error::Messages);
        };
        if structure.slot_configuration != SlotConfiguration::OneFullRate
            || !(1..=MAX_EBCCH_SLOTS as u8).contains(&structure.ebcch_slots)
            || structure.sbcch_slots != 0
            || structure.reserved_slots != 0
        {
            return Err(Error::Arrangement);
        }
        // IS-136.1-A Table 4-3 requires slots 1 and 4 for this advertised arrangement.
        if pair != AssignedSlots::Full14 {
            return Err(Error::Arrangement);
        }
        let mut encoded: Vec<_> = messages
            .iter()
            .map(MandatoryBroadcast::encode)
            .collect::<Result<_, _>>()
            .map_err(Error::Wire)?;
        encoded.extend_from_slice(optional);
        let references: Vec<_> = encoded.iter().map(Vec::as_slice).collect();
        let fbcch = encode_fbcch(&references, changes, usize::from(structure.fbcch_slots))
            .map_err(Error::Framing)?;
        let neighbors = ExtendedBroadcast::NeighborCell {
            service_offering_offset_code: 0,
        }
        .encode()
        .map_err(Error::Wire)?;
        let regulatory = ExtendedBroadcast::StandardRegulatoryConfiguration
            .encode()
            .map_err(Error::Wire)?;
        let mut ebcch = vec![
            encode_ebcch_begin(&[&neighbors, &regulatory], structure.ebcch_slots)
                .map_err(Error::Framing)?,
        ];
        while ebcch.len() < usize::from(structure.ebcch_slots) {
            ebcch.push(encode_ebcch_begin(&[], structure.ebcch_slots).map_err(Error::Framing)?);
        }
        Ok(Self {
            fbcch,
            ebcch,
            ebcch_slots: usize::from(structure.ebcch_slots),
            ebcch_start: 0,
            dvcc: structure.dvcc,
            bcch_notification: None,
        })
    }
    pub(crate) fn set_neighbors(&mut self, packet: &[u8], sf: u64) -> Result<(), Error> {
        let frames = neighbor_cycle(packet)?;
        self.ebcch = frames;
        self.ebcch_start = ((sf % self.ebcch.len() as u64) * self.ebcch_slots as u64
            % self.ebcch.len() as u64) as usize;
        Ok(())
    }
    pub(crate) fn set_time_date(
        &mut self,
        time: crate::l3::time_date::TimeAndDate,
    ) -> Result<(), Error> {
        if self.ebcch.len() < CLOCK_EBCCH_SLOTS {
            return Err(Error::Arrangement);
        }
        let cycle = self.ebcch.len();
        self.ebcch[cycle - 1] =
            encode_ebcch_begin(&[&time.encode()], cycle as u8).map_err(Error::Framing)?;
        Ok(())
    }
    pub fn channel(&self, slot: u64) -> Result<Option<LogicalChannel>, Error> {
        let Some(phase) = self.phase(slot)? else {
            return Ok(None);
        };
        Ok(Some(if usize::from(phase) < self.fbcch.len() {
            LogicalChannel::Fbcch
        } else if usize::from(phase) < self.fbcch.len() + self.ebcch_slots {
            LogicalChannel::Ebcch
        } else {
            LogicalChannel::Spach
        }))
    }
    pub fn encode_slot(
        &self,
        slot: u64,
        feedback: &SharedFeedback,
        spach: Option<&[bool; INFORMATION_BITS]>,
    ) -> Result<Option<ControlSlot>, Error> {
        let channel = self.channel(slot)?;
        if spach.is_some() && channel != Some(LogicalChannel::Spach) {
            return Err(Error::Spach);
        }
        let Some(channel) = channel else {
            return Ok(None);
        };
        let phase = self
            .phase(slot)?
            .expect("control channel implies assigned phase");
        let mut information = [false; INFORMATION_BITS];
        match channel {
            LogicalChannel::Fbcch => information = self.fbcch[usize::from(phase)],
            LogicalChannel::Ebcch => {
                information = self.ebcch
                    [(self.ebcch_start + usize::from(phase) - self.fbcch.len()) % self.ebcch.len()]
            }
            LogicalChannel::Spach => {
                if let Some(spach) = spach {
                    information = *spach;
                }
                if let Some(notification) = self.bcch_notification {
                    // TIA-136-122-B Figure 29 places BCN after the three usage bits and PCON.
                    const BCN_BIT: usize = 4;
                    information[BCN_BIT] = notification;
                }
            }
            _ => unreachable!("superframe arrangement validated at construction"),
        }
        let bits = control::encode_forward(
            SyncWord::One,
            phase,
            feedback,
            &information,
            channel,
            self.dvcc,
        )
        .map_err(Error::Physical)?;
        Ok(Some(ControlSlot {
            channel,
            phase,
            bits,
        }))
    }
    fn phase(&self, slot: u64) -> Result<Option<u8>, Error> {
        if slot >= PHYSICAL_SLOTS {
            return Err(Error::Slot);
        }
        Ok(if slot.is_multiple_of(CONTROL_PERIOD) {
            Some((slot / CONTROL_PERIOD) as u8)
        } else {
            None
        })
    }
}

const HYPERFRAME_COUNTER_VALUES: u128 = 12;
const SUPERFRAMES_PER_HYPERFRAME: u128 = 2;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Superframe {
    Primary,
    Secondary,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub hyperframe_counter: u8,
    pub superframe: Superframe,
}
#[derive(Debug, Clone, Copy)]
pub struct BroadcastClock {
    initial: Position,
}
impl BroadcastClock {
    pub fn snapshot_with_signaling(
        &self,
        superframe_index: u64,
        pair: AssignedSlots,
        messages: &[MandatoryBroadcast; 5],
        optional: &[Vec<u8>],
        signaling: BroadcastSignaling,
    ) -> Result<BroadcastSuperframe, Error> {
        let mut snapshot = self.snapshot_with_optional(
            superframe_index,
            pair,
            messages,
            optional,
            signaling.changes,
        )?;
        snapshot.bcch_notification = Some(signaling.notification);
        Ok(snapshot)
    }
    pub fn try_new(initial: Position) -> Result<Self, Error> {
        if u128::from(initial.hyperframe_counter) >= HYPERFRAME_COUNTER_VALUES {
            return Err(Error::Counter(initial.hyperframe_counter));
        }
        Ok(Self { initial })
    }
    // IS-136.1-A §§4.6 and 6.5, paired superframes with an HFC ranging from zero through eleven.
    pub fn at(&self, superframe_index: u64) -> Position {
        let offset = u128::from(superframe_index)
            + u128::from(self.initial.superframe == Superframe::Secondary);
        let counter = (u128::from(self.initial.hyperframe_counter)
            + offset / SUPERFRAMES_PER_HYPERFRAME)
            % HYPERFRAME_COUNTER_VALUES;
        Position {
            hyperframe_counter: counter as u8,
            superframe: if offset.is_multiple_of(SUPERFRAMES_PER_HYPERFRAME) {
                Superframe::Primary
            } else {
                Superframe::Secondary
            },
        }
    }
    pub fn snapshot(
        &self,
        superframe_index: u64,
        pair: AssignedSlots,
        messages: &[MandatoryBroadcast; 5],
        changes: BroadcastChanges,
    ) -> Result<BroadcastSuperframe, Error> {
        self.snapshot_with_optional(superframe_index, pair, messages, &[], changes)
    }
    pub fn snapshot_with_optional(
        &self,
        superframe_index: u64,
        pair: AssignedSlots,
        messages: &[MandatoryBroadcast; 5],
        optional: &[Vec<u8>],
        changes: BroadcastChanges,
    ) -> Result<BroadcastSuperframe, Error> {
        let mut messages = messages.clone();
        let MandatoryBroadcast::DcchStructure(structure) = &mut messages[0] else {
            return Err(Error::Messages);
        };
        let position = self.at(superframe_index);
        structure.hyperframe_counter = position.hyperframe_counter;
        structure.secondary_superframe = position.superframe == Superframe::Secondary;
        BroadcastSuperframe::try_new_with_optional(pair, &messages, optional, changes)
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn neighbor_cycle(packet: &[u8]) -> Result<Vec<[bool; INFORMATION_BITS]>, Error> {
    crate::l3::neighbors::decode(packet).map_err(Error::Wire)?;
    let regulatory = ExtendedBroadcast::StandardRegulatoryConfiguration
        .encode()
        .map_err(Error::Wire)?;
    for count in 1..=u8::MAX as usize - 1 {
        match crate::link::broadcast::encode_ebcch_cycle(&[packet, &regulatory], count) {
            Ok(mut frames) => {
                let slots = (count + 1).min(MAX_EBCCH_SLOTS);
                let total = (count + 1).div_ceil(slots) * slots;
                while frames.len() < total {
                    frames.push(encode_ebcch_begin(&[], total as u8).map_err(Error::Framing)?);
                }
                for frame in &mut frames {
                    if !frame[0] {
                        for bit in 0..u8::BITS as usize {
                            frame[1 + bit] = total & (1 << (u8::BITS as usize - 1 - bit)) != 0;
                        }
                    }
                }
                return Ok(frames);
            }
            Err(BroadcastError::Capacity) => (),
            Err(error) => return Err(Error::Framing(error)),
        }
    }
    Err(Error::Arrangement)
}
