use super::StateStore;
use crate::{
    link::broadcast::{BroadcastChanges, BroadcastSignaling},
    phy::channel::{self, Band, ChannelError},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{error::Error as StdError, fmt};
const OWNER_OCTETS: usize = 16;
const MAX_CONTENT_OCTETS: usize = u16::MAX as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellKey {
    band: Band,
    channel: u16,
}
impl CellKey {
    pub fn try_new(band: Band, channel: u16) -> Result<Self, ChannelError> {
        channel::frequencies(band, channel)?;
        Ok(Self { band, channel })
    }
    fn band_code(self) -> u8 {
        match self.band {
            Band::Cellular800 => 0,
            Band::Pcs1900 => 1,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Revision {
    id: i64,
    owner: [u8; OWNER_OCTETS],
    cell: CellKey,
    contents: Vec<u8>,
    signaling: BroadcastSignaling,
    prepared_at_unix_ms: i64,
    confirmed_at_unix_ms: Option<i64>,
}
impl Revision {
    pub fn same_revision(&self, other: &Self) -> bool {
        self.id == other.id && self.owner == other.owner && self.cell == other.cell
    }
    /// Contents are opaque and must be decoded and validated before radio use.
    pub fn contents(&self) -> &[u8] {
        &self.contents
    }
    pub fn signaling(&self) -> BroadcastSignaling {
        self.signaling
    }
    pub fn prepared_at_unix_ms(&self) -> i64 {
        self.prepared_at_unix_ms
    }
    /// A historical host confirmation does not establish the radio state after restart.
    pub fn confirmed_at_unix_ms(&self) -> Option<i64> {
        self.confirmed_at_unix_ms
    }
}
#[derive(Debug, Default)]
pub struct Recovery {
    pub confirmed: Option<Revision>,
    pub pending: Option<Revision>,
}
#[derive(Debug)]
pub enum Error {
    Sql(rusqlite::Error),
    Contents,
    Clock,
    Conflict,
    UnknownOwner,
    Corrupt,
}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "broadcast journal failed: {self:?}")
    }
}
impl StdError for Error {}

impl StateStore {
    pub fn broadcast_predecessor(&self, revision: &Revision) -> Result<Option<Revision>, Error> {
        let owned: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM broadcast_revisions WHERE id=?1 AND owner=?2 AND band=?3 AND channel=?4)",
            params![revision.id,revision.owner.as_slice(),revision.cell.band_code(),revision.cell.channel],
            |row|row.get(0),
        )?;
        if !owned {
            return Err(Error::UnknownOwner);
        }
        self.connection.query_row(
            "SELECT id,owner,contents,fbcch,ebcch,notification,prepared_at_unix_ms,confirmed_at_unix_ms FROM broadcast_revisions WHERE band=?1 AND channel=?2 AND id<?3 AND confirmed_at_unix_ms IS NOT NULL ORDER BY id DESC LIMIT 1",
            params![revision.cell.band_code(),revision.cell.channel,revision.id],
            |row| read_row(row,revision.cell),
        ).optional()?.transpose()
    }
    pub fn broadcast_recovery(&self, cell: CellKey) -> Result<Recovery, Error> {
        recovery(&self.connection, cell)
    }
    pub fn prepare_broadcast_revision(
        &mut self,
        cell: CellKey,
        contents: &[u8],
        signaling: BroadcastSignaling,
        now_unix_ms: i64,
    ) -> Result<Revision, Error> {
        if contents.is_empty() || contents.len() > MAX_CONTENT_OCTETS {
            return Err(Error::Contents);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_clock(&transaction, now_unix_ms)?;
        let state = recovery(&transaction, cell)?;
        if let Some(pending) = state.pending {
            if pending.contents != contents || pending.signaling != signaling {
                return Err(Error::Conflict);
            }
            return Ok(pending);
        }
        if let Some(confirmed) = state.confirmed
            && confirmed.contents == contents
            && confirmed.signaling == signaling
        {
            return Ok(confirmed);
        }
        transaction.execute("INSERT INTO broadcast_revisions(band,channel,contents,fbcch,ebcch,notification,prepared_at_unix_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![cell.band_code(),cell.channel,contents,signaling.changes.fbcch,signaling.changes.ebcch,signaling.notification,now_unix_ms])?;
        let pending = recovery(&transaction, cell)?
            .pending
            .ok_or(Error::Corrupt)?;
        transaction.commit()?;
        Ok(pending)
    }
    /// Confirm from host completion of every broadcast slot in this revision. Queue drainage or stopped TX alone is insufficient.
    pub fn confirm_broadcast_revision(
        &mut self,
        revision: &Revision,
        now_unix_ms: i64,
    ) -> Result<Revision, Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_clock(&transaction, now_unix_ms)?;
        let stored = transaction.query_row("SELECT id,owner,contents,fbcch,ebcch,notification,prepared_at_unix_ms,confirmed_at_unix_ms FROM broadcast_revisions WHERE id=?1 AND owner=?2 AND band=?3 AND channel=?4", params![revision.id,revision.owner.as_slice(),revision.cell.band_code(),revision.cell.channel], |row| read_row(row, revision.cell)).optional()?.ok_or(Error::UnknownOwner)??;
        if stored.contents != revision.contents
            || stored.signaling != revision.signaling
            || stored.prepared_at_unix_ms != revision.prepared_at_unix_ms
        {
            return Err(Error::Corrupt);
        }
        if stored.confirmed_at_unix_ms.is_some() {
            return Ok(stored);
        }
        let state = recovery(&transaction, revision.cell)?;
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| pending.id != revision.id || pending.owner != revision.owner)
        {
            return Err(Error::Corrupt);
        }
        let mut confirmed = stored;
        let changed = transaction.execute("UPDATE broadcast_revisions SET confirmed_at_unix_ms=?1 WHERE id=?2 AND owner=?3 AND confirmed_at_unix_ms IS NULL", params![now_unix_ms,revision.id,revision.owner.as_slice()])?;
        if changed != 1 {
            return Err(Error::Corrupt);
        }
        confirmed.confirmed_at_unix_ms = Some(now_unix_ms);
        transaction.commit()?;
        Ok(confirmed)
    }
}
fn check_clock(connection: &Connection, now_unix_ms: i64) -> Result<(), Error> {
    let latest: Option<i64> = connection.query_row(
        "SELECT max(coalesce(confirmed_at_unix_ms,prepared_at_unix_ms)) FROM broadcast_revisions",
        [],
        |row| row.get(0),
    )?;
    if now_unix_ms < 0 || latest.is_some_and(|latest| latest > now_unix_ms) {
        return Err(Error::Clock);
    }
    Ok(())
}
fn recovery(connection: &Connection, cell: CellKey) -> Result<Recovery, Error> {
    let mut statement = connection.prepare("SELECT id,owner,contents,fbcch,ebcch,notification,prepared_at_unix_ms,confirmed_at_unix_ms FROM broadcast_revisions WHERE band=?1 AND channel=?2 AND (confirmed_at_unix_ms IS NULL OR id=(SELECT max(id) FROM broadcast_revisions WHERE band=?1 AND channel=?2 AND confirmed_at_unix_ms IS NOT NULL)) ORDER BY id")?;
    let mut state = Recovery::default();
    for row in statement.query_map(params![cell.band_code(), cell.channel], |row| {
        read_row(row, cell)
    })? {
        let revision = row??;
        let target = if revision.confirmed_at_unix_ms.is_some() {
            &mut state.confirmed
        } else {
            &mut state.pending
        };
        if target.replace(revision).is_some() {
            return Err(Error::Corrupt);
        }
    }
    if let (Some(confirmed), Some(pending)) = (&state.confirmed, &state.pending)
        && confirmed.id >= pending.id
    {
        return Err(Error::Corrupt);
    }
    Ok(state)
}
fn read_row(row: &rusqlite::Row<'_>, cell: CellKey) -> rusqlite::Result<Result<Revision, Error>> {
    let id: i64 = row.get(0)?;
    let owner: Vec<u8> = row.get(1)?;
    let contents: Vec<u8> = row.get(2)?;
    let fbcch: i64 = row.get(3)?;
    let ebcch: i64 = row.get(4)?;
    let notification: i64 = row.get(5)?;
    let prepared: i64 = row.get(6)?;
    let confirmed: Option<i64> = row.get(7)?;
    if id <= 0
        || owner.len() != OWNER_OCTETS
        || contents.is_empty()
        || contents.len() > MAX_CONTENT_OCTETS
        || !(0..=1).contains(&fbcch)
        || !(0..=1).contains(&ebcch)
        || !(0..=1).contains(&notification)
        || prepared < 0
        || confirmed.is_some_and(|confirmed| confirmed < prepared)
    {
        return Ok(Err(Error::Corrupt));
    }
    let owner = match owner.try_into() {
        Ok(owner) => owner,
        Err(_) => return Ok(Err(Error::Corrupt)),
    };
    Ok(Ok(Revision {
        id,
        owner,
        cell,
        contents,
        signaling: BroadcastSignaling {
            changes: BroadcastChanges {
                fbcch: fbcch != 0,
                ebcch: ebcch != 0,
            },
            notification: notification != 0,
        },
        prepared_at_unix_ms: prepared,
        confirmed_at_unix_ms: confirmed,
    }))
}

#[cfg(test)]
mod tests;
