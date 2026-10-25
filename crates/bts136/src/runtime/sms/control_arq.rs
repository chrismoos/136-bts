use super::*;
#[derive(Debug, Clone)]
pub(crate) struct ArqMessage {
    pub identity: MobileIdentity,
    pub bytes: Vec<u8>,
    pub deadline_ms: u64,
    pub(super) generation: u64,
    pub(super) owner: ForwardOwner,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArqOutcome {
    Confirmed,
    Expired,
    Cancelled,
    Failed,
}
impl ArqOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Confirmed => "ARQ confirmed",
            Self::Expired => "ARQ expired without confirmation",
            Self::Cancelled => "ARQ cancelled",
            Self::Failed => "ARQ failed without confirmation",
        }
    }
}
pub(super) struct Transfer {
    pub(super) message: ArqMessage,
    physical_complete: bool,
}
impl SmsRuntime {
    pub(super) fn cancel_control_arq(
        &mut self,
        identity: MobileIdentity,
        now_ms: u64,
    ) -> Result<(), Failure> {
        let messages: Vec<_> = self
            .control_arq
            .iter()
            .filter(|transfer| transfer.message.identity == identity)
            .map(|transfer| transfer.message.clone())
            .collect();
        for message in messages {
            self.arq_ended(message, ArqOutcome::Cancelled, now_ms)?;
        }
        Ok(())
    }
    pub(super) fn arq_busy(&self, identity: MobileIdentity) -> bool {
        self.control_arq
            .iter()
            .any(|pending| pending.message.identity == identity)
    }
    pub(super) fn arq_measurements(&self, id_value: i64) -> Vec<crate::operations::proto::Metric> {
        use crate::operations::metric;
        self.control_arq
            .iter()
            .find(|pending| {
                pending
                    .message
                    .owner
                    .delivery()
                    .is_some_and(|delivery| delivery.id().value() == id_value)
            })
            .map_or_else(Vec::new, |pending| {
                vec![
                    metric("Delivery channel", "SMSCH ARQ", ""),
                    metric(
                        "ARQ deadline",
                        pending.message.deadline_ms,
                        "ms since startup",
                    ),
                    metric("Final TX completed", pending.physical_complete, ""),
                ]
            })
    }
    pub(super) fn detach_control_arq(&mut self, identity: MobileIdentity) -> Vec<Vec<u8>> {
        let mut replies = Vec::new();
        self.control_arq.retain(|pending| {
            if pending.message.identity != identity {
                return true;
            }
            match &pending.message.owner {
                ForwardOwner::Reply => replies.push(pending.message.bytes.clone()),
                ForwardOwner::Delivery(_)
                | ForwardOwner::Auxiliary(_)
                | ForwardOwner::TrackedReply(_) => {}
            }
            false
        });
        replies
    }
    pub(crate) fn arq_offer(
        &self,
        eligible: impl Fn(MobileIdentity) -> bool,
    ) -> Option<ArqMessage> {
        self.pending
            .iter()
            .find(|pending| {
                pending.confirmed
                    && !pending.started
                    && pending.frames.len() > 1
                    && eligible(pending.identity)
                    && self.durable(pending.durable_after)
            })
            .map(|pending| ArqMessage {
                identity: pending.identity,
                bytes: pending.bytes.clone(),
                deadline_ms: pending.deadline_ms,
                generation: pending.generation,
                owner: pending.owner.clone(),
            })
    }
    pub(crate) fn arq_admitted(&mut self, message: ArqMessage) {
        let index = self
            .pending
            .iter()
            .position(|pending| pending.generation == message.generation)
            .expect("admitted SMS offer is pending");
        self.pending.remove(index);
        self.control_arq.push(Transfer {
            message,
            physical_complete: false,
        });
    }
    pub(crate) fn arq_current(&self, message: &ArqMessage) -> bool {
        self.control_arq
            .iter()
            .any(|pending| pending.message.generation == message.generation)
            && message
                .owner
                .delivery()
                .is_none_or(|prepared| self.delivery.is_active(prepared))
            && message
                .owner
                .auxiliary()
                .is_none_or(|packet| self.auxiliary_active(packet))
            && message
                .owner
                .tracked_reply()
                .is_none_or(|packet| self.tracked_reply_active(packet, self.last_ms))
            && (message.owner.is_reply()
                || self
                    .core()
                    .registry()
                    .subscribers()
                    .iter()
                    .any(|subscriber| {
                        subscriber.provisioning.identity == message.identity
                            && subscriber.registration.is_some()
                    }))
    }
    #[cfg(test)]
    pub(crate) fn arq_submitted(&mut self, message: &ArqMessage) {
        self.arq_submitted_at(message, self.last_ms).unwrap();
    }
    pub(crate) fn arq_submitted_at(
        &mut self,
        message: &ArqMessage,
        now_ms: u64,
    ) -> Result<(), Failure> {
        if !self.arq_current(message) {
            return Ok(());
        }
        match &message.owner {
            ForwardOwner::Reply => {}
            ForwardOwner::TrackedReply(packet) => self.reply_handed_off(packet, now_ms, now_ms)?,
            ForwardOwner::Auxiliary(packet) => self.auxiliary_handed_off(packet, now_ms, now_ms)?,
            ForwardOwner::Delivery(prepared) => {
                self.delivery.submitted(prepared);
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn arq_transmitted(
        &mut self,
        message: &ArqMessage,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.arq_transmitted_at(message, now_ms, now_ms)
    }
    pub(crate) fn arq_transmitted_at(
        &mut self,
        message: &ArqMessage,
        completed_ms: u64,
        now_ms: u64,
    ) -> Result<(), Failure> {
        if completed_ms > now_ms {
            return Err(Failure::Clock);
        }
        self.observe(now_ms)?;
        let Some(index) = self
            .control_arq
            .iter()
            .position(|pending| pending.message.generation == message.generation)
        else {
            return Ok(());
        };
        if self.control_arq[index].physical_complete {
            return Ok(());
        }
        match &message.owner {
            ForwardOwner::Reply => {}
            ForwardOwner::TrackedReply(packet) => {
                self.reply_transmitted(packet, completed_ms, now_ms)?
            }
            ForwardOwner::Auxiliary(packet) => {
                self.auxiliary_completed(packet, completed_ms, now_ms)?;
            }
            ForwardOwner::Delivery(prepared) => {
                if self.delivery.is_active(prepared) {
                    self.delivery
                        .transmitted_at(prepared, completed_ms, now_ms)
                        .map_err(Failure::Delivery)?;
                    debug_delivered_sms(prepared);
                }
            }
        }
        self.control_arq[index].physical_complete = true;
        if let Some(log) = &self.protocol {
            log.message(
                "Forward",
                "SMSCH",
                message.identity,
                &message.bytes,
                "TX completion observed. ARQ confirmation pending",
            );
        }
        Ok(())
    }
    pub(crate) fn arq_ended(
        &mut self,
        message: ArqMessage,
        outcome: ArqOutcome,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        let Some(index) = self
            .control_arq
            .iter()
            .position(|pending| pending.message.generation == message.generation)
        else {
            return Ok(());
        };
        self.control_arq.remove(index);
        if outcome != ArqOutcome::Confirmed {
            match &message.owner {
                ForwardOwner::Reply | ForwardOwner::TrackedReply(_) => {}
                ForwardOwner::Auxiliary(packet) => self.request_auxiliary_cancel(packet),
                ForwardOwner::Delivery(prepared) => {
                    self.delivery
                        .cancel_prepared(prepared, now_ms)
                        .map_err(Failure::Delivery)?;
                }
            }
        }
        if let Some(log) = &self.protocol {
            log.message(
                "Forward",
                "SMSCH",
                message.identity,
                &message.bytes,
                outcome.label(),
            );
        }
        Ok(())
    }
}
