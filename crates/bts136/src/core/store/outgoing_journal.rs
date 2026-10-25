use super::{SmsId, StateStore, StoreError, record_relay_accept_in};
use crate::{
    l3::{
        broadcast::ProtocolVersion,
        relay::{HigherLayerProtocol, Teleservice},
        tsar::{self, AcknowledgmentWindow, MessagePlan, Segment, SegmentedMessage},
    },
    link::{
        rach::MobileIdentity,
        tsar::{LowerLayerFailure, Sender, SenderState, Submission},
    },
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const OWNER_OCTETS: usize = 16;
const TRANSACTION_COUNT: u8 = 8;
const MAX_EVENT_OCTETS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalError {
    Binding,
    Limits,
    Capacity,
    StaleHandle,
    Conflict,
    Revision,
    Clock,
    Plan,
    Event,
    Replay,
    Closed,
    StillActive,
    GuardDuration,
}
fn failure(error: JournalError) -> StoreError {
    StoreError::OutgoingJournal(error)
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
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingRequest {
    pub sms_id: SmsId,
    pub peer: MobileIdentity,
    pub version: ProtocolVersion,
    pub payload_budget_octets: usize,
    pub acknowledgment_window: AcknowledgmentWindow,
    pub context: Vec<u8>,
    pub maximum_context_octets: u32,
    pub maximum_events: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum JournalEvent {
    Submitted {
        sequence: u8,
        queued_at_unix_ms: i64,
    },
    Accepted {
        submission_ordinal: u32,
    },
    Rejected {
        submission_ordinal: u32,
        cause: u8,
    },
    Response {
        payload: Vec<u8>,
    },
    Advance,
    Cancel,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedEvent {
    pub ordinal: u32,
    pub observed_at_unix_ms: i64,
    pub event: JournalEvent,
}
#[derive(Debug, Clone)]
pub struct JournalSnapshot {
    pub handle: JournalHandle,
    pub sms_id: SmsId,
    pub peer: MobileIdentity,
    pub version: ProtocolVersion,
    pub context: Vec<u8>,
    pub plan: SegmentedMessage,
    pub created_at_unix_ms: i64,
    pub observed_at_unix_ms: i64,
    pub maximum_events: u32,
    pub maximum_context_octets: u32,
    pub events: Vec<ObservedEvent>,
    pub retirement: Option<Retirement>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retirement {
    pub retired_at_unix_ms: i64,
    pub reuse_after_unix_ms: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetirementReceipt {
    pub retirement: Retirement,
    pub already_committed: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedFragment {
    pub submission_ordinal: u32,
    pub segment: Segment,
    pub queued_at_unix_ms: i64,
}
impl JournalSnapshot {
    pub fn revision(&self) -> u32 {
        self.events.len() as u32
    }
    /// Recover the existing Layer 3 handoff without recording a new submission or resetting SEGINT.
    pub fn submitted_fragment(&self) -> Result<Option<SubmittedFragment>, StoreError> {
        if self.replay()?.state() != SenderState::AwaitingAccept {
            return Ok(None);
        }
        for observation in self.events.iter().rev() {
            if let JournalEvent::Submitted {
                sequence,
                queued_at_unix_ms,
            } = observation.event
            {
                let segment = self
                    .plan
                    .segments()
                    .get(usize::from(sequence))
                    .ok_or_else(|| failure(JournalError::Replay))?
                    .clone();
                return Ok(Some(SubmittedFragment {
                    submission_ordinal: observation.ordinal,
                    segment,
                    queued_at_unix_ms,
                }));
            }
        }
        Err(failure(JournalError::Replay))
    }
    pub fn replay(&self) -> Result<Sender, StoreError> {
        let mut sender = Sender::try_new(
            self.peer,
            service(),
            self.plan.clone(),
            self.version,
            timestamp(self.created_at_unix_ms)?,
        )
        .map_err(|_| failure(JournalError::Replay))?;
        let mut submissions: BTreeMap<u32, Submission> = BTreeMap::new();
        let mut last = self.created_at_unix_ms;
        for (index, observation) in self.events.iter().enumerate() {
            if observation.ordinal as usize != index || observation.observed_at_unix_ms < last {
                return Err(failure(JournalError::Replay));
            }
            let now = timestamp(observation.observed_at_unix_ms)?;
            match &observation.event {
                JournalEvent::Submitted {
                    sequence,
                    queued_at_unix_ms,
                } => {
                    let submission = sender
                        .submission()
                        .filter(|submission| submission.segment().sequence_number == *sequence)
                        .cloned()
                        .ok_or_else(|| failure(JournalError::Event))?;
                    sender
                        .submitted(&submission, timestamp(*queued_at_unix_ms)?, now)
                        .map_err(|_| failure(JournalError::Event))?;
                    submissions.insert(observation.ordinal, submission);
                }
                JournalEvent::Accepted { submission_ordinal } => {
                    let submission = submissions
                        .get(submission_ordinal)
                        .ok_or_else(|| failure(JournalError::Event))?;
                    sender
                        .accepted(submission, now)
                        .map_err(|_| failure(JournalError::Event))?;
                }
                JournalEvent::Rejected {
                    submission_ordinal,
                    cause,
                } => {
                    let submission = submissions
                        .get(submission_ordinal)
                        .ok_or_else(|| failure(JournalError::Event))?;
                    sender
                        .lower_failed(
                            submission,
                            LowerLayerFailure::Rejected { cause: *cause },
                            now,
                        )
                        .map_err(|_| failure(JournalError::Event))?;
                }
                JournalEvent::Response { payload } => {
                    let decoded = tsar::decode_response(payload, self.version)
                        .map_err(|_| failure(JournalError::Event))?;
                    sender
                        .receive_response(self.peer, service(), &decoded, now)
                        .map_err(|_| failure(JournalError::Event))?;
                }
                JournalEvent::Advance => {
                    sender
                        .advance(now)
                        .map_err(|_| failure(JournalError::Event))?;
                }
                JournalEvent::Cancel => {
                    sender
                        .cancel(now)
                        .map_err(|_| failure(JournalError::Event))?;
                }
            }
            last = observation.observed_at_unix_ms;
        }
        if last != self.observed_at_unix_ms {
            return Err(failure(JournalError::Replay));
        }
        Ok(sender)
    }
}
fn closed(state: SenderState) -> bool {
    matches!(
        state,
        SenderState::Complete(_) | SenderState::Failed(_) | SenderState::Cancelled
    )
}
fn service() -> HigherLayerProtocol {
    HigherLayerProtocol::Segmented(Teleservice::Sms)
}
fn timestamp(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| failure(JournalError::Clock))
}
fn encode_event(event: &JournalEvent) -> Result<Vec<u8>, StoreError> {
    let bytes = serde_yaml_ng::to_string(event)
        .map_err(|_| failure(JournalError::Event))?
        .into_bytes();
    if bytes.len() > MAX_EVENT_OCTETS {
        return Err(failure(JournalError::Limits));
    }
    Ok(bytes)
}
fn identity(kind: u8, value: u64) -> Result<MobileIdentity, StoreError> {
    let peer = match kind {
        0 => MobileIdentity::Tmsi20(
            u32::try_from(value).map_err(|_| failure(JournalError::Binding))?,
        ),
        1 => MobileIdentity::Tmsi24(
            u32::try_from(value).map_err(|_| failure(JournalError::Binding))?,
        ),
        2 => MobileIdentity::Min34(value),
        3 => MobileIdentity::Imsi50(value),
        _ => return Err(failure(JournalError::Binding)),
    };
    let (_, width, value) = peer.parts();
    if value == 0 || value >= 1 << width {
        return Err(failure(JournalError::Binding));
    }
    Ok(peer)
}
struct StoredHeader {
    owner: Vec<u8>,
    sms_id: i64,
    peer_type: u8,
    peer_value: i64,
    version: u8,
    budget: u16,
    message: Vec<u8>,
    context: Vec<u8>,
    maximum_events: u32,
    created: i64,
    observed: i64,
    event_count: u32,
    transaction_id: u8,
    hlpi: u8,
    maximum_context_octets: u32,
    retired_at_unix_ms: Option<i64>,
    reuse_after_unix_ms: Option<i64>,
}
fn load(
    transaction: &Transaction<'_>,
    handle: &JournalHandle,
) -> Result<JournalSnapshot, StoreError> {
    let header = transaction.query_row("SELECT owner,sms_id,peer_type,peer_value,protocol_version,payload_budget,message,context,maximum_events,created_at_unix_ms,observed_at_unix_ms,event_count,tsar_id,hlpi,maximum_context_octets,retired_at_unix_ms,reuse_after_unix_ms FROM tsar_outgoing WHERE id=?1", [handle.id], |row| Ok(StoredHeader {
        owner: row.get(0)?,
        sms_id: row.get(1)?,
        peer_type: row.get(2)?,
        peer_value: row.get(3)?,
        version: row.get(4)?,
        budget: row.get(5)?,
        message: row.get(6)?,
        context: row.get(7)?,
        maximum_events: row.get(8)?,
        created: row.get(9)?,
        observed: row.get(10)?,
        event_count: row.get(11)?,
        transaction_id: row.get(12)?,
        hlpi: row.get(13)?,
        maximum_context_octets: row.get(14)?,
        retired_at_unix_ms: row.get(15)?,
        reuse_after_unix_ms: row.get(16)?,
    })).optional()?.ok_or_else(|| failure(JournalError::StaleHandle))?;
    if header.owner != handle.owner {
        return Err(failure(JournalError::StaleHandle));
    }
    let peer = identity(
        header.peer_type,
        u64::try_from(header.peer_value).map_err(|_| failure(JournalError::Binding))?,
    )?;
    let version =
        ProtocolVersion::decode(header.version).map_err(|_| failure(JournalError::Binding))?;
    if header.hlpi
        != service()
            .code()
            .map_err(|_| failure(JournalError::Binding))?
    {
        return Err(failure(JournalError::Binding));
    }
    let (queue_peer_type, queue_peer_value, message): (u8, i64, Vec<u8>) = transaction.query_row(
        "SELECT recipient_type,recipient_value,sms_pdu FROM sms_queue WHERE id=?1",
        [header.sms_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if (queue_peer_type, queue_peer_value) != (header.peer_type, header.peer_value)
        || message != header.message
    {
        return Err(failure(JournalError::Binding));
    }
    let mut statement = transaction.prepare(
        "SELECT sequence,payload FROM tsar_outgoing_segments WHERE stream_id=?1 ORDER BY sequence",
    )?;
    let mut segments = Vec::new();
    for entry in statement.query_map([handle.id], |row| {
        Ok((row.get::<_, u16>(0)?, row.get::<_, Vec<u8>>(1)?))
    })? {
        let (sequence, payload) = entry?;
        let decoded =
            tsar::decode_segment(&payload, version).map_err(|_| failure(JournalError::Plan))?;
        if usize::from(sequence) != segments.len()
            || decoded.segment.sequence_number as usize != usize::from(sequence)
            || !decoded.unparsed_octets.is_empty()
            || decoded.segment.transaction_id != header.transaction_id
            || decoded
                .segment
                .encode(version)
                .map_err(|_| failure(JournalError::Plan))?
                != payload
        {
            return Err(failure(JournalError::Plan));
        }
        segments.push(decoded.segment);
    }
    let plan =
        SegmentedMessage::try_from_frozen(segments, &message, usize::from(header.budget), version)
            .map_err(|_| failure(JournalError::Plan))?;
    let mut statement = transaction.prepare("SELECT ordinal,observed_at_unix_ms,event FROM tsar_outgoing_events WHERE stream_id=?1 ORDER BY ordinal")?;
    let mut events = Vec::new();
    for entry in statement.query_map([handle.id], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Vec<u8>>(2)?,
        ))
    })? {
        let (ordinal, observed_at_unix_ms, bytes) = entry?;
        if bytes.len() > MAX_EVENT_OCTETS {
            return Err(failure(JournalError::Limits));
        }
        let event = serde_yaml_ng::from_slice(&bytes).map_err(|_| failure(JournalError::Event))?;
        events.push(ObservedEvent {
            ordinal,
            observed_at_unix_ms,
            event,
        });
    }
    if events.len() != header.event_count as usize {
        return Err(failure(JournalError::Revision));
    }
    let retirement = match (header.retired_at_unix_ms, header.reuse_after_unix_ms) {
        (None, None) => None,
        (Some(retired_at_unix_ms), Some(reuse_after_unix_ms))
            if retired_at_unix_ms >= header.observed
                && reuse_after_unix_ms > retired_at_unix_ms =>
        {
            Some(Retirement {
                retired_at_unix_ms,
                reuse_after_unix_ms,
            })
        }
        _ => return Err(failure(JournalError::Replay)),
    };
    let snapshot = JournalSnapshot {
        handle: handle.clone(),
        sms_id: SmsId(header.sms_id),
        peer,
        version,
        context: header.context,
        plan,
        created_at_unix_ms: header.created,
        observed_at_unix_ms: header.observed,
        maximum_events: header.maximum_events,
        maximum_context_octets: header.maximum_context_octets,
        events,
        retirement,
    };
    if snapshot.context.len() > snapshot.maximum_context_octets as usize {
        return Err(failure(JournalError::Limits));
    }
    let sender = snapshot.replay()?;
    if snapshot.retirement.is_some() && !closed(sender.state()) {
        return Err(failure(JournalError::StillActive));
    }
    Ok(snapshot)
}
impl StateStore {
    pub fn begin_outgoing_journal(
        &mut self,
        request: OutgoingRequest,
        created_at_unix_ms: i64,
    ) -> Result<JournalSnapshot, StoreError> {
        timestamp(created_at_unix_ms)?;
        let (kind, _, value) = request.peer.parts();
        identity(kind, value)?;
        if request.maximum_events == 0
            || request.maximum_context_octets == 0
            || request.context.len() > request.maximum_context_octets as usize
        {
            return Err(failure(JournalError::Limits));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT id,owner FROM tsar_outgoing WHERE sms_id=?1",
                [request.sms_id.0],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        if let Some((id, owner)) = existing {
            let handle = JournalHandle {
                id,
                owner: owner
                    .try_into()
                    .map_err(|_| failure(JournalError::StaleHandle))?,
            };
            let snapshot = load(&transaction, &handle)?;
            if snapshot.peer != request.peer
                || snapshot.version != request.version
                || snapshot.context != request.context
                || snapshot.plan.payload_budget_octets() != request.payload_budget_octets
                || snapshot.plan.acknowledgment_window() != request.acknowledgment_window
                || snapshot.maximum_events != request.maximum_events
                || snapshot.maximum_context_octets != request.maximum_context_octets
            {
                return Err(failure(JournalError::Conflict));
            }
            transaction.commit()?;
            return Ok(snapshot);
        }
        let (peer_kind, peer_value, message, state, accepted): (u8,i64,Vec<u8>,i64,bool) = transaction.query_row("SELECT recipient_type,recipient_value,sms_pdu,state,relay_accepted FROM sms_queue WHERE id=?1", [request.sms_id.0], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?.ok_or(StoreError::MissingMessage)?;
        if (kind, value as i64) != (peer_kind, peer_value) || state != super::QUEUED || accepted {
            return Err(failure(JournalError::Binding));
        }
        let mut selected = None;
        for id in 0..TRANSACTION_COUNT {
            let occupied: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM tsar_outgoing WHERE peer_type=?1 AND peer_value=?2 AND hlpi=?3 AND tsar_id=?4 AND (retired_at_unix_ms IS NULL OR reuse_after_unix_ms>?5))", params![kind,value as i64,service().code().map_err(|_| failure(JournalError::Binding))?,id,created_at_unix_ms], |row| row.get(0))?;
            if !occupied {
                selected = Some(id);
                break;
            }
        }
        let id = selected.ok_or_else(|| failure(JournalError::Capacity))?;
        let MessagePlan::Segmented(plan) = tsar::plan_message(
            &message,
            request.payload_budget_octets,
            id,
            request.acknowledgment_window,
            request.version,
        )
        .map_err(|_| failure(JournalError::Plan))?
        else {
            return Err(failure(JournalError::Plan));
        };
        Sender::try_new(
            request.peer,
            service(),
            plan.clone(),
            request.version,
            timestamp(created_at_unix_ms)?,
        )
        .map_err(|_| failure(JournalError::Binding))?;
        transaction.execute("INSERT INTO tsar_outgoing(sms_id,peer_type,peer_value,hlpi,tsar_id,protocol_version,payload_budget,message,context,maximum_events,created_at_unix_ms,observed_at_unix_ms,maximum_context_octets) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11,?12)", params![request.sms_id.0,kind,value as i64,service().code().map_err(|_| failure(JournalError::Binding))?,id,request.version.code(),request.payload_budget_octets as i64,message,request.context,request.maximum_events,created_at_unix_ms,request.maximum_context_octets])?;
        let stream_id = transaction.last_insert_rowid();
        for segment in plan.segments() {
            transaction.execute(
                "INSERT INTO tsar_outgoing_segments(stream_id,sequence,payload) VALUES (?1,?2,?3)",
                params![
                    stream_id,
                    segment.sequence_number,
                    segment
                        .encode(request.version)
                        .map_err(|_| failure(JournalError::Plan))?
                ],
            )?;
        }
        let owner: Vec<u8> = transaction.query_row(
            "SELECT owner FROM tsar_outgoing WHERE id=?1",
            [stream_id],
            |row| row.get(0),
        )?;
        let handle = JournalHandle {
            id: stream_id,
            owner: owner
                .try_into()
                .map_err(|_| failure(JournalError::StaleHandle))?,
        };
        let snapshot = load(&transaction, &handle)?;
        transaction.commit()?;
        Ok(snapshot)
    }
    /// Fence every old source and bound the peer's late-message lifetime before supplying a reuse guard.
    pub fn retire_outgoing_journal_fenced(
        &mut self,
        handle: &JournalHandle,
        expected_revision: u32,
        now_unix_ms: i64,
        reuse_guard_ms: u64,
    ) -> Result<RetirementReceipt, StoreError> {
        if reuse_guard_ms == 0 {
            return Err(failure(JournalError::GuardDuration));
        }
        let guard =
            i64::try_from(reuse_guard_ms).map_err(|_| failure(JournalError::GuardDuration))?;
        let reuse_after_unix_ms = now_unix_ms
            .checked_add(guard)
            .ok_or_else(|| failure(JournalError::Clock))?;
        let retirement = Retirement {
            retired_at_unix_ms: now_unix_ms,
            reuse_after_unix_ms,
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let snapshot = load(&transaction, handle)?;
        if snapshot.revision() != expected_revision {
            return Err(failure(JournalError::Revision));
        }
        if let Some(previous) = snapshot.retirement {
            if previous != retirement {
                return Err(failure(JournalError::Conflict));
            }
            transaction.commit()?;
            return Ok(RetirementReceipt {
                retirement: previous,
                already_committed: true,
            });
        }
        if now_unix_ms < snapshot.observed_at_unix_ms {
            return Err(failure(JournalError::Clock));
        }
        if !closed(snapshot.replay()?.state()) {
            return Err(failure(JournalError::StillActive));
        }
        transaction.execute(
            "UPDATE tsar_outgoing SET retired_at_unix_ms=?1,reuse_after_unix_ms=?2 WHERE id=?3",
            params![now_unix_ms, reuse_after_unix_ms, handle.id],
        )?;
        transaction.commit()?;
        Ok(RetirementReceipt {
            retirement,
            already_committed: false,
        })
    }
    pub fn outgoing_owns_sms(&self, id: SmsId) -> Result<bool, StoreError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tsar_outgoing WHERE sms_id=?1)",
                [id.0],
                |row| row.get(0),
            )
            .map_err(StoreError::Sql)
    }
    pub fn outgoing_journals(
        &mut self,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<JournalSnapshot>, StoreError> {
        self.scan_outgoing_journals(after_id, limit, false)
    }
    pub fn active_outgoing_journals(
        &mut self,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<JournalSnapshot>, StoreError> {
        self.scan_outgoing_journals(after_id, limit, true)
    }
    fn scan_outgoing_journals(
        &mut self,
        after_id: i64,
        limit: u32,
        active_only: bool,
    ) -> Result<Vec<JournalSnapshot>, StoreError> {
        if after_id < 0 || limit == 0 {
            return Err(failure(JournalError::Limits));
        }
        let transaction = self.connection.transaction()?;
        let sql = if active_only {
            "SELECT id,owner FROM tsar_outgoing WHERE id>?1 AND retired_at_unix_ms IS NULL ORDER BY id LIMIT ?2"
        } else {
            "SELECT id,owner FROM tsar_outgoing WHERE id>?1 ORDER BY id LIMIT ?2"
        };
        let mut statement = transaction.prepare(sql)?;
        let mut snapshots = Vec::new();
        for row in statement.query_map(params![after_id, limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })? {
            let (id, owner) = row?;
            let handle = JournalHandle {
                id,
                owner: owner
                    .try_into()
                    .map_err(|_| failure(JournalError::StaleHandle))?,
            };
            snapshots.push(load(&transaction, &handle)?);
        }
        drop(statement);
        transaction.commit()?;
        Ok(snapshots)
    }
    pub fn outgoing_journal(
        &mut self,
        handle: &JournalHandle,
    ) -> Result<JournalSnapshot, StoreError> {
        let transaction = self.connection.transaction()?;
        let snapshot = load(&transaction, handle)?;
        transaction.commit()?;
        Ok(snapshot)
    }
    pub fn append_outgoing_event(
        &mut self,
        handle: &JournalHandle,
        expected_revision: u32,
        event: JournalEvent,
        observed_at_unix_ms: i64,
    ) -> Result<JournalSnapshot, StoreError> {
        let bytes = encode_event(&event)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = load(&transaction, handle)?;
        if expected_revision < snapshot.revision() {
            let previous = &snapshot.events[expected_revision as usize];
            if previous.event != event || previous.observed_at_unix_ms != observed_at_unix_ms {
                return Err(failure(JournalError::Conflict));
            }
            transaction.commit()?;
            return Ok(snapshot);
        }
        if expected_revision != snapshot.revision() {
            return Err(failure(JournalError::Revision));
        }
        if snapshot.retirement.is_some() {
            return Err(failure(JournalError::Closed));
        }
        if expected_revision >= snapshot.maximum_events {
            return Err(failure(JournalError::Capacity));
        }
        if observed_at_unix_ms < snapshot.observed_at_unix_ms {
            return Err(failure(JournalError::Clock));
        }
        snapshot.events.push(ObservedEvent {
            ordinal: expected_revision,
            observed_at_unix_ms,
            event,
        });
        snapshot.observed_at_unix_ms = observed_at_unix_ms;
        let sender = snapshot.replay()?;
        transaction.execute("INSERT INTO tsar_outgoing_events(stream_id,ordinal,observed_at_unix_ms,event) VALUES (?1,?2,?3,?4)", params![handle.id,expected_revision,observed_at_unix_ms,bytes])?;
        transaction.execute(
            "UPDATE tsar_outgoing SET observed_at_unix_ms=?1,event_count=event_count+1 WHERE id=?2",
            params![observed_at_unix_ms, handle.id],
        )?;
        if matches!(sender.state(), SenderState::Complete(_)) {
            record_relay_accept_in(&transaction, snapshot.sms_id, snapshot.peer)?;
        }
        if matches!(
            sender.state(),
            SenderState::Failed(_) | SenderState::Cancelled
        ) {
            transaction.execute(
                "UPDATE sms_queue SET state=?1 WHERE id=?2 AND state=?3 AND relay_accepted=0",
                params![super::FAILED, snapshot.sms_id.0, super::QUEUED],
            )?;
        }
        transaction.commit()?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests;
