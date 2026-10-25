use super::*;

const PENDING: i64 = 0;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResponseClosure {
    pub ordinal: u32,
    pub settled_at_unix_ms: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum ResponseOutcome {
    Accepted = 1,
    Rejected = 2,
    Expired = 3,
    Canceled = 4,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHandle {
    journal: JournalHandle,
    ordinal: u32,
    bytes: Vec<u8>,
}
impl ResponseHandle {
    pub fn journal(&self) -> &JournalHandle {
        &self.journal
    }
    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }
}
#[derive(Debug, Clone)]
pub struct IncomingResponse {
    pub handle: ResponseHandle,
    pub journal: IncomingJournal,
    pub response: tsar::Response,
}
pub(super) fn encode_response(
    journal: &IncomingJournal,
    event: &JournalEvent,
    response: Option<&tsar::Response>,
) -> Result<Option<Vec<u8>>, StoreError> {
    response
        .map(|response| {
            if !matches!(event, JournalEvent::Segment(_) | JournalEvent::Advance)
                || response.transaction_id != journal.binding.transaction_id
            {
                return Err(failure(JournalError::Binding));
            }
            response
                .encode(journal.binding.version)
                .map_err(|_| failure(JournalError::Segment))
        })
        .transpose()
}
impl StateStore {
    pub(crate) fn incoming_completion_response(
        &self,
        handle: &JournalHandle,
    ) -> Result<Option<ResponseClosure>, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let (journal, application, _) = read_handle(&transaction, handle)?;
        if application != APPLICATION_COMMITTED || journal.reuse_after_unix_ms.is_some() {
            return Ok(None);
        }
        let pending: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM tsar_incoming_responses WHERE stream_id=?1 AND outcome=?2)",
            params![handle.id, PENDING],
            |row| row.get(0),
        )?;
        if pending {
            return Ok(None);
        }
        let latest = transaction.query_row(
            "SELECT event_ordinal,response,outcome,settled_at_unix_ms FROM tsar_incoming_responses WHERE stream_id=?1 ORDER BY event_ordinal DESC LIMIT 1",
            [handle.id],
            |row| Ok((row.get::<_,u32>(0)?, row.get::<_,Vec<u8>>(1)?, row.get::<_,i64>(2)?, row.get::<_,Option<i64>>(3)?)),
        ).optional()?;
        let Some((ordinal, bytes, outcome, settled)) = latest else {
            return Ok(None);
        };
        let decoded = tsar::decode_response(&bytes, journal.binding.version)
            .map_err(|_| StoreError::CorruptState)?;
        if decoded.response.transaction_id != journal.binding.transaction_id
            || decoded
                .response
                .encode(journal.binding.version)
                .map_err(|_| StoreError::CorruptState)?
                != bytes
        {
            return Err(StoreError::CorruptState);
        }
        let closed = outcome == ResponseOutcome::Accepted as i64
            && decoded.response.status == tsar::Status::Received;
        let proof = if closed {
            let settled_at_unix_ms = settled.ok_or(StoreError::CorruptState)?;
            if settled_at_unix_ms < journal.created_at_unix_ms {
                return Err(StoreError::CorruptState);
            }
            Some(ResponseClosure {
                ordinal,
                settled_at_unix_ms,
            })
        } else {
            None
        };
        transaction.commit()?;
        Ok(proof)
    }
    pub fn pending_incoming_responses(
        &self,
        limit: u32,
    ) -> Result<Vec<IncomingResponse>, StoreError> {
        if limit == 0 {
            return Err(failure(JournalError::Limits));
        }
        let transaction = self.connection.unchecked_transaction()?;
        let mut statement = transaction.prepare("SELECT r.stream_id,s.owner,r.event_ordinal,r.response,r.awaits_application FROM tsar_incoming_responses r JOIN tsar_incoming s ON s.id=r.stream_id WHERE r.outcome=?1 AND s.reuse_after_unix_ms IS NULL AND (s.application_state<>0 OR r.awaits_application=0) AND NOT EXISTS(SELECT 1 FROM tsar_incoming_responses older WHERE older.stream_id=r.stream_id AND older.outcome=?1 AND older.event_ordinal<r.event_ordinal) ORDER BY r.stream_id,r.event_ordinal LIMIT ?2")?;
        let rows = statement.query_map(params![PENDING, limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?;
        let mut responses = Vec::new();
        for row in rows {
            let (id, owner, ordinal, bytes, awaits_application) = row?;
            let journal_handle = JournalHandle {
                id,
                owner: owner.try_into().map_err(|_| StoreError::CorruptState)?,
            };
            let (journal, application, _) = read_handle(&transaction, &journal_handle)?;
            let decoded = tsar::decode_response(&bytes, journal.binding.version)
                .map_err(|_| StoreError::CorruptState)?;
            if awaits_application != (decoded.response.status == tsar::Status::Received) {
                return Err(StoreError::CorruptState);
            }
            if decoded.response.transaction_id != journal.binding.transaction_id
                || decoded
                    .response
                    .encode(journal.binding.version)
                    .map_err(|_| StoreError::CorruptState)?
                    != bytes
            {
                return Err(StoreError::CorruptState);
            }
            if decoded.response.status == tsar::Status::Received
                && application == APPLICATION_UNSETTLED
            {
                continue;
            }
            responses.push(IncomingResponse {
                handle: ResponseHandle {
                    journal: journal_handle,
                    ordinal,
                    bytes,
                },
                journal,
                response: decoded.response,
            });
        }
        drop(statement);
        transaction.commit()?;
        Ok(responses)
    }
    /// Settle after the exact auxiliary exchange has completed its radio fence. Pending work survives process loss without an ephemeral claim.
    pub fn settle_incoming_response(
        &mut self,
        handle: &ResponseHandle,
        outcome: ResponseOutcome,
        now_unix_ms: i64,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (journal, _, _) = read_handle(&transaction, &handle.journal)?;
        let previous = transaction.query_row("SELECT response,outcome,settled_at_unix_ms FROM tsar_incoming_responses WHERE stream_id=?1 AND event_ordinal=?2", params![handle.journal.id,handle.ordinal], |row| Ok((row.get::<_,Vec<u8>>(0)?,row.get::<_,i64>(1)?,row.get::<_,Option<i64>>(2)?))).optional()?.ok_or(failure(JournalError::StaleHandle))?;
        if previous.0 != handle.bytes {
            return Err(StoreError::CorruptState);
        }
        if previous.1 != PENDING {
            if previous.1 == outcome as i64
                && previous.2.is_some_and(|settled| now_unix_ms >= settled)
            {
                transaction.commit()?;
                return Ok(());
            }
            return Err(failure(JournalError::RevisionConflict));
        }
        if now_unix_ms < journal.observed_at_unix_ms {
            return Err(failure(JournalError::ClockRegression));
        }
        transaction.execute("UPDATE tsar_incoming_responses SET outcome=?1,settled_at_unix_ms=?2 WHERE stream_id=?3 AND event_ordinal=?4", params![outcome as i64,now_unix_ms,handle.journal.id,handle.ordinal])?;
        transaction.commit()?;
        Ok(())
    }
}
