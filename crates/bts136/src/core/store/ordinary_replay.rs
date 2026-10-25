use super::*;
const REQUEST_HEADER_OCTETS: usize = 1;
const MAX_REQUEST_OCTETS: usize = sms::MAX_SUBMIT_PDU_OCTETS + REQUEST_HEADER_OCTETS;
const MAX_OPTIONAL_OCTETS: usize = u8::MAX as usize;
const MAX_ACTIVE_REQUESTS: u32 = 256;
#[derive(Debug, Clone)]
pub(crate) struct Request {
    id: i64,
    owner: Vec<u8>,
    sender: MobileIdentity,
    pub observed_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
    pub response: Option<Vec<u8>>,
}
impl Request {
    pub fn submission_key(&self) -> String {
        use std::fmt::Write;
        let mut key = format!("ordinary-request:{}:", self.id);
        for octet in &self.owner {
            write!(key, "{octet:02x}").expect("writing into String is infallible");
        }
        key
    }
}
// Request bytes are one transaction-ID octet followed by the unchanged CMT PDU.
fn transaction_id(bytes: &[u8]) -> Result<u8, StoreError> {
    let (id, payload) = bytes.split_first().ok_or(StoreError::MessageType)?;
    if !matches!(
        sms::decode_prefix(payload, Direction::Reverse)
            .map_err(StoreError::Sms)?
            .message,
        SmsMessage::Submit { .. }
    ) {
        return Err(StoreError::MessageType);
    }
    Ok(*id)
}

impl StateStore {
    pub(crate) fn validate_ordinary_clock(&self, now_unix_ms: i64) -> Result<(), StoreError> {
        validate_clock(&self.connection, now_unix_ms)
    }
    /// Serialize each request from this durable claim through SMS insertion and response settlement.
    pub(crate) fn claim_ordinary_request(
        &mut self,
        sender: MobileIdentity,
        bytes: &[u8],
        optional_octets: &[u8],
        now_unix_ms: i64,
        guard_ms: u64,
    ) -> Result<Request, StoreError> {
        transaction_id(bytes)?;
        let (kind, width, value) = sender.parts();
        if value == 0
            || value >= 1 << width
            || bytes.len() > MAX_REQUEST_OCTETS
            || optional_octets.len() > MAX_OPTIONAL_OCTETS
        {
            return Err(StoreError::MessageType);
        }
        let guard = i64::try_from(guard_ms).map_err(|_| StoreError::Timestamp)?;
        let expires = now_unix_ms
            .checked_add(guard)
            .filter(|_| guard > 0)
            .ok_or(StoreError::Timestamp)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_clock(&transaction, now_unix_ms)?;
        let latest = transaction.query_row(
            "SELECT id,observed_at_unix_ms,expires_at_unix_ms,response,owner FROM ordinary_sms_requests WHERE sender_type=?1 AND sender_value=?2 AND request=?3 AND optional_octets=?4 ORDER BY id DESC LIMIT 1",
            params![kind,value as i64,bytes,optional_octets],
            |row| Ok(Request { id: row.get(0)?, sender, observed_at_unix_ms: row.get(1)?, expires_at_unix_ms: row.get(2)?, response: row.get(3)?, owner: row.get(4)? }),
        ).optional()?;
        if let Some(request) = latest
            && now_unix_ms < request.expires_at_unix_ms
        {
            transaction.commit()?;
            return Ok(request);
        }
        let active: u32 = transaction.query_row(
            "SELECT count(*) FROM ordinary_sms_requests WHERE expires_at_unix_ms>?1",
            [now_unix_ms],
            |row| row.get(0),
        )?;
        if active >= MAX_ACTIVE_REQUESTS {
            return Err(StoreError::SubmissionCapacity);
        }
        transaction.execute("INSERT INTO ordinary_sms_requests(sender_type,sender_value,request,optional_octets,observed_at_unix_ms,expires_at_unix_ms) VALUES(?1,?2,?3,?4,?5,?6)", params![kind,value as i64,bytes,optional_octets,now_unix_ms,expires])?;
        let id = transaction.last_insert_rowid();
        let owner = transaction.query_row(
            "SELECT owner FROM ordinary_sms_requests WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        let request = Request {
            id,
            owner,
            sender,
            observed_at_unix_ms: now_unix_ms,
            expires_at_unix_ms: expires,
            response: None,
        };
        transaction.commit()?;
        Ok(request)
    }
    pub(crate) fn ordinary_request_sms(
        &self,
        request: &Request,
    ) -> Result<Option<SmsId>, StoreError> {
        let (kind, _, value) = request.sender.parts();
        let owned: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM ordinary_sms_requests WHERE id=?1 AND owner=?2 AND sender_type=?3 AND sender_value=?4)",
            params![request.id, request.owner, kind, value as i64], |row| row.get(0),
        )?;
        if !owned {
            return Err(StoreError::InvalidTransition);
        }

        let existing = self
            .connection
            .query_row(
                "SELECT id,sender_type,sender_value FROM sms_queue WHERE dedup_key=?1",
                [submission_dedup_key(&request.submission_key())],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, u8>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        existing
            .map(|(id, old_kind, old_value)| {
                if old_kind != kind || old_value != value as i64 {
                    return Err(StoreError::CorruptState);
                }
                Ok(SmsId(id))
            })
            .transpose()
    }
    pub(crate) fn settle_ordinary_request(
        &mut self,
        request: &Request,
        response: &[u8],
        now_unix_ms: i64,
    ) -> Result<(), StoreError> {
        let decoded = crate::l3::relay::decode_prefix(response, Direction::Forward)
            .map_err(|_| StoreError::MessageType)?;
        if decoded
            .message
            .encode(Direction::Forward)
            .map_err(|_| StoreError::MessageType)?
            != response
        {
            return Err(StoreError::MessageType);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_clock(&transaction, now_unix_ms)?;
        let (bytes, observed, old_response, old_settled): (Vec<u8>,i64,Option<Vec<u8>>,Option<i64>) = transaction.query_row(
            "SELECT request,observed_at_unix_ms,response,settled_at_unix_ms FROM ordinary_sms_requests WHERE id=?1 AND owner=?2", params![request.id, request.owner], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        )?;
        if now_unix_ms < observed {
            return Err(StoreError::Timestamp);
        }
        let id = transaction_id(&bytes)?;
        let accepted = match decoded.message {
            RelayMessage::Accept { transaction_id } if transaction_id == id => true,
            RelayMessage::Reject { transaction_id, .. } if transaction_id == id => false,
            _ => return Err(StoreError::InvalidTransition),
        };
        let stored_sender = transaction
            .query_row(
                "SELECT sender_type,sender_value FROM sms_queue WHERE dedup_key=?1",
                [submission_dedup_key(&request.submission_key())],
                |row| Ok((row.get::<_, u8>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let (kind, _, value) = request.sender.parts();
        if stored_sender.is_some_and(|old| old != (kind, value as i64)) {
            return Err(StoreError::CorruptState);
        }
        let stored = stored_sender.is_some();
        if accepted != stored {
            return Err(StoreError::InvalidTransition);
        }
        if let Some(previous) = old_response {
            if previous != response || old_settled.is_none_or(|at| now_unix_ms < at) {
                return Err(StoreError::InvalidTransition);
            }
        } else {
            transaction.execute(
                "UPDATE ordinary_sms_requests SET response=?1,settled_at_unix_ms=?2 WHERE id=?3",
                params![response, now_unix_ms, request.id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn validate_clock(connection: &Connection, now_unix_ms: i64) -> Result<(), StoreError> {
    let latest: Option<i64> = connection.query_row(
        "SELECT max(coalesce(settled_at_unix_ms,observed_at_unix_ms)) FROM ordinary_sms_requests",
        [],
        |row| row.get(0),
    )?;
    if now_unix_ms < 0 || latest.is_some_and(|latest| now_unix_ms < latest) {
        return Err(StoreError::Timestamp);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
