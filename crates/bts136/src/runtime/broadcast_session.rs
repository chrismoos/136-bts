use super::{
    broadcast_completion::{BroadcastCompletion, NotificationCompletion},
    timers::SUPERFRAME_MS,
};
use crate::{
    core::store::{
        StateStore,
        broadcast::{self, CellKey, Revision},
    },
    l3::broadcast::{DcchStructure, MandatoryBroadcast},
    link::broadcast::{BroadcastChanges, BroadcastSignaling},
    scheduler::broadcast::{
        BroadcastClock, BroadcastContent, BroadcastContentStorageError, BroadcastSuperframe,
        BroadcastTransition, BroadcastTransitionError, ChangePhase, PHYSICAL_SLOTS, Superframe,
    },
};
use std::{error::Error as StdError, fmt};
const CONTROL_PERIOD: u64 = 3;
const SUPERFRAMES_PER_HYPERFRAME: u64 = 2;
// TIA-136-121-A Table 2 defines PFC periodicity in hyperframes.
const PAGING_PERIODS: [u64; 8] = [1, 2, 3, 6, 12, 24, 48, 96];

#[derive(Debug)]
pub enum Error {
    Store(broadcast::Error),
    Storage(BroadcastContentStorageError),
    Schedule(crate::scheduler::broadcast::Error),
    Transition(BroadcastTransitionError),
    Completion(super::broadcast_completion::Error),
    Clock,
    Layout,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "broadcast session failed: {self:?}")
    }
}
impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::Schedule(error) => Some(error),
            Self::Transition(error) => Some(error),
            Self::Completion(error) => Some(error),
            Self::Clock | Self::Layout => None,
        }
    }
}
impl From<broadcast::Error> for Error {
    fn from(error: broadcast::Error) -> Self {
        Self::Store(error)
    }
}
impl From<BroadcastContentStorageError> for Error {
    fn from(error: BroadcastContentStorageError) -> Self {
        Self::Storage(error)
    }
}
impl From<crate::scheduler::broadcast::Error> for Error {
    fn from(error: crate::scheduler::broadcast::Error) -> Self {
        Self::Schedule(error)
    }
}
impl From<BroadcastTransitionError> for Error {
    fn from(error: BroadcastTransitionError) -> Self {
        Self::Transition(error)
    }
}
impl From<super::broadcast_completion::Error> for Error {
    fn from(error: super::broadcast_completion::Error) -> Self {
        Self::Completion(error)
    }
}

enum Plan {
    Stable {
        revision: Revision,
        content: BroadcastContent,
    },
    Changing {
        previous: Revision,
        target: Revision,
        transition: Box<BroadcastTransition>,
    },
}
struct Active<'a> {
    revision: &'a Revision,
    content: &'a BroadcastContent,
    signaling: BroadcastSignaling,
    first_superframe: u64,
}
impl Plan {
    fn at(&self, sf: u64) -> Active<'_> {
        match self {
            Self::Stable { revision, content } => Active {
                revision,
                content,
                signaling: revision.signaling(),
                first_superframe: 0,
            },
            Self::Changing {
                previous,
                target,
                transition,
            } => {
                let state = transition.state(sf);
                let first_superframe = match state.phase {
                    ChangePhase::Current => 0,
                    ChangePhase::Announced => {
                        transition.activation_superframe() - SUPERFRAMES_PER_HYPERFRAME
                    }
                    ChangePhase::Applied => transition.activation_superframe(),
                };
                Active {
                    revision: if state.phase == ChangePhase::Applied {
                        target
                    } else {
                        previous
                    },
                    content: transition.content(sf),
                    signaling: state.signaling,
                    first_superframe,
                }
            }
        }
    }
}
struct Evidence {
    revision: Revision,
    signaling: BroadcastSignaling,
    first_superframe: u64,
    data: BroadcastCompletion,
    notification: NotificationCompletion,
    last_data: Option<u64>,
    cycle_start: Option<u64>,
    cycle_superframes: u64,
    data_reported: bool,
    notification_reported: bool,
}
#[derive(Default)]
pub(crate) struct Events {
    pub data_cycle: bool,
    pub notification_hyperframe: bool,
    pub confirmed: bool,
}
pub(crate) struct BroadcastSession {
    cell: CellKey,
    clock: BroadcastClock,
    desired: BroadcastContent,
    desired_bytes: Vec<u8>,
    plan: Plan,
    evidence: Option<Evidence>,
    blocked_through: Option<u64>,
    next_change_ms: u64,
    cadence_ms: u64,
    epoch_ms: u64,
    version: u64,
    serving_allowed: bool,
    last_link_ms: u64,
    last_unix_ms: i64,
}
impl BroadcastSession {
    pub fn try_new(
        store: &mut StateStore,
        cell: CellKey,
        clock: BroadcastClock,
        desired: BroadcastContent,
        now_unix_ms: i64,
        epoch_ms: u64,
    ) -> Result<Self, Error> {
        let desired_bytes = desired.storage_bytes()?;
        let state = store.broadcast_recovery(cell)?;
        let revision = if let Some(revision) = state.pending.or(state.confirmed) {
            store.prepare_broadcast_revision(
                cell,
                revision.contents(),
                revision.signaling(),
                now_unix_ms,
            )?
        } else {
            store.prepare_broadcast_revision(
                cell,
                &desired_bytes,
                BroadcastSignaling {
                    changes: BroadcastChanges {
                        fbcch: false,
                        ebcch: false,
                    },
                    notification: false,
                },
                now_unix_ms,
            )?
        };
        let content = BroadcastContent::from_storage_bytes(revision.contents())?;
        let mut cadence_ms = period(&content)?.max(period(&desired)?);
        if let Some(previous) = store.broadcast_predecessor(&revision)? {
            cadence_ms = cadence_ms.max(period(&BroadcastContent::from_storage_bytes(
                previous.contents(),
            )?)?);
        }
        let next_change_ms = if let Some(confirmed) = revision.confirmed_at_unix_ms() {
            let eligible = confirmed
                .checked_add(i64::try_from(cadence_ms).map_err(|_| Error::Clock)?)
                .ok_or(Error::Clock)?;
            u64::try_from(eligible.saturating_sub(now_unix_ms).max(0)).map_err(|_| Error::Clock)?
        } else {
            0
        };
        let serving_allowed = revision.contents() == desired_bytes;
        let mut session = Self {
            cell,
            clock,
            desired,
            desired_bytes,
            plan: Plan::Stable { revision, content },
            evidence: None,
            blocked_through: None,
            next_change_ms,
            cadence_ms,
            epoch_ms,
            version: 0,
            serving_allowed,
            last_link_ms: 0,
            last_unix_ms: now_unix_ms,
        };
        session.advance(store, 0, 0, now_unix_ms)?;
        Ok(session)
    }
    pub fn set_time_zone(
        &mut self,
        offset: crate::l3::time_date::TimeZoneOffset,
    ) -> Result<bool, Error> {
        if self.desired.time_zone() == Some(offset) {
            return Ok(false);
        }
        self.desired =
            BroadcastContent::from_storage_bytes(&self.desired_bytes)?.with_time_zone(offset)?;
        self.desired_bytes = self.desired.storage_bytes()?;
        self.serving_allowed = false;
        Ok(true)
    }
    pub fn desired(&self) -> &BroadcastContent {
        &self.desired
    }
    pub fn content(&self, sf: u64) -> &BroadcastContent {
        self.plan.at(sf).content
    }
    pub fn revision(&self, sf: u64) -> &Revision {
        self.plan.at(sf).revision
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn serving_allowed(&self) -> bool {
        self.serving_allowed
    }
    pub fn snapshot(&self, sf: u64) -> Result<BroadcastSuperframe, Error> {
        let active = self.plan.at(sf);
        let mut snapshot = self.clock.snapshot_with_signaling(
            sf,
            active.content.pair(),
            active.content.messages(),
            active.content.optional_messages(),
            active.signaling,
        )?;
        if let Some(packet) = active.content.neighbor_packet() {
            snapshot.set_neighbors(packet, sf)?;
        }
        Ok(snapshot)
    }
    pub fn invalidate_through(&mut self, sf: u64) {
        self.blocked_through = Some(self.blocked_through.map_or(sf, |old| old.max(sf)));
        if let Some(evidence) = self.evidence.as_mut() {
            evidence.data.invalidate_through(sf);
            evidence.notification.invalidate_through(sf);
            evidence.last_data = None;
            evidence.cycle_start = None;
            evidence.data_reported = false;
            evidence.notification_reported = false;
        }
    }
    pub fn advance(
        &mut self,
        store: &mut StateStore,
        next_slot: u64,
        now_ms: u64,
        now_unix_ms: i64,
    ) -> Result<bool, Error> {
        self.observe_clock(now_ms, now_unix_ms)?;
        let Plan::Stable { revision, content } = &self.plan else {
            return Ok(false);
        };
        if revision.contents() == self.desired_bytes
            || revision.confirmed_at_unix_ms().is_none()
            || now_ms < self.next_change_ms
        {
            return Ok(false);
        }
        let earliest = self
            .next_change_ms
            .saturating_sub(self.epoch_ms)
            .div_ceil(SUPERFRAME_MS);
        let mut announcement = next_slot.div_ceil(PHYSICAL_SLOTS).max(earliest);
        if self.clock.at(announcement).superframe != Superframe::Primary {
            announcement = announcement.checked_add(1).ok_or(Error::Clock)?;
        }
        let transition = BroadcastTransition::try_new(
            self.clock,
            announcement,
            revision.signaling(),
            BroadcastContent::from_storage_bytes(&content.storage_bytes()?)?,
            BroadcastContent::from_storage_bytes(&self.desired_bytes)?,
        )?;
        let signaling = transition
            .state(transition.activation_superframe())
            .signaling;
        let target = store.prepare_broadcast_revision(
            self.cell,
            &self.desired_bytes,
            signaling,
            now_unix_ms,
        )?;
        self.plan = Plan::Changing {
            previous: revision.clone(),
            target,
            transition: Box::new(transition),
        };
        self.version = self.version.checked_add(1).ok_or(Error::Clock)?;
        Ok(true)
    }
    pub fn completed(
        &mut self,
        store: &mut StateStore,
        slot: u64,
        now_ms: u64,
        now_unix_ms: i64,
    ) -> Result<Events, Error> {
        self.observe_clock(now_ms, now_unix_ms)?;
        let mut events = Events::default();
        if !slot.is_multiple_of(CONTROL_PERIOD) {
            return Ok(events);
        }
        let sf = slot / PHYSICAL_SLOTS;
        let phase = ((slot % PHYSICAL_SLOTS) / CONTROL_PERIOD) as u8;
        if self
            .evidence
            .as_ref()
            .is_some_and(|evidence| sf < evidence.first_superframe)
        {
            return Ok(events);
        }
        let active = self.plan.at(sf);
        if self.evidence.as_ref().is_none_or(|evidence| {
            !evidence.revision.same_revision(active.revision)
                || evidence.signaling != active.signaling
        }) {
            let structure = structure(active.content)?;
            let mut evidence = Evidence {
                revision: active.revision.clone(),
                signaling: active.signaling,
                first_superframe: active.first_superframe,
                data: BroadcastCompletion::try_new(structure.fbcch_slots, structure.ebcch_slots)?,
                notification: NotificationCompletion::try_new(
                    structure.fbcch_slots,
                    structure.ebcch_slots,
                    self.clock,
                )?,
                last_data: None,
                cycle_start: None,
                cycle_superframes: active.content.cycle_superframes()?,
                data_reported: false,
                notification_reported: false,
            };
            let boundary = active
                .first_superframe
                .checked_sub(1)
                .into_iter()
                .chain(self.blocked_through)
                .max();
            if let Some(boundary) = boundary {
                evidence.data.invalidate_through(boundary);
                evidence.notification.invalidate_through(boundary);
            }
            self.serving_allowed = active.revision.contents() == self.desired_bytes;
            self.evidence = Some(evidence);
        }
        let evidence = self.evidence.as_mut().ok_or(Error::Layout)?;
        if evidence.data.completed(sf, phase) {
            if sf.is_multiple_of(evidence.cycle_superframes) {
                evidence.cycle_start = Some(sf);
            }
            if evidence
                .cycle_start
                .is_some_and(|start| sf == start + evidence.cycle_superframes - 1)
                && (evidence.cycle_superframes == 1 || evidence.last_data == sf.checked_sub(1))
            {
                events.data_cycle = !evidence.data_reported;
                evidence.data_reported = true;
            }
            evidence.last_data = Some(sf);
        }
        if evidence.notification.completed(sf, phase) {
            events.notification_hyperframe = !evidence.notification_reported;
            evidence.notification_reported = true;
            if evidence.data_reported
                && evidence.last_data == Some(sf)
                && evidence.revision.confirmed_at_unix_ms().is_none()
                && evidence.revision.signaling() == evidence.signaling
            {
                let confirmed =
                    store.confirm_broadcast_revision(&evidence.revision, now_unix_ms)?;
                self.plan = Plan::Stable {
                    content: BroadcastContent::from_storage_bytes(confirmed.contents())?,
                    revision: confirmed.clone(),
                };
                evidence.revision = confirmed;
                self.next_change_ms = now_ms.checked_add(self.cadence_ms).ok_or(Error::Clock)?;
                self.version = self.version.checked_add(1).ok_or(Error::Clock)?;
                events.confirmed = true;
            }
        }
        Ok(events)
    }
    fn observe_clock(&mut self, link_ms: u64, unix_ms: i64) -> Result<(), Error> {
        if link_ms < self.last_link_ms || unix_ms < self.last_unix_ms {
            return Err(Error::Clock);
        }
        self.last_link_ms = link_ms;
        self.last_unix_ms = unix_ms;
        Ok(())
    }
}
fn structure(content: &BroadcastContent) -> Result<&DcchStructure, Error> {
    match &content.messages()[0] {
        MandatoryBroadcast::DcchStructure(value) => Ok(value),
        _ => Err(Error::Layout),
    }
}
fn period(content: &BroadcastContent) -> Result<u64, Error> {
    let class = structure(content)?.max_supported_pfc;
    period_class(class)
}
fn period_class(class: u8) -> Result<u64, Error> {
    let index = class.checked_sub(1).ok_or(Error::Layout)?;
    PAGING_PERIODS
        .get(usize::from(index))
        .and_then(|period| period.checked_mul(SUPERFRAMES_PER_HYPERFRAME * SUPERFRAME_MS))
        .ok_or(Error::Layout)
}

#[cfg(test)]
mod tests;
