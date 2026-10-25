use super::*;
use crate::l3::sms::{DisplayTime, Metadata, UserData, Validity};
const MAX_PARTS: i64 = 4096;
const REFERENCE_MASK: i64 = (1 << 13) - 1;
const CONCAT_8: u8 = 0;
const CONCAT_16: u8 = 8;
const SHORT_REFERENCE_BITS: u8 = 8;
const LONG_REFERENCE_BITS: u8 = 16;
const MESSAGE_MODE_MASK: u8 = 0x03;
const STORE_AND_FORWARD_MODE: u8 = 0x03;
const MAX_PRIORITY: u8 = 3;
#[derive(Clone, Copy)]
struct Concatenation {
    reference: u16,
    width: u8,
    total: u8,
    part: u8,
}
fn concatenation(message: &MessageBody) -> Result<(Option<Concatenation>, &[u8]), StoreError> {
    let mut concat = match (
        message.sar_reference,
        message.sar_total,
        message.sar_sequence,
    ) {
        (None, None, None) => None,
        (Some(reference), Some(total), Some(part)) => Some(Concatenation {
            reference,
            width: LONG_REFERENCE_BITS,
            total,
            part,
        }),
        _ => return Err(HubError::Options.into()),
    };
    let mut bytes = message.text.as_slice();
    if message.esm_class & smpp_core::esm_class::UDHI != 0 {
        if concat.is_some() {
            return Err(HubError::Options.into());
        }
        let length = usize::from(*bytes.first().ok_or(HubError::Length)?) + 1;
        if length > bytes.len() {
            return Err(HubError::Length.into());
        }
        let header = &bytes[1..length];
        concat = Some(match header {
            [CONCAT_8, 3, reference, total, part] => Concatenation {
                reference: u16::from(*reference),
                width: SHORT_REFERENCE_BITS,
                total: *total,
                part: *part,
            },
            [CONCAT_16, 4, high, low, total, part] => Concatenation {
                reference: u16::from_be_bytes([*high, *low]),
                width: LONG_REFERENCE_BITS,
                total: *total,
                part: *part,
            },
            _ => return Err(HubError::Options.into()),
        });
        bytes = &bytes[length..];
    }
    if concat.is_some_and(|c| c.total == 0 || c.part == 0 || c.part > c.total) {
        return Err(HubError::Options.into());
    }
    Ok((concat, bytes))
}
fn validate(message: &MessageBody) -> Result<(), StoreError> {
    const ALLOWED_ESM: u8 = smpp_core::esm_class::UDHI | MESSAGE_MODE_MASK;
    if !number(&message.source.digits) || !number(&message.destination.digits) {
        return Err(HubError::Address.into());
    }
    if message.esm_class & !ALLOWED_ESM != 0
        || !matches!(
            message.esm_class & MESSAGE_MODE_MASK,
            0 | STORE_AND_FORWARD_MODE
        )
        || message.registered_delivery > smpp_core::pdu::DELIVERY_RECEIPT_FAILURE_ONLY
        || message.priority_flag > MAX_PRIORITY
        || !message.schedule_delivery_time.is_empty()
        || !message.validity_period.is_empty()
        || message.replace_if_present != 0
        || message.sm_default_msg_id != 0
        || message.bearer_data.is_some()
        || message.teleservice.is_some()
        || !matches!(message.service_type.as_str(), "" | "CMT")
    {
        return Err(HubError::Options.into());
    }
    for address in [&message.source, &message.destination] {
        if !matches!(address.ton, 0..=4 | 6) || !matches!(address.npi, 0 | 1 | 9) {
            return Err(HubError::Address.into());
        }
    }
    Ok(())
}
fn delivered_text(text: &str, reference: u16, priority: u8) -> Result<Vec<u8>, StoreError> {
    let user_data = UserData::from_text(text).map_err(|_| HubError::Length)?;
    SmsMessage::Deliver {
        metadata: Metadata {
            message_reference: reference,
            privacy: 0,
            urgency: priority,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        validity: Validity::Indefinite,
        display_time: DisplayTime::Invoke,
        user_data,
    }
    .encode()
    .map_err(StoreError::Sms)
}
impl StateStore {
    pub(crate) fn receive_hub_sms(
        &mut self,
        peer: &str,
        message: &MessageBody,
        now_ms: i64,
    ) -> Result<String, StoreError> {
        validate(message)?;
        let (concat, bytes) = concatenation(message)?;
        let text = smpp_core::text::decode_bytes(message.data_coding, bytes)
            .map_err(|_| HubError::Encoding)?;
        delivered_text(&text, 0, message.priority_flag)?;
        let raw = wire(message.clone())?;
        let expires = now_ms
            .checked_add(MESSAGE_TTL_MS)
            .ok_or(StoreError::Timestamp)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recipient:Option<(u8,i64)>=tx.query_row("SELECT identity_type,identity_value FROM subscribers WHERE number=?1 AND enabled=1",[&message.destination.digits],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        tx.execute("DELETE FROM sms_hub_parts WHERE inbox_id IN (SELECT id FROM sms_hub_inbox WHERE expires_ms<=?1)",[now_ms])?;
        let (kind, value) = recipient.ok_or(HubError::Address)?;
        if let Some(reference) = message.user_message_reference {
            let previous:Option<i64>=tx.query_row("SELECT id FROM sms_hub_inbox WHERE peer=?1 AND source=?2 AND destination=?3 AND reference=?4 AND pdu=?5 AND expires_ms>?6 AND failed=0",params![peer,message.source.digits,message.destination.digits,reference,raw,now_ms],|r|r.get(0)).optional()?;
            if let Some(id) = previous {
                return Ok(format!("136bts-{id}"));
            }
        }
        if let Some(c) = concat {
            let existing: Option<(u8, i64, Vec<u8>)> = tx.query_row(
                "SELECT i.recipient_type,i.recipient_value,i.pdu FROM sms_hub_parts p JOIN sms_hub_inbox i ON i.id=p.inbox_id WHERE p.peer=?1 AND p.source=?2 AND p.destination=?3 AND p.reference=?4 AND p.width=?5 AND i.expires_ms>?6 LIMIT 1",
                params![peer,message.source.digits,message.destination.digits,c.reference,c.width,now_ms],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            ).optional()?;
            if let Some((old_kind, old_value, pdu)) = existing {
                let old = body(&pdu)?;
                if (old_kind, old_value) != (kind, value)
                    || old.data_coding != message.data_coding
                    || old.priority_flag != message.priority_flag
                    || concatenation(&old)?.0.map(|part| part.total) != Some(c.total)
                {
                    return Err(StoreError::IdempotencyConflict);
                }
            }
            let previous:Option<(i64,Vec<u8>)>=tx.query_row("SELECT p.inbox_id,i.pdu FROM sms_hub_parts p JOIN sms_hub_inbox i ON i.id=p.inbox_id WHERE p.peer=?1 AND p.source=?2 AND p.destination=?3 AND p.reference=?4 AND p.width=?5 AND p.coding=?6 AND p.total=?7 AND p.part=?8 AND i.expires_ms>?9",params![peer,message.source.digits,message.destination.digits,c.reference,c.width,message.data_coding,c.total,c.part,now_ms],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((id, previous)) = previous {
                if previous != raw {
                    return Err(StoreError::IdempotencyConflict);
                }
                return Ok(format!("136bts-{id}"));
            }
        }
        let pending: i64 = tx.query_row(
            "SELECT count(*) FROM sms_hub_inbox i LEFT JOIN sms_queue q ON q.id=i.sms_id WHERE i.failed=0 AND (i.sms_id IS NULL OR q.state=0)",
            [],
            |r| r.get(0),
        )?;
        if pending >= MAX_PARTS {
            return Err(HubError::QueueFull.into());
        }
        tx.execute("INSERT INTO sms_hub_inbox(peer,source,destination,reference,pdu,request_receipt,created_ms,expires_ms,recipient_type,recipient_value) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![peer,message.source.digits,message.destination.digits,message.user_message_reference,raw,message.registered_delivery,now_ms,expires,kind,value])?;
        let inbox = tx.last_insert_rowid();
        let (complete, owners) = if let Some(c) = concat {
            tx.execute("INSERT INTO sms_hub_parts(peer,source,destination,reference,width,coding,total,part,data,inbox_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![peer,message.source.digits,message.destination.digits,c.reference,c.width,message.data_coding,c.total,c.part,text.as_bytes(),inbox])?;
            let mut statement=tx.prepare("SELECT p.data,p.inbox_id FROM sms_hub_parts p JOIN sms_hub_inbox i ON i.id=p.inbox_id WHERE p.peer=?1 AND p.source=?2 AND p.destination=?3 AND p.reference=?4 AND p.width=?5 AND p.coding=?6 AND p.total=?7 AND i.expires_ms>?8 AND i.failed=0 ORDER BY p.part")?;
            let parts = statement
                .query_map(
                    params![
                        peer,
                        message.source.digits,
                        message.destination.digits,
                        c.reference,
                        c.width,
                        message.data_coding,
                        c.total,
                        now_ms
                    ],
                    |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let mut data = Vec::new();
            let mut owners = Vec::new();
            for (part, id) in parts {
                data.extend(part);
                owners.push(id);
            }
            if owners.len() == usize::from(c.total) {
                (
                    Some(String::from_utf8(data).map_err(|_| StoreError::CorruptState)?),
                    owners,
                )
            } else {
                (None, owners)
            }
        } else {
            (Some(text), vec![inbox])
        };
        if let Some(text) = complete {
            let payload = delivered_text(
                &text,
                (inbox & REFERENCE_MASK) as u16,
                message.priority_flag,
            )?;
            tx.execute("INSERT INTO sms_queue(dedup_key,sender_type,sender_value,recipient_type,recipient_value,sender_number,sms_pdu,created_at_unix_ms,recipient_number) VALUES(?1,NULL,NULL,?2,?3,?4,?5,?6,?7)",params![format!("hub-inbox:{inbox}"),kind,value,message.source.digits,payload,now_ms,message.destination.digits])?;
            let sms = tx.last_insert_rowid();
            for owner in owners {
                tx.execute(
                    "UPDATE sms_hub_inbox SET sms_id=?1 WHERE id=?2",
                    params![sms, owner],
                )?;
            }
        }
        tx.commit()?;
        Ok(format!("136bts-{inbox}"))
    }
}
