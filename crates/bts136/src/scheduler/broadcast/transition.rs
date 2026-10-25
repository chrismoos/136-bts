use super::{
    AssignedSlots, BroadcastChange, BroadcastChangeError, BroadcastChangeState, BroadcastChanges,
    BroadcastClock, BroadcastScope, BroadcastSignaling, BroadcastSuperframe, ChangePhase, Error,
    MandatoryBroadcast,
};
use std::{error::Error as StdError, fmt};
mod storage;
pub use storage::BroadcastContentStorageError;

pub struct BroadcastContent {
    pair: AssignedSlots,
    messages: [MandatoryBroadcast; 5],
    optional: Vec<Vec<u8>>,
    time_zone: Option<crate::l3::time_date::TimeZoneOffset>,
    neighbor_packet: Option<Vec<u8>>,
}
impl BroadcastContent {
    pub fn try_new(
        pair: AssignedSlots,
        messages: [MandatoryBroadcast; 5],
        optional: Vec<Vec<u8>>,
    ) -> Result<Self, Error> {
        let mut messages = BroadcastSuperframe::fit_optional(
            pair,
            &messages,
            &optional,
            BroadcastChanges {
                fbcch: false,
                ebcch: false,
            },
        )?;
        let MandatoryBroadcast::DcchStructure(structure) = &mut messages[0] else {
            return Err(Error::Messages);
        };
        // TIA-136-121-A §4 changes HFC and primary/secondary position independently of broadcast content.
        structure.hyperframe_counter = 0;
        structure.secondary_superframe = false;
        Ok(Self {
            pair,
            messages,
            optional,
            time_zone: None,
            neighbor_packet: None,
        })
    }
    pub fn with_time_zone(
        mut self,
        time_zone: crate::l3::time_date::TimeZoneOffset,
    ) -> Result<Self, Error> {
        let MandatoryBroadcast::DcchStructure(structure) = &mut self.messages[0] else {
            return Err(Error::Messages);
        };
        structure.ebcch_slots = self.neighbor_packet.as_ref().map_or(Ok(2), |packet| {
            super::neighbor_cycle(packet)
                .map(|frames| frames.len().min(super::MAX_EBCCH_SLOTS) as u8)
        })?;
        let mut content = Self::try_new(self.pair, self.messages, self.optional)?;
        content.time_zone = Some(time_zone);
        content.neighbor_packet = self.neighbor_packet;
        Ok(content)
    }
    pub fn with_neighbors(
        mut self,
        neighbors: &[crate::l3::neighbors::Neighbor],
    ) -> Result<Self, Error> {
        let packet = crate::l3::neighbors::encode(neighbors).map_err(Error::Wire)?;
        let count = super::neighbor_cycle(&packet)?.len().min(8) as u8;
        let MandatoryBroadcast::DcchStructure(structure) = &mut self.messages[0] else {
            return Err(Error::Messages);
        };
        structure.ebcch_slots = count;
        self.neighbor_packet = Some(packet);
        if let Some(zone) = self.time_zone {
            self = self.with_time_zone(zone)?;
        }
        Ok(self)
    }
    pub(crate) fn neighbor_packet(&self) -> Option<&[u8]> {
        self.neighbor_packet.as_deref()
    }
    pub(crate) fn cycle_superframes(&self) -> Result<u64, Error> {
        self.neighbor_packet.as_ref().map_or(Ok(1), |packet| {
            super::neighbor_cycle(packet)
                .map(|frames| frames.len().div_ceil(super::MAX_EBCCH_SLOTS) as u64)
        })
    }
    pub fn time_zone(&self) -> Option<crate::l3::time_date::TimeZoneOffset> {
        self.time_zone
    }
    fn scope(&self, next: &Self) -> BroadcastScope {
        let fbcch = self.pair != next.pair
            || self.messages != next.messages
            || self.optional != next.optional;
        let ebcch = self.neighbor_packet != next.neighbor_packet
            || self.time_zone != next.time_zone
            || match (&self.messages[0], &next.messages[0]) {
                (
                    MandatoryBroadcast::DcchStructure(old),
                    MandatoryBroadcast::DcchStructure(new),
                ) => old.ebcch_slots != new.ebcch_slots,
                _ => true,
            };
        match (fbcch, ebcch) {
            (true, true) => BroadcastScope::Both,
            (false, true) => BroadcastScope::Ebcch,
            _ => BroadcastScope::Fbcch,
        }
    }
    pub fn messages(&self) -> &[MandatoryBroadcast; 5] {
        &self.messages
    }
    pub fn optional_messages(&self) -> &[Vec<u8>] {
        &self.optional
    }
    pub fn pair(&self) -> AssignedSlots {
        self.pair
    }
}
#[derive(Debug)]
pub enum BroadcastTransitionError {
    Unchanged,
    Change(BroadcastChangeError),
}
impl fmt::Display for BroadcastTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unchanged => f.write_str("broadcast transition requires changed content"),
            Self::Change(error) => error.fmt(f),
        }
    }
}
impl StdError for BroadcastTransitionError {}

pub struct BroadcastTransition {
    clock: BroadcastClock,
    change: BroadcastChange,
    previous: BroadcastContent,
    updated: BroadcastContent,
}
impl BroadcastTransition {
    pub fn try_new(
        clock: BroadcastClock,
        announcement_superframe: u64,
        signaling: BroadcastSignaling,
        previous: BroadcastContent,
        updated: BroadcastContent,
    ) -> Result<Self, BroadcastTransitionError> {
        if previous.pair == updated.pair
            && previous.messages == updated.messages
            && previous.optional == updated.optional
            && previous.time_zone == updated.time_zone
            && previous.neighbor_packet == updated.neighbor_packet
        {
            return Err(BroadcastTransitionError::Unchanged);
        }
        let change = BroadcastChange::try_new(
            &clock,
            announcement_superframe,
            signaling,
            previous.scope(&updated),
        )
        .map_err(BroadcastTransitionError::Change)?;
        Ok(Self {
            clock,
            change,
            previous,
            updated,
        })
    }
    pub fn state(&self, superframe: u64) -> BroadcastChangeState {
        self.change.at(superframe)
    }
    pub fn activation_superframe(&self) -> u64 {
        self.change.activation_superframe()
    }
    pub fn content(&self, superframe: u64) -> &BroadcastContent {
        if self.state(superframe).phase == ChangePhase::Applied {
            &self.updated
        } else {
            &self.previous
        }
    }
    pub fn snapshot(&self, superframe: u64) -> Result<BroadcastSuperframe, Error> {
        let content = self.content(superframe);
        let mut snapshot = self.clock.snapshot_with_signaling(
            superframe,
            content.pair,
            &content.messages,
            &content.optional,
            self.state(superframe).signaling,
        )?;
        if let Some(packet) = content.neighbor_packet() {
            snapshot.set_neighbors(packet, superframe)?;
        }
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests;
