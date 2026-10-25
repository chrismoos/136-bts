mod inbound;
mod jobs;
use super::*;
use smpp_core::{Address, Body, MessageBody, Pdu};
pub(crate) const MESSAGE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
pub(crate) const RETRY_MS: i64 = 30_000;
const MAX_PENDING: i64 = 1000;
const STORED_SEQUENCE: u32 = 1;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubError {
    Disabled,
    Address,
    Options,
    Encoding,
    Length,
    Receipt,
    QueueFull,
}
impl From<HubError> for StoreError {
    fn from(error: HubError) -> Self {
        Self::Hub(error)
    }
}
pub(crate) struct ExternalSubmission<'a> {
    pub sender: MobileIdentity,
    pub expected_sender: Option<&'a str>,
    pub destination: &'a str,
    pub payload: &'a [u8],
    pub key: &'a str,
    pub created_ms: i64,
    pub enabled: bool,
}
pub(crate) fn number(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}
pub(crate) fn wire(body: MessageBody) -> Result<Vec<u8>, StoreError> {
    smpp_core::encode(&Pdu::request(
        STORED_SEQUENCE,
        Body::SubmitSm(Box::new(body)),
    ))
    .map_err(|_| HubError::Options.into())
}
pub(crate) fn body(bytes: &[u8]) -> Result<MessageBody, StoreError> {
    match smpp_core::decode(bytes)
        .map_err(|_| StoreError::CorruptState)?
        .body
    {
        Body::SubmitSm(body) | Body::DeliverSm(body) => Ok(*body),
        _ => Err(StoreError::CorruptState),
    }
}
impl StateStore {
    pub(crate) fn number_is_local(&self, number: &str) -> Result<bool, StoreError> {
        Ok(self.connection.query_row("SELECT EXISTS(SELECT 1 FROM subscribers WHERE number=?1) OR EXISTS(SELECT 1 FROM esn_subscribers WHERE number=?1)",[number],|row|row.get(0))?)
    }
    pub(crate) fn enqueue_external_sms(
        &mut self,
        request: ExternalSubmission<'_>,
    ) -> Result<SmsId, StoreError> {
        let ExternalSubmission {
            sender,
            expected_sender,
            destination,
            payload,
            key,
            created_ms,
            enabled,
        } = request;
        if key.is_empty() {
            return Err(StoreError::DedupKey);
        }
        if created_ms < 0 {
            return Err(StoreError::Timestamp);
        }
        if !number(destination) {
            return Err(HubError::Address.into());
        }
        let decoded = sms::decode_prefix(payload, Direction::Reverse).map_err(StoreError::Sms)?;
        let SmsMessage::Submit {
            metadata,
            user_data,
        } = decoded.message
        else {
            return Err(StoreError::MessageType);
        };
        // TIA/EIA-136-710-C §2.2.2 defines DELIVERY ACK as first user display, not SMSC delivery.
        if metadata.delivery_acknowledgment
            || metadata.manual_acknowledgment
            || metadata.privacy != 0
            || !metadata.new_message
        {
            return Err(HubError::Options.into());
        }
        let text = user_data.text().ok_or(HubError::Encoding)?;
        let (coding, data) = smpp_core::text::encode(&text).map_err(|_| HubError::Encoding)?;
        let deliver = SmsMessage::Deliver {
            metadata,
            validity: sms::Validity::Indefinite,
            display_time: sms::DisplayTime::Invoke,
            user_data,
        };
        let stored_payload = deliver.encode().map_err(StoreError::Sms)?;
        let dedup = submission_dedup_key(key);
        let (kind, _, value) = sender.parts();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = tx.query_row("SELECT id,sender_type,sender_value,sender_number,recipient_number,sms_pdu,recipient_type FROM sms_queue WHERE dedup_key=?1",[&dedup],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,Option<u8>>(1)?,row.get::<_,Option<i64>>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,Vec<u8>>(5)?,row.get::<_,Option<u8>>(6)?))).optional()?;
        if let Some((
            id,
            old_kind,
            old_value,
            old_sender,
            old_destination,
            old_payload,
            recipient,
        )) = previous
        {
            if old_kind != Some(kind)
                || old_value != Some(value as i64)
                || recipient.is_some()
                || old_destination != destination
                || old_payload != stored_payload
                || expected_sender.is_some_and(|v| v != old_sender)
            {
                return Err(StoreError::IdempotencyConflict);
            }
            return Ok(SmsId(id));
        }
        if !enabled {
            return Err(HubError::Disabled.into());
        }
        let sender_number = enabled_number(&tx, kind, value)?;
        if expected_sender.is_some_and(|v| v != sender_number) {
            return Err(StoreError::SenderMismatch);
        }
        let local:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM subscribers WHERE number=?1) OR EXISTS(SELECT 1 FROM esn_subscribers WHERE number=?1)",[destination],|row|row.get(0))?;
        if local {
            return Err(StoreError::RecipientMismatch);
        }
        let pending: i64 = tx.query_row(
            "SELECT count(*) FROM sms_hub_outbox WHERE state IN ('queued','sending')",
            [],
            |row| row.get(0),
        )?;
        if pending >= MAX_PENDING {
            return Err(HubError::QueueFull.into());
        }
        let body = MessageBody {
            source: Address::unknown(&sender_number),
            destination: Address::unknown(destination),
            text: data,
            data_coding: coding,
            // SMPP v3.4 Table 5-5 and TIA/EIA-136-710-C §4.26 use the same four priority levels.
            priority_flag: metadata.urgency,
            user_message_reference: Some(metadata.message_reference),
            registered_delivery: smpp_core::REGISTERED_DELIVERY_RECEIPT,
            ..Default::default()
        };
        let pdu = wire(body)?;
        let expires = created_ms
            .checked_add(MESSAGE_TTL_MS)
            .ok_or(StoreError::Timestamp)?;
        tx.execute("INSERT INTO sms_queue(dedup_key,sender_type,sender_value,recipient_type,recipient_value,sender_number,sms_pdu,created_at_unix_ms,recipient_number) VALUES(?1,?2,?3,NULL,NULL,?4,?5,?6,?7)",params![dedup,kind,value as i64,sender_number,stored_payload,created_ms,destination])?;
        let id = SmsId(tx.last_insert_rowid());
        tx.execute(
            "INSERT INTO sms_hub_outbox(sms_id,pdu,next_attempt_ms,expires_ms) VALUES(?1,?2,?3,?4)",
            params![id.0, pdu, created_ms, expires],
        )?;
        tx.commit()?;
        Ok(id)
    }
}
#[cfg(test)]
mod tests;
