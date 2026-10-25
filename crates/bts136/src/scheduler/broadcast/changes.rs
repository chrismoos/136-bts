use super::{BroadcastChanges, BroadcastClock, BroadcastSignaling, Superframe};
use std::{error::Error, fmt};

const SUPERFRAMES_PER_HYPERFRAME: u64 = 2;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastChangeError {
    Boundary,
    CounterOverflow,
}
impl fmt::Display for BroadcastChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Boundary => "broadcast change announcement must start at a primary superframe",
            Self::CounterOverflow => "broadcast change activation exceeds the superframe counter",
        })
    }
}
impl Error for BroadcastChangeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastScope {
    Fbcch,
    Ebcch,
    Both,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangePhase {
    Current,
    Announced,
    Applied,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastChangeState {
    pub phase: ChangePhase,
    pub signaling: BroadcastSignaling,
}
pub struct BroadcastChange {
    announcement_superframe: u64,
    activation_superframe: u64,
    previous: BroadcastSignaling,
    announced: BroadcastSignaling,
    applied: BroadcastSignaling,
}
impl BroadcastChange {
    /// TIA-136-121-A §4 changes BCN in the preceding hyperframe and FC/EC when the new content takes effect.
    pub fn try_new(
        clock: &BroadcastClock,
        announcement_superframe: u64,
        previous: BroadcastSignaling,
        scope: BroadcastScope,
    ) -> Result<Self, BroadcastChangeError> {
        if clock.at(announcement_superframe).superframe != Superframe::Primary {
            return Err(BroadcastChangeError::Boundary);
        }
        let activation_superframe = announcement_superframe
            .checked_add(SUPERFRAMES_PER_HYPERFRAME)
            .ok_or(BroadcastChangeError::CounterOverflow)?;
        let announced = BroadcastSignaling {
            notification: !previous.notification,
            ..previous
        };
        let applied = BroadcastSignaling {
            changes: BroadcastChanges {
                fbcch: previous.changes.fbcch
                    ^ matches!(scope, BroadcastScope::Fbcch | BroadcastScope::Both),
                ebcch: previous.changes.ebcch
                    ^ matches!(scope, BroadcastScope::Ebcch | BroadcastScope::Both),
            },
            ..announced
        };
        Ok(Self {
            announcement_superframe,
            activation_superframe,
            previous,
            announced,
            applied,
        })
    }
    pub fn activation_superframe(&self) -> u64 {
        self.activation_superframe
    }
    pub fn at(&self, superframe: u64) -> BroadcastChangeState {
        let (phase, signaling) = if superframe < self.announcement_superframe {
            (ChangePhase::Current, self.previous)
        } else if superframe < self.activation_superframe {
            (ChangePhase::Announced, self.announced)
        } else {
            (ChangePhase::Applied, self.applied)
        };
        BroadcastChangeState { phase, signaling }
    }
}

#[cfg(test)]
mod tests;
