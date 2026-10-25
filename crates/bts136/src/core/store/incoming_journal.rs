mod responses;
use super::{SmsId, StateStore, StoreError};
use crate::{
    l3::{
        broadcast::ProtocolVersion,
        relay::{HigherLayerProtocol, MAX_DATA_PAYLOAD_OCTETS},
        tsar::{self, Segment},
    },
    link::rach::MobileIdentity,
};
pub(crate) use responses::ResponseClosure;
pub use responses::{IncomingResponse, ResponseHandle, ResponseOutcome};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

const OWNER_OCTETS: usize = 16;
const TRANSACTION_COUNT: u8 = 8;
const SEGMENTED_CARRIER_MASK: u8 = 0xc0;
const SEGMENT: i64 = 0;
const ADVANCE: i64 = 1;
const COMMITTED: i64 = 2;
const REJECTED: i64 = 3;
const CANCELED: i64 = 4;
const APPLICATION_UNSETTLED: i64 = 0;
const APPLICATION_COMMITTED: i64 = 1;
const APPLICATION_REJECTED: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalError {
    Binding,
    Limits,
    StaleHandle,
    ContextConflict,
    Guarded,
    Closed,
    RevisionConflict,
    Capacity,
    ClockRegression,
    GuardDuration,
    ApplicationConflict,
    Segment,
    ResponsePending,
}
fn failure(error: JournalError) -> StoreError {
    StoreError::IncomingJournal(error)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalLimits {
    pub maximum_events: u32,
    pub maximum_context_octets: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingBinding {
    pub peer: MobileIdentity,
    pub protocol: HigherLayerProtocol,
    pub transaction_id: u8,
    pub version: ProtocolVersion,
    pub context: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalHandle {
    id: i64,
    owner: [u8; OWNER_OCTETS],
}
impl JournalHandle {
    pub fn id(&self) -> i64 {
        self.id
    }
    pub fn submission_key(&self) -> String {
        let mut key = format!("tsar-incoming:{}:", self.id);
        use std::fmt::Write;
        for octet in self.owner {
            write!(key, "{octet:02x}").expect("writing into String is infallible");
        }
        key
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalEvent {
    Segment(Segment),
    Advance,
    ApplicationCommitted(SmsId),
    ApplicationRejected,
    Cancel,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedEvent {
    pub ordinal: u32,
    pub observed_at_unix_ms: i64,
    pub event: JournalEvent,
}
#[derive(Debug, Clone)]
pub struct IncomingJournal {
    pub handle: JournalHandle,
    pub binding: IncomingBinding,
    pub limits: JournalLimits,
    pub created_at_unix_ms: i64,
    pub observed_at_unix_ms: i64,
    pub revision: u32,
    pub reuse_after_unix_ms: Option<i64>,
}
#[derive(Debug, Clone)]
pub struct JournalSnapshot {
    pub journal: IncomingJournal,
    pub events: Vec<ObservedEvent>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendReceipt {
    pub ordinal: u32,
    pub revision: u32,
    pub already_committed: bool,
}
struct StoredEvent {
    kind: i64,
    segment: Vec<u8>,
    sms_id: Option<i64>,
}
impl IncomingBinding {
    fn validate(&self, limits: JournalLimits) -> Result<u8, StoreError> {
        let (_, width, value) = self.peer.parts();
        if value == 0
            || value >= 1 << width
            || self.transaction_id >= TRANSACTION_COUNT
            || self.version.code() < ProtocolVersion::Pv1.code()
        {
            return Err(failure(JournalError::Binding));
        }
        if !matches!(self.protocol, HigherLayerProtocol::Segmented(_))
            && !matches!(self.protocol, HigherLayerProtocol::CarrierSpecific(raw) if raw & SEGMENTED_CARRIER_MASK == SEGMENTED_CARRIER_MASK)
        {
            return Err(failure(JournalError::Binding));
        }
        if limits.maximum_events == 0
            || limits.maximum_context_octets == 0
            || self.context.len() > limits.maximum_context_octets as usize
        {
            return Err(failure(JournalError::Limits));
        }
        self.protocol
            .code()
            .map_err(|_| failure(JournalError::Binding))
    }
}
fn encode_event(
    binding: &IncomingBinding,
    event: &JournalEvent,
) -> Result<StoredEvent, StoreError> {
    Ok(match event {
        JournalEvent::Segment(segment) => {
            if segment.transaction_id != binding.transaction_id
                || segment.segment_data.len() > MAX_DATA_PAYLOAD_OCTETS
            {
                return Err(failure(JournalError::Segment));
            }
            let bytes = segment
                .encode(binding.version)
                .map_err(|_| failure(JournalError::Segment))?;
            if bytes.len() > MAX_DATA_PAYLOAD_OCTETS {
                return Err(failure(JournalError::Segment));
            }
            StoredEvent {
                kind: SEGMENT,
                segment: bytes,
                sms_id: None,
            }
        }
        JournalEvent::Advance => StoredEvent {
            kind: ADVANCE,
            segment: Vec::new(),
            sms_id: None,
        },
        JournalEvent::ApplicationCommitted(id) => StoredEvent {
            kind: COMMITTED,
            segment: Vec::new(),
            sms_id: Some(id.value()),
        },
        JournalEvent::ApplicationRejected => StoredEvent {
            kind: REJECTED,
            segment: Vec::new(),
            sms_id: None,
        },
        JournalEvent::Cancel => StoredEvent {
            kind: CANCELED,
            segment: Vec::new(),
            sms_id: None,
        },
    })
}
fn journal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(IncomingJournal, i64, Option<i64>)> {
    let peer_type: u8 = row.get(2)?;
    let signed_value: i64 = row.get(3)?;
    let corrupt = || rusqlite::Error::InvalidQuery;
    let value = u64::try_from(signed_value).map_err(|_| corrupt())?;
    let peer = match peer_type {
        0 => MobileIdentity::Tmsi20(u32::try_from(value).map_err(|_| corrupt())?),
        1 => MobileIdentity::Tmsi24(u32::try_from(value).map_err(|_| corrupt())?),
        2 => MobileIdentity::Min34(value),
        3 => MobileIdentity::Imsi50(value),
        _ => return Err(corrupt()),
    };
    let owner: Vec<u8> = row.get(1)?;
    let journal = IncomingJournal {
        handle: JournalHandle {
            id: row.get(0)?,
            owner: owner.try_into().map_err(|_| corrupt())?,
        },
        binding: IncomingBinding {
            peer,
            protocol: HigherLayerProtocol::decode(row.get(4)?).map_err(|_| corrupt())?,
            transaction_id: row.get(5)?,
            version: ProtocolVersion::decode(row.get(6)?).map_err(|_| corrupt())?,
            context: row.get(7)?,
        },
        limits: JournalLimits {
            maximum_events: row.get(8)?,
            maximum_context_octets: row.get(9)?,
        },
        created_at_unix_ms: row.get(10)?,
        observed_at_unix_ms: row.get(11)?,
        revision: row.get(12)?,
        reuse_after_unix_ms: row.get(15)?,
    };
    journal
        .binding
        .validate(journal.limits)
        .map_err(|_| corrupt())?;
    Ok((journal, row.get(13)?, row.get(14)?))
}
const COLUMNS: &str = "id,owner,peer_type,peer_value,hlpi,tsar_id,protocol_version,context,maximum_events,maximum_context_octets,created_at_unix_ms,observed_at_unix_ms,event_count,application_state,application_sms_id,reuse_after_unix_ms";
fn application_matches(
    connection: &rusqlite::Connection,
    handle: &JournalHandle,
    peer: MobileIdentity,
    id: i64,
) -> Result<bool, StoreError> {
    let (kind, _, value) = peer.parts();
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM sms_queue WHERE id=?1 AND dedup_key=?2 AND sender_type=?3 AND sender_value=?4)", params![id,super::submission_dedup_key(&handle.submission_key()),kind,value as i64], |row| row.get(0))?)
}
pub(super) fn read_handle(
    connection: &rusqlite::Connection,
    handle: &JournalHandle,
) -> Result<(IncomingJournal, i64, Option<i64>), StoreError> {
    connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM tsar_incoming WHERE id=?1 AND owner=?2"),
            params![handle.id, handle.owner.as_slice()],
            journal_row,
        )
        .optional()?
        .ok_or(failure(JournalError::StaleHandle))
}
impl StateStore {
    pub fn begin_incoming_journal(
        &mut self,
        binding: IncomingBinding,
        limits: JournalLimits,
        now_unix_ms: i64,
    ) -> Result<IncomingJournal, StoreError> {
        if now_unix_ms < 0 {
            return Err(StoreError::Timestamp);
        }
        let hlpi = binding.validate(limits)?;
        let (kind, _, value) = binding.peer.parts();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest = transaction.query_row(&format!("SELECT {COLUMNS} FROM tsar_incoming WHERE peer_type=?1 AND peer_value=?2 AND hlpi=?3 AND tsar_id=?4 ORDER BY id DESC LIMIT 1"), params![kind,value as i64,hlpi,binding.transaction_id], journal_row).optional()?;
        if let Some((journal, _, _)) = latest {
            if now_unix_ms < journal.observed_at_unix_ms {
                return Err(failure(JournalError::ClockRegression));
            }
            if let Some(until) = journal.reuse_after_unix_ms {
                if now_unix_ms < until {
                    return Err(failure(JournalError::Guarded));
                }
            } else {
                if journal.binding != binding || journal.limits != limits {
                    return Err(failure(JournalError::ContextConflict));
                }
                transaction.commit()?;
                return Ok(journal);
            }
        }
        transaction.execute("INSERT INTO tsar_incoming(peer_type,peer_value,hlpi,tsar_id,protocol_version,context,maximum_events,maximum_context_octets,created_at_unix_ms,observed_at_unix_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)", params![kind,value as i64,hlpi,binding.transaction_id,binding.version.code(),binding.context,limits.maximum_events,limits.maximum_context_octets,now_unix_ms])?;
        let id = transaction.last_insert_rowid();
        let (journal, _, _) = transaction.query_row(
            &format!("SELECT {COLUMNS} FROM tsar_incoming WHERE id=?1"),
            [id],
            journal_row,
        )?;
        transaction.commit()?;
        Ok(journal)
    }
    pub fn incoming_journal(&self, handle: &JournalHandle) -> Result<IncomingJournal, StoreError> {
        Ok(read_handle(&self.connection, handle)?.0)
    }
    pub fn active_incoming_journals(
        &self,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<IncomingJournal>, StoreError> {
        if after_id < 0 || limit == 0 {
            return Err(failure(JournalError::Limits));
        }
        let mut statement = self.connection.prepare(&format!("SELECT {COLUMNS} FROM tsar_incoming WHERE reuse_after_unix_ms IS NULL AND id>?1 ORDER BY id LIMIT ?2"))?;
        let rows = statement.query_map(params![after_id, limit], journal_row)?;
        rows.map(|row| row.map(|(journal, _, _)| journal).map_err(StoreError::from))
            .collect()
    }
    pub(crate) fn incoming_bank_journals(
        &self,
        peer: MobileIdentity,
        protocol: HigherLayerProtocol,
    ) -> Result<Vec<IncomingJournal>, StoreError> {
        let (kind, _, value) = peer.parts();
        let hlpi = protocol
            .code()
            .map_err(|_| failure(JournalError::Binding))?;
        let mut statement = self.connection.prepare(&format!("SELECT {COLUMNS} FROM tsar_incoming WHERE reuse_after_unix_ms IS NULL AND peer_type=?1 AND peer_value=?2 AND hlpi=?3 ORDER BY id LIMIT ?4"))?;
        let rows = statement.query_map(
            params![kind, value as i64, hlpi, i64::from(TRANSACTION_COUNT) + 1],
            journal_row,
        )?;
        rows.map(|row| row.map(|(journal, _, _)| journal).map_err(StoreError::from))
            .collect()
    }
    pub fn append_incoming_event(
        &mut self,
        handle: &JournalHandle,
        expected_revision: u32,
        event: &JournalEvent,
        observed_at_unix_ms: i64,
    ) -> Result<AppendReceipt, StoreError> {
        self.append_incoming_transition(handle, expected_revision, event, observed_at_unix_ms, None)
    }
    pub fn append_incoming_transition(
        &mut self,
        handle: &JournalHandle,
        expected_revision: u32,
        event: &JournalEvent,
        observed_at_unix_ms: i64,
        response: Option<&tsar::Response>,
    ) -> Result<AppendReceipt, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (journal, application_state, application_sms_id) = read_handle(&transaction, handle)?;
        let response_bytes = responses::encode_response(&journal, event, response)?;
        if journal.reuse_after_unix_ms.is_some() {
            return Err(failure(JournalError::Closed));
        }
        let stored = encode_event(&journal.binding, event)?;
        if expected_revision < journal.revision {
            let previous = transaction.query_row("SELECT event_type,segment,sms_id,observed_at_unix_ms FROM tsar_incoming_events WHERE stream_id=?1 AND ordinal=?2", params![handle.id,expected_revision], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,Vec<u8>>(1)?,row.get::<_,Option<i64>>(2)?,row.get::<_,i64>(3)?))).optional()?;
            if previous
                .as_ref()
                .is_some_and(|(kind, bytes, sms_id, observed)| {
                    *kind == stored.kind
                        && *bytes == stored.segment
                        && *sms_id == stored.sms_id
                        && *observed == observed_at_unix_ms
                })
            {
                let previous_response = transaction.query_row("SELECT response FROM tsar_incoming_responses WHERE stream_id=?1 AND event_ordinal=?2", params![handle.id,expected_revision], |row| row.get::<_,Vec<u8>>(0)).optional()?;
                if previous_response != response_bytes {
                    return Err(failure(JournalError::RevisionConflict));
                }
                transaction.commit()?;
                return Ok(AppendReceipt {
                    ordinal: expected_revision,
                    revision: journal.revision,
                    already_committed: true,
                });
            }
            return Err(failure(JournalError::RevisionConflict));
        }
        if expected_revision != journal.revision {
            return Err(failure(JournalError::RevisionConflict));
        }
        if observed_at_unix_ms < journal.observed_at_unix_ms {
            return Err(failure(JournalError::ClockRegression));
        }
        if journal.revision >= journal.limits.maximum_events {
            return Err(failure(JournalError::Capacity));
        }
        let next_application = match event {
            JournalEvent::ApplicationCommitted(id) => {
                if application_state == APPLICATION_REJECTED
                    || application_sms_id.is_some_and(|old| old != id.value())
                {
                    return Err(failure(JournalError::ApplicationConflict));
                }
                if !application_matches(&transaction, handle, journal.binding.peer, id.value())? {
                    return Err(failure(JournalError::ApplicationConflict));
                }
                APPLICATION_COMMITTED
            }
            JournalEvent::ApplicationRejected => {
                if application_state == APPLICATION_COMMITTED {
                    return Err(failure(JournalError::ApplicationConflict));
                }
                APPLICATION_REJECTED
            }
            _ => application_state,
        };
        let next_sms_id = if stored.kind == COMMITTED {
            stored.sms_id
        } else {
            application_sms_id
        };
        transaction.execute("INSERT INTO tsar_incoming_events(stream_id,ordinal,observed_at_unix_ms,event_type,segment,sms_id) VALUES(?1,?2,?3,?4,?5,?6)", params![handle.id,journal.revision,observed_at_unix_ms,stored.kind,stored.segment,stored.sms_id])?;
        if let Some(response_bytes_value) = response_bytes {
            transaction.execute("INSERT INTO tsar_incoming_responses(stream_id,event_ordinal,response,awaits_application) VALUES(?1,?2,?3,?4)", params![handle.id,journal.revision,response_bytes_value,response.is_some_and(|response| response.status == tsar::Status::Received)])?;
        }
        let revision = journal
            .revision
            .checked_add(1)
            .ok_or(failure(JournalError::Capacity))?;
        transaction.execute("UPDATE tsar_incoming SET event_count=?1,observed_at_unix_ms=?2,application_state=?3,application_sms_id=?4 WHERE id=?5", params![revision,observed_at_unix_ms,next_application,next_sms_id,handle.id])?;
        transaction.commit()?;
        Ok(AppendReceipt {
            ordinal: expected_revision,
            revision,
            already_committed: false,
        })
    }
    pub fn incoming_events(
        &self,
        handle: &JournalHandle,
    ) -> Result<Vec<ObservedEvent>, StoreError> {
        Ok(self.incoming_snapshot(handle)?.events)
    }
    pub fn incoming_snapshot(&self, handle: &JournalHandle) -> Result<JournalSnapshot, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let (journal, recorded_application, recorded_sms) = read_handle(&transaction, handle)?;
        let mut statement = transaction.prepare("SELECT ordinal,observed_at_unix_ms,event_type,segment,sms_id FROM tsar_incoming_events WHERE stream_id=?1 ORDER BY ordinal")?;
        let rows = statement.query_map([handle.id], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let mut events = Vec::new();
        let mut previous_time = journal.created_at_unix_ms;
        let mut application = APPLICATION_UNSETTLED;
        let mut application_sms = None;
        for row in rows {
            let (ordinal, observed, kind, bytes, sms_id) = row?;
            if ordinal as usize != events.len() || observed < previous_time {
                return Err(StoreError::CorruptState);
            }
            if kind != SEGMENT && !bytes.is_empty() {
                return Err(StoreError::CorruptState);
            }
            let event = match (kind, sms_id) {
                (SEGMENT, None) => {
                    let decoded = tsar::decode_segment(&bytes, journal.binding.version)
                        .map_err(|_| StoreError::CorruptState)?;
                    if !decoded.unparsed_octets.is_empty()
                        || decoded.segment.transaction_id != journal.binding.transaction_id
                    {
                        return Err(StoreError::CorruptState);
                    }
                    JournalEvent::Segment(decoded.segment)
                }
                (ADVANCE, None) => JournalEvent::Advance,
                (COMMITTED, Some(id)) => {
                    if application == APPLICATION_REJECTED
                        || application_sms.is_some_and(|previous| previous != id)
                        || application_sms.is_none()
                            && !application_matches(&transaction, handle, journal.binding.peer, id)?
                    {
                        return Err(StoreError::CorruptState);
                    }
                    application = APPLICATION_COMMITTED;
                    application_sms = Some(id);
                    JournalEvent::ApplicationCommitted(SmsId(id))
                }
                (REJECTED, None) => {
                    if application == APPLICATION_COMMITTED {
                        return Err(StoreError::CorruptState);
                    }
                    application = APPLICATION_REJECTED;
                    JournalEvent::ApplicationRejected
                }
                (CANCELED, None) => JournalEvent::Cancel,
                _ => return Err(StoreError::CorruptState),
            };
            previous_time = observed;
            events.push(ObservedEvent {
                ordinal,
                observed_at_unix_ms: observed,
                event,
            });
        }
        if events.len() != journal.revision as usize
            || previous_time != journal.observed_at_unix_ms
            || application != recorded_application
            || application_sms != recorded_sms
        {
            return Err(StoreError::CorruptState);
        }
        drop(statement);
        transaction.commit()?;
        Ok(JournalSnapshot { journal, events })
    }
    /// Settle application ownership and fence all old transport work before retiring the wire transaction ID.
    pub fn retire_incoming_journal_fenced(
        &mut self,
        handle: &JournalHandle,
        expected_revision: u32,
        now_unix_ms: i64,
        reuse_guard_ms: u64,
    ) -> Result<(), StoreError> {
        if reuse_guard_ms == 0 {
            return Err(failure(JournalError::GuardDuration));
        }
        let guard = i64::try_from(reuse_guard_ms).map_err(|_| StoreError::Timestamp)?;
        let until = now_unix_ms
            .checked_add(guard)
            .ok_or(StoreError::Timestamp)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (journal, _, _) = read_handle(&transaction, handle)?;
        if journal.revision != expected_revision {
            return Err(failure(JournalError::RevisionConflict));
        }
        if let Some(previous) = journal.reuse_after_unix_ms {
            let retired_at: Option<i64> = transaction.query_row(
                "SELECT retired_at_unix_ms FROM tsar_incoming WHERE id=?1",
                [handle.id],
                |row| row.get(0),
            )?;
            if previous != until || retired_at != Some(now_unix_ms) {
                return Err(failure(JournalError::Closed));
            }
            transaction.commit()?;
            return Ok(());
        }
        if now_unix_ms < journal.observed_at_unix_ms {
            return Err(failure(JournalError::ClockRegression));
        }
        let pending: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM tsar_incoming_responses WHERE stream_id=?1 AND outcome=0)",
            [handle.id],
            |row| row.get(0),
        )?;
        if pending {
            return Err(failure(JournalError::ResponsePending));
        }
        transaction.execute(
            "UPDATE tsar_incoming SET reuse_after_unix_ms=?1,retired_at_unix_ms=?3 WHERE id=?2",
            params![until, handle.id, now_unix_ms],
        )?;
        transaction.commit()?;
        Ok(())
    }
}
