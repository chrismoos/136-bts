use super::*;
const LOCAL_PREFIX: &str = "136bts-";
pub(crate) struct Job {
    pub id: i64,
    pub body: MessageBody,
}
impl StateStore {
    pub(crate) fn recover_hub_sms(&mut self) -> Result<(), StoreError> {
        self.connection.execute("UPDATE sms_hub_outbox SET state='queued',detail='Retrying after disconnect' WHERE state='sending'",[])?;
        Ok(())
    }
    pub(crate) fn next_hub_sms(
        &mut self,
        now_ms: i64,
        server: bool,
        peer: &str,
    ) -> Result<Option<Job>, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE sms_hub_outbox SET state='expired',detail='Expired before peer acceptance' WHERE state='queued' AND expires_ms<=?1",[now_ms])?;
        tx.execute("UPDATE sms_queue SET state=?1 WHERE id IN (SELECT sms_id FROM sms_hub_outbox WHERE state='expired' AND receipt_inbox_id IS NULL) AND state=?2",params![FAILED,QUEUED])?;
        tx.execute(
            "UPDATE sms_hub_inbox SET failed=1 WHERE sms_id IS NULL AND expires_ms<=?1",
            [now_ms],
        )?;
        let row:Option<(i64,Vec<u8>)>=tx.query_row("SELECT id,pdu FROM sms_hub_outbox WHERE state='queued' AND next_attempt_ms<=?1 AND expires_ms>?1 AND (receipt_inbox_id IS NULL OR (?2 AND receipt_inbox_id IN (SELECT id FROM sms_hub_inbox WHERE peer=?3))) ORDER BY id LIMIT 1",params![now_ms,server,peer],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let result = if let Some((id, bytes)) = row {
            let body = body(&bytes)?;
            tx.execute(
                "UPDATE sms_hub_outbox SET state='sending',attempts=attempts+1 WHERE id=?1",
                [id],
            )?;
            Some(Job { id, body })
        } else {
            None
        };
        tx.commit()?;
        Ok(result)
    }
    pub(crate) fn retry_hub_sms(&mut self, id: i64, now_ms: i64) -> Result<(), StoreError> {
        self.connection.execute("UPDATE sms_hub_outbox SET state='queued',next_attempt_ms=?2,detail='Peer response lost. Retrying may duplicate delivery' WHERE id=?1 AND state='sending'",params![id,now_ms.saturating_add(RETRY_MS)])?;
        Ok(())
    }
    pub(crate) fn hub_sms_result(
        &mut self,
        id: i64,
        status: smpp_core::CommandStatus,
        remote_id: &str,
        peer: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let state = if status == smpp_core::CommandStatus::Ok {
            "accepted"
        } else if status.is_retryable() {
            "queued"
        } else {
            "failed"
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated=tx.execute("UPDATE sms_hub_outbox SET state=?2,peer_id=?3,next_attempt_ms=?4,detail=?5,peer=?6 WHERE id=?1 AND state='sending'",params![id,state,remote_id,now_ms.saturating_add(RETRY_MS),if state=="accepted"{"Accepted by peer. Handset delivery is unconfirmed"}else{"Peer refused message"},peer])?;
        if updated != 1 {
            return Err(StoreError::InvalidTransition);
        }
        if state == "failed" {
            tx.execute("UPDATE sms_queue SET state=?1 WHERE id=(SELECT sms_id FROM sms_hub_outbox WHERE id=?2 AND receipt_inbox_id IS NULL)",params![FAILED,id])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn receive_hub_receipt(
        &mut self,
        peer: &str,
        message: &MessageBody,
    ) -> Result<(), StoreError> {
        let receipt = smpp_core::receipt::parse(message).ok_or(HubError::Receipt)?;
        let remote = &receipt.message_id;
        let state = receipt.state;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous:Option<(i64,i64,String)>=tx.query_row("SELECT h.id,h.sms_id,h.state FROM sms_hub_outbox h JOIN sms_queue q ON q.id=h.sms_id WHERE h.peer_id=?1 AND h.peer=?4 AND q.sender_number=?2 AND q.recipient_number=?3 AND h.receipt_inbox_id IS NULL ORDER BY h.id DESC LIMIT 1",params![remote,message.destination.digits,message.source.digits,peer],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (id, sms, old) = previous.ok_or(HubError::Receipt)?;
        if !smpp_core::pdu::message_state::is_final(state) {
            return Ok(());
        }
        let delivered = state == smpp_core::pdu::message_state::DELIVERED;
        let target = if delivered { "delivered" } else { "failed" };
        if old == target {
            return Ok(());
        }
        if old != "accepted" {
            return Err(HubError::Receipt.into());
        }
        tx.execute(
            "UPDATE sms_hub_outbox SET state=?2,detail=?3 WHERE id=?1",
            params![id, target, format!("Final peer delivery report: {state}")],
        )?;
        tx.execute(
            "UPDATE sms_queue SET state=?2 WHERE id=?1",
            params![sms, if delivered { DELIVERED } else { FAILED }],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn queue_hub_reports(&mut self, now_ms: i64) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE sms_hub_inbox SET failed=1 WHERE sms_id IS NULL AND expires_ms<=?1",
            [now_ms],
        )?;
        let mut statement=tx.prepare("SELECT i.id,i.source,i.destination,i.sms_id,q.state,i.failed FROM sms_hub_inbox i LEFT JOIN sms_queue q ON q.id=i.sms_id WHERE i.request_receipt!=0 AND (q.state IN (?1,?2) OR i.failed=1) AND NOT EXISTS(SELECT 1 FROM sms_hub_outbox h WHERE h.receipt_inbox_id=i.id) AND (i.request_receipt=1 OR q.state!=?1 OR i.failed=1)")?;
        let rows = statement
            .query_map(params![DELIVERED, FAILED], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, bool>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (inbox, source, destination, sms, state, failed) in rows {
            let body = MessageBody {
                source: Address::unknown(destination),
                destination: Address::unknown(source),
                esm_class: smpp_core::esm_class::DELIVERY_RECEIPT,
                receipted_message_id: Some(format!("{LOCAL_PREFIX}{inbox}")),
                message_state: Some(if state == Some(DELIVERED) && !failed {
                    smpp_core::pdu::message_state::DELIVERED
                } else if failed {
                    smpp_core::pdu::message_state::EXPIRED
                } else {
                    smpp_core::pdu::message_state::UNDELIVERABLE
                }),
                ..Default::default()
            };
            tx.execute("INSERT INTO sms_hub_outbox(sms_id,pdu,next_attempt_ms,expires_ms,receipt_inbox_id) VALUES(?1,?2,?3,?4,?5)",params![sms,wire(body)?,now_ms,now_ms.saturating_add(MESSAGE_TTL_MS),inbox])?;
        }
        tx.commit()?;
        Ok(())
    }
}
impl StateStore {
    pub(crate) fn hub_queue_depth(&self) -> Result<usize, StoreError> {
        let count: i64 = self.connection.query_row(
            "SELECT count(*) FROM sms_hub_outbox WHERE state IN ('queued','sending')",
            [],
            |r| r.get(0),
        )?;
        usize::try_from(count).map_err(|_| StoreError::CorruptState)
    }
}
impl StateStore {
    pub(crate) fn append_hub_details(
        &self,
        message: &mut crate::operations::proto::Sms,
    ) -> Result<bool, StoreError> {
        let outbound:Option<(String,Option<String>,String)>=self.connection.query_row("SELECT state,peer_id,detail FROM sms_hub_outbox WHERE sms_id=?1 AND receipt_inbox_id IS NULL",[message.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((state, peer_id, detail)) = outbound {
            use crate::operations::proto::SmsPhase;
            let (phase, label) = match state.as_str() {
                "queued" => (SmsPhase::Queued, "Queued for hub"),
                "sending" => (SmsPhase::Queued, "Sending to hub"),
                "accepted" => (SmsPhase::Queued, "Accepted by peer"),
                "delivered" => (SmsPhase::Delivered, "Delivered"),
                "expired" => (SmsPhase::Failed, "Expired"),
                _ => (SmsPhase::Failed, "Failed"),
            };
            message.set_phase(phase);
            message
                .measurements
                .push(crate::operations::metric("Route", "SMS hub", ""));
            message
                .measurements
                .push(crate::operations::metric("Hub progress", label, ""));
            message
                .measurements
                .push(crate::operations::metric("Hub status", detail, ""));
            if let Some(peer_id) = peer_id.filter(|id| !id.is_empty()) {
                message.measurements.push(crate::operations::metric(
                    "Peer message ID",
                    peer_id,
                    "",
                ));
            }
            return Ok(true);
        }
        let inbound: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sms_hub_inbox WHERE sms_id=?1)",
            [message.id],
            |r| r.get(0),
        )?;
        if inbound {
            message
                .measurements
                .push(crate::operations::metric("Origin", "SMS hub", ""));
        }
        Ok(false)
    }
}
