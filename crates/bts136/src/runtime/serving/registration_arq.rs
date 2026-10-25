use super::*;
use std::collections::VecDeque;
#[derive(Default)]
pub(super) struct Backlog {
    pending: VecDeque<super::super::spach_arq::Request<ArqOwner>>,
}
impl Backlog {
    pub(super) fn enqueue(
        &mut self,
        request: super::super::spach_arq::Request<ArqOwner>,
    ) -> Result<(), crate::runtime::registration::Error> {
        if self.pending.len() >= MAX_REPLIES
            && !self
                .pending
                .iter()
                .any(|pending| pending.identity == request.identity)
        {
            return Err(crate::runtime::registration::Error::QueueFull);
        }
        self.cancel(request.identity);
        self.pending.push_back(request);
        Ok(())
    }
    pub(super) fn cancel(&mut self, identity: crate::link::rach::MobileIdentity) {
        self.pending.retain(|pending| pending.identity != identity);
    }
    pub(super) fn advance(
        &mut self,
        sender: &mut super::super::spach_arq::Sender<ArqOwner>,
        block: u64,
        now_ms: u64,
        cancelled: impl Fn(crate::link::rach::MobileIdentity) -> bool,
        protocol: Option<&crate::operations::protocol::ProtocolLog>,
    ) -> Result<(), super::super::spach_arq::Error> {
        let mut index = 0;
        while index < self.pending.len() {
            let request = &self.pending[index];
            let outcome = if now_ms >= request.deadline_ms {
                Some("Expired before ARQ admission")
            } else if cancelled(request.identity) {
                Some("Cancelled before ARQ admission")
            } else {
                None
            };
            if let Some(outcome) = outcome {
                let request = self.pending.remove(index).expect("pending index exists");
                if let Some(protocol) = protocol {
                    protocol.arq_outcome(request.identity, outcome);
                }
                tracing::warn!(identity=%request.identity,outcome,"registration ARQ admission ended");
            } else {
                index += 1;
            }
        }
        while sender.can_enqueue(block, now_ms) {
            let Some(request) = self.pending.front() else {
                break;
            };
            sender.enqueue(request.clone(), block, now_ms)?;
            self.pending.pop_front();
        }
        Ok(())
    }
    pub(super) fn shutdown(&mut self, protocol: Option<&crate::operations::protocol::ProtocolLog>) {
        for request in self.pending.drain(..) {
            if let Some(protocol) = protocol {
                protocol.arq_outcome(request.identity, "Cancelled before ARQ admission");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::registry::RegistrationReply,
        l3::{
            address::{AddressInfo, AddressValue},
            registration::{AcceptOptions, RegistrationResponse},
        },
        link::rach::MobileIdentity,
    };
    fn reply(identity: u64) -> RegistrationReply {
        RegistrationReply {
            identity: MobileIdentity::Min34(identity),
            message: RegistrationResponse::Accept,
            options: AcceptOptions {
                directory_address: Some(AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: AddressValue::Tbcd("1".repeat(32)),
                }),
                ..Default::default()
            },
        }
    }
    #[test]
    fn startup_retains_a_fragmented_reply_with_its_original_deadline_then_admits_at_the_boundary() {
        let config = Config::from_yaml(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/two-phones.yaml"
        )))
        .unwrap();
        let policy = arq_reuse_policy(&config).unwrap();
        let mut sender = super::super::super::spach_arq::Sender::after_restart(1, policy).unwrap();
        let mut plain = ReplyQueue::try_new(1).unwrap();
        let mut backlog = Backlog::default();
        let reply = reply(1);
        queue_registration_reply(&mut plain, &mut sender, &mut backlog, reply.clone(), 0).unwrap();
        assert_eq!(backlog.pending.len(), 1);
        assert_eq!(plain.pending_replies(), 0);
        assert_eq!(
            backlog.pending[0].deadline_ms,
            super::super::super::timers::SPACH_MS
        );
        backlog
            .advance(
                &mut sender,
                policy.quiet_blocks() - 1,
                policy.quiet_ms - 1,
                |_| false,
                None,
            )
            .unwrap();
        assert_eq!(sender.pending(), 0);
        assert_eq!(backlog.pending.len(), 1);
        backlog
            .advance(
                &mut sender,
                policy.quiet_blocks(),
                policy.quiet_ms,
                |_| false,
                None,
            )
            .unwrap();
        assert!(backlog.pending.is_empty());
        assert_eq!(sender.pending(), 1);
        let frame = sender
            .prepare(policy.quiet_blocks(), policy.quiet_ms)
            .unwrap()
            .unwrap();
        assert_eq!(frame.identity, reply.identity);
        assert!(sender.pop_event().is_none());
    }
    #[test]
    fn invalid_replacement_retains_waiting_reply_and_single_frame_replacement_cancels_it() {
        let mut sender = super::super::super::spach_arq::Sender::try_new(1).unwrap();
        let mut plain = ReplyQueue::try_new(1).unwrap();
        let mut backlog = Backlog::default();
        queue_registration_reply(&mut plain, &mut sender, &mut backlog, reply(1), 0).unwrap();
        let bytes = backlog.pending[0].message.clone();
        let mut invalid = reply(1);
        invalid.options.directory_address.as_mut().unwrap().address =
            AddressValue::Tbcd("1".repeat(600));
        assert!(
            queue_registration_reply(&mut plain, &mut sender, &mut backlog, invalid, 1).is_err()
        );
        assert_eq!(backlog.pending.len(), 1);
        assert_eq!(backlog.pending[0].message, bytes);
        assert_eq!(
            backlog.pending[0].deadline_ms,
            super::super::super::timers::SPACH_MS
        );
        let mut single = reply(1);
        single.options = AcceptOptions::default();
        queue_registration_reply(&mut plain, &mut sender, &mut backlog, single, 2).unwrap();
        assert!(backlog.pending.is_empty());
        assert_eq!(plain.pending_replies(), 1);
    }
    #[test]
    fn waiting_replies_expire_or_cancel_without_receipts_or_false_sender_completion() {
        let mut sender = super::super::super::spach_arq::Sender::try_new(1).unwrap();
        let mut plain = ReplyQueue::try_new(1).unwrap();
        let mut backlog = Backlog::default();
        for identity in 1..=MAX_REPLIES as u64 {
            queue_registration_reply(&mut plain, &mut sender, &mut backlog, reply(identity), 0)
                .unwrap();
        }
        assert!(
            queue_registration_reply(
                &mut plain,
                &mut sender,
                &mut backlog,
                reply(MAX_REPLIES as u64 + 1),
                0
            )
            .is_err()
        );
        queue_registration_reply(&mut plain, &mut sender, &mut backlog, reply(1), 0).unwrap();
        assert_eq!(backlog.pending.len(), MAX_REPLIES);
        let protocol = crate::operations::protocol::ProtocolLog::default();
        backlog
            .advance(
                &mut sender,
                0,
                super::super::super::timers::SPACH_MS,
                |_| false,
                Some(&protocol),
            )
            .unwrap();
        assert!(backlog.pending.is_empty());
        assert_eq!(sender.pending(), 0);
        assert!(sender.pop_event().is_none());
        queue_registration_reply(
            &mut plain,
            &mut sender,
            &mut backlog,
            reply(1),
            super::super::super::timers::SPACH_MS,
        )
        .unwrap();
        backlog
            .advance(
                &mut sender,
                1,
                super::super::super::timers::SPACH_MS + 1,
                |_| true,
                Some(&protocol),
            )
            .unwrap();
        assert!(backlog.pending.is_empty());
        assert_eq!(sender.pending(), 0);
        let air_messages = protocol.messages();
        assert!(
            air_messages
                .iter()
                .any(|message| message.status == "Expired before ARQ admission")
        );
        assert!(
            air_messages
                .iter()
                .any(|message| message.status == "Cancelled before ARQ admission")
        );
        assert!(
            air_messages
                .iter()
                .all(|message| message.name == "ARQ transaction")
        );
    }
}
