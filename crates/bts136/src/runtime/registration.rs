use crate::{
    core::registry::RegistrationReply,
    link::spach::{Channel, HeaderFlags, SingleMessage, SpachError},
    phy::control::{INFORMATION_BITS, LogicalChannel, SharedFeedback},
    radio::{
        carrier::{self, Carrier},
        timed_tx::Receipt,
    },
    scheduler::broadcast::{self, BroadcastSuperframe},
};
use std::{
    collections::{TryReserveError, VecDeque},
    error::Error as StdError,
    fmt,
};
#[derive(Debug)]
pub enum Error {
    Limits,
    QueueFull,
    Allocation(TryReserveError),
    Framing(SpachError),
    Encoding(crate::l3::registration::OptionsError),
    Broadcast(broadcast::Error),
    Carrier(carrier::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "registration reply transmission failed: {self:?}")
    }
}
impl Error {
    pub(crate) fn is_late(&self) -> bool {
        matches!(self, Self::Carrier(error) if error.is_late())
    }
}
impl StdError for Error {}
struct Pending {
    reply: RegistrationReply,
    frames: VecDeque<[bool; INFORMATION_BITS]>,
    started: bool,
    deadline_ms: u64,
}
pub struct ReplyQueue {
    pending: VecDeque<Pending>,
    max_replies: usize,
}
impl ReplyQueue {
    pub fn try_new(max_replies: usize) -> Result<Self, Error> {
        if max_replies == 0 {
            return Err(Error::Limits);
        }
        let mut pending = VecDeque::new();
        pending
            .try_reserve_exact(max_replies)
            .map_err(Error::Allocation)?;
        Ok(Self {
            pending,
            max_replies,
        })
    }
    pub(crate) fn in_progress(&self) -> bool {
        self.pending.front().is_some_and(|pending| pending.started)
    }
    pub fn pending_replies(&self) -> usize {
        self.pending.len()
    }
    pub fn enqueue(&mut self, reply: RegistrationReply, deadline_ms: u64) -> Result<(), Error> {
        self.enqueue_inner(reply, deadline_ms, false)
    }
    pub(crate) fn enqueue_replacing(
        &mut self,
        reply: RegistrationReply,
        deadline_ms: u64,
    ) -> Result<(), Error> {
        self.enqueue_inner(reply, deadline_ms, true)
    }
    fn enqueue_inner(
        &mut self,
        reply: RegistrationReply,
        deadline_ms: u64,
        replacing: bool,
    ) -> Result<(), Error> {
        let can_replace = replacing
            && self
                .pending
                .iter()
                .any(|pending| pending.reply.identity == reply.identity && !pending.started);
        if self.pending.len() == self.max_replies && !can_replace {
            return Err(Error::QueueFull);
        }
        let frames = SingleMessage {
            channel: Channel::Arch,
            flags: HeaderFlags::default(),
            identity: reply.identity,
            message: reply.encode().map_err(Error::Encoding)?,
        }
        .encode_fragmented()
        .map_err(Error::Framing)?;
        if replacing {
            self.cancel_queued(reply.identity);
        }
        self.pending.push_back(Pending {
            reply,
            frames: frames.into(),
            started: false,
            deadline_ms,
        });
        Ok(())
    }
    pub(crate) fn cancel_queued(&mut self, identity: crate::link::rach::MobileIdentity) {
        self.pending
            .retain(|pending| pending.reply.identity != identity || pending.started);
    }
    pub(crate) fn frame_count(&mut self, now_ms: u64) -> usize {
        self.expire(now_ms);
        self.pending
            .front()
            .map_or(0, |pending| pending.frames.len())
    }
    fn expire(&mut self, monotonic_ms: u64) {
        while self
            .pending
            .front()
            .is_some_and(|pending| monotonic_ms >= pending.deadline_ms)
        {
            let expired = self.pending.pop_front().expect("expired head exists");
            tracing::warn!(identity=%expired.reply.identity,"registration reply expired before completion");
        }
    }
    pub fn submit_next<T: From<Option<RegistrationReply>>>(
        &mut self,
        carrier: &mut Carrier<T>,
        snapshot: &BroadcastSuperframe,
        physical_slot_index: u64,
        feedback: &SharedFeedback,
        monotonic_ms: u64,
    ) -> Result<Option<Receipt>, Error> {
        self.expire(monotonic_ms);
        let slot = physical_slot_index % broadcast::PHYSICAL_SLOTS;
        if snapshot.channel(slot).map_err(Error::Broadcast)? != Some(LogicalChannel::Spach) {
            return Ok(None);
        }
        let Some(pending) = self.pending.front() else {
            return Ok(None);
        };
        let control = snapshot
            .encode_slot(slot, feedback, pending.frames.front())
            .map_err(Error::Broadcast)?
            .expect("SPACH channel has a control slot");
        let receipt = carrier
            .submit_slot(
                physical_slot_index,
                &control.bits,
                T::from((pending.frames.len() == 1).then(|| pending.reply.clone())),
            )
            .map_err(|rejected| Error::Carrier(rejected.error))?;
        let pending = self.pending.front_mut().expect("submitted reply exists");
        pending.started = true;
        pending.frames.pop_front();
        if pending.frames.is_empty() {
            self.pending.pop_front();
        }
        Ok(Some(receipt))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        l3::{
            address::{AddressInfo, AddressValue},
            registration::{AcceptOptions, RegistrationResponse},
        },
        link::{rach::MobileIdentity, spach::SpachReceiver},
    };
    #[test]
    fn registration_queue_retains_fragmented_reply_and_expiry_drops_partial_without_completion() {
        let reply = RegistrationReply {
            identity: MobileIdentity::Min34(1),
            message: RegistrationResponse::Accept,
            options: AcceptOptions {
                pfc: Some(0),
                directory_address: Some(AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: AddressValue::Tbcd("12345678901234567890".into()),
                }),
                ..Default::default()
            },
        };
        let mut queue = ReplyQueue::try_new(1).unwrap();
        queue.enqueue(reply.clone(), 100).unwrap();
        assert!(!queue.in_progress());
        let pending = queue.pending.front_mut().unwrap();
        assert!(pending.frames.len() > 1);
        let mut receiver = SpachReceiver::default();
        assert!(
            receiver
                .push(&pending.frames.pop_front().unwrap())
                .unwrap()
                .is_none()
        );
        pending.started = true;
        assert!(queue.in_progress());
        let mut recovered = None;
        for frame in &queue.pending.front().unwrap().frames {
            recovered = receiver.push(frame).unwrap();
        }
        assert_eq!(recovered.unwrap().message, reply.encode().unwrap());
        let config = crate::config::Config::from_yaml(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/two-phones.yaml"
        )))
        .unwrap();
        let mut carrier: Carrier<Option<RegistrationReply>> = Carrier::try_new(
            crate::radio::duplex::Duplex::try_new(
                crate::radio::PreparedRadio::open(&config)
                    .unwrap()
                    .into_streams(
                        crate::radio::timed_tx::Limits {
                            max_queued_blocks: 4,
                            max_block_samples: 5184,
                        },
                        crate::radio::receiver::Limits {
                            max_block_samples: 64,
                            timeout_us: 1000,
                        },
                    )
                    .unwrap(),
                16,
            )
            .unwrap(),
            1,
        )
        .unwrap();
        let snapshot = BroadcastSuperframe::try_new(
            crate::l3::designation::AssignedSlots::Full14,
            &super::super::serving::messages(&config),
            crate::link::broadcast::BroadcastChanges {
                fbcch: false,
                ebcch: false,
            },
        )
        .unwrap();
        let feedback = SharedFeedback {
            leading: [false; 12],
            trailing: [false; 10],
        };
        assert!(
            queue
                .submit_next(&mut carrier, &snapshot, 0, &feedback, 100)
                .unwrap()
                .is_none()
        );
        assert!(!queue.in_progress());
        assert_eq!(queue.pending_replies(), 0);
    }
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    use crate::{
        l3::registration::{RegistrationResponse, RejectCause},
        link::rach::MobileIdentity,
    };
    #[test]
    fn unsent_replies_are_replaced_but_started_atomic_replies_keep_their_ownership() {
        let reply = RegistrationReply {
            identity: MobileIdentity::Min34(1),
            message: RegistrationResponse::Accept,
            options: Default::default(),
        };
        let mut queue = ReplyQueue::try_new(1).unwrap();
        queue.enqueue(reply.clone(), 100).unwrap();
        let replacement = RegistrationReply {
            message: RegistrationResponse::Reject {
                cause: RejectCause::Congestion,
            },
            ..reply.clone()
        };
        queue.enqueue_replacing(replacement.clone(), 200).unwrap();
        assert_eq!(queue.pending_replies(), 1);
        assert_eq!(queue.pending[0].reply, replacement);
        queue.pending[0].started = true;
        let frames = queue.pending[0].frames.clone();
        assert!(matches!(
            queue.enqueue_replacing(reply, 300),
            Err(Error::QueueFull)
        ));
        queue.cancel_queued(MobileIdentity::Min34(1));
        assert_eq!(queue.pending[0].reply, replacement);
        assert_eq!(queue.pending[0].frames, frames);
    }
}
