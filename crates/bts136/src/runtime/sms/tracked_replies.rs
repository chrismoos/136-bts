use super::*;
use crate::{
    core::{
        registry::RegistrationHandle,
        store::{incoming_journal, outgoing_journal},
    },
    l3::legacy_traffic::TrafficRelay,
    radio::carrier::{Carrier, QueueFence},
};
use std::sync::{Arc, Mutex};
const TSAR_ID_MASK: u8 = 7;
/// Fencing proves source drainage, including canceled replies that were never transmitted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplyStatus {
    pub submitted_monotonic_ms: Option<u64>,
    pub completed_monotonic_ms: Option<u64>,
    pub fenced: bool,
}
#[derive(Debug, Clone)]
pub struct ReplyPacket {
    identity: MobileIdentity,
    bytes: Vec<u8>,
    status: Arc<Mutex<ReplyStatus>>,
}
impl PartialEq for ReplyPacket {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.status, &other.status)
    }
}
impl Eq for ReplyPacket {}
impl ReplyPacket {
    pub fn identity(&self) -> MobileIdentity {
        self.identity
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn status(&self) -> Result<ReplyStatus, Failure> {
        self.status
            .lock()
            .map(|status| *status)
            .map_err(|_| Failure::Wire)
    }
}
#[derive(Clone, PartialEq, Eq)]
enum Group {
    Incoming(incoming_journal::JournalHandle),
    Outgoing(outgoing_journal::JournalHandle),
}
struct Job {
    packet: ReplyPacket,
    route: Route,
    registration: RegistrationHandle,
    deadline_ms: u64,
    traffic_token: Option<TrafficToken>,
    fence: Option<QueueFence>,
}
#[derive(Default)]
pub(super) struct ReplyQueues {
    jobs: Vec<Job>,
    latest: Vec<(Group, ReplyPacket)>,
}
impl SmsRuntime {
    pub(super) fn latest_incoming_reply(
        &self,
        handle: &incoming_journal::JournalHandle,
    ) -> Option<&ReplyPacket> {
        self.tracked_replies
            .latest
            .iter()
            .find_map(|(group, packet)| {
                matches!(group, Group::Incoming(owner) if owner == handle).then_some(packet)
            })
    }
    pub(super) fn forget_incoming_reply(&mut self, handle: &incoming_journal::JournalHandle) {
        self.tracked_replies
            .latest
            .retain(|(group, _)| !matches!(group, Group::Incoming(owner) if owner == handle));
    }

    pub(super) fn latest_outgoing_reply(
        &self,
        handle: &outgoing_journal::JournalHandle,
    ) -> Option<&ReplyPacket> {
        self.tracked_replies
            .latest
            .iter()
            .find_map(|(group, packet)| {
                matches!(group, Group::Outgoing(owner) if owner == handle).then_some(packet)
            })
    }
    pub(super) fn reply_work_pending(&self, peer: MobileIdentity) -> bool {
        self.tracked_replies
            .jobs
            .iter()
            .any(|job| job.packet.identity == peer)
    }
    pub(super) fn forget_outgoing_reply(&mut self, handle: &outgoing_journal::JournalHandle) {
        self.tracked_replies
            .latest
            .retain(|(group, _)| !matches!(group, Group::Outgoing(owner) if owner == handle));
    }
    fn reply_group(
        &self,
        identity: MobileIdentity,
        inbound: &RelayMessage,
    ) -> Result<Option<Group>, Failure> {
        let RelayMessage::Data {
            protocol: relay::HigherLayerProtocol::Segmented(relay::Teleservice::Sms),
            payload,
            ..
        } = inbound
        else {
            return Ok(None);
        };
        let Some(header) = payload.first() else {
            return Ok(None);
        };
        if super::outgoing_sms::is_response(payload) {
            return Ok(self
                .outgoing_reply_handle(identity, payload)
                .map(Group::Outgoing));
        }
        Ok(self
            .core()
            .store()
            .incoming_bank_journals(
                identity,
                relay::HigherLayerProtocol::Segmented(relay::Teleservice::Sms),
            )
            .map_err(Failure::Store)?
            .into_iter()
            .find(|journal| journal.binding.transaction_id == header & TSAR_ID_MASK)
            .map(|journal| Group::Incoming(journal.handle)))
    }
    pub(super) fn enqueue_response(
        &mut self,
        identity: MobileIdentity,
        inbound: &RelayMessage,
        bytes: Vec<u8>,
        route: Route,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        let Some(group) = self.reply_group(identity, inbound)? else {
            return self.enqueue_route(identity, bytes, None, route, now_ms);
        };
        let Some(registration) = self.core().registry().registration_handle(identity) else {
            return self.enqueue_route(identity, bytes, None, route, now_ms);
        };
        if self.tracked_replies.jobs.len() >= MAX_PENDING
            || self.tracked_replies.latest.len() >= MAX_SUBMISSION_HISTORY * 2
                && !self
                    .tracked_replies
                    .latest
                    .iter()
                    .any(|(old, _)| old == &group)
        {
            return Err(Failure::QueueFull);
        }
        let packet = ReplyPacket {
            identity,
            bytes: bytes.clone(),
            status: Arc::new(Mutex::new(ReplyStatus::default())),
        };
        match route {
            Route::Control => self.enqueue_owned(
                identity,
                bytes,
                ForwardOwner::TrackedReply(packet.clone()),
                now_ms,
            )?,
            Route::Traffic(call_id) => self.enqueue_traffic_owned(
                identity,
                call_id,
                TrafficRelay::from_control(&bytes, Direction::Forward)
                    .map_err(|_| Failure::Wire)?,
                ForwardOwner::TrackedReply(packet.clone()),
                now_ms,
            )?,
        }
        let (deadline_ms, traffic_token) = match route {
            Route::Control => (self.pending.back().ok_or(Failure::Wire)?.deadline_ms, None),
            Route::Traffic(_) => {
                let pending = self.traffic.pending.back().ok_or(Failure::Wire)?;
                (pending.deadline_ms, Some(pending.offer.token))
            }
        };
        self.tracked_replies.jobs.push(Job {
            packet: packet.clone(),
            route,
            registration,
            deadline_ms,
            traffic_token,
            fence: None,
        });
        if let Some((_, last)) = self
            .tracked_replies
            .latest
            .iter_mut()
            .find(|(old, _)| old == &group)
        {
            *last = packet;
        } else {
            self.tracked_replies.latest.push((group, packet));
        }
        Ok(())
    }
    pub(super) fn tracked_reply_active(&self, packet: &ReplyPacket, now_ms: u64) -> bool {
        self.tracked_replies
            .jobs
            .iter()
            .find(|job| job.packet == *packet)
            .is_some_and(|job| {
                now_ms < job.deadline_ms
                    && self
                        .core()
                        .registry()
                        .registration_current(&job.registration)
                    && match job.route {
                        Route::Control => !self
                            .traffic
                            .peers
                            .iter()
                            .any(|peer| peer.identity == packet.identity),
                        Route::Traffic(call_id) => self.traffic.peers.iter().any(|peer| {
                            peer.identity == packet.identity && peer.call_id == call_id
                        }),
                    }
            })
    }
    pub(super) fn reply_handed_off(
        &mut self,
        packet: &ReplyPacket,
        handed_off_ms: u64,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        if handed_off_ms > now_ms {
            return Err(Failure::Clock);
        }
        if self
            .tracked_replies
            .jobs
            .iter()
            .any(|job| job.packet == *packet)
        {
            packet
                .status
                .lock()
                .map_err(|_| Failure::Wire)?
                .submitted_monotonic_ms
                .get_or_insert(handed_off_ms);
        }
        Ok(())
    }
    pub fn reply_transmitted(
        &mut self,
        packet: &ReplyPacket,
        completed_ms: u64,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        if completed_ms > now_ms {
            return Err(Failure::Clock);
        }
        if !self
            .tracked_replies
            .jobs
            .iter()
            .any(|job| job.packet == *packet)
        {
            return Ok(());
        }
        let mut status = packet.status.lock().map_err(|_| Failure::Wire)?;
        let submitted = status.submitted_monotonic_ms.ok_or(Failure::Wire)?;
        if completed_ms < submitted {
            return Err(Failure::Clock);
        }
        status.completed_monotonic_ms.get_or_insert(completed_ms);
        Ok(())
    }
    pub(super) fn advance_tracked_replies(&mut self, now_ms: u64) {
        let inactive: Vec<_> = self
            .tracked_replies
            .jobs
            .iter()
            .filter(|job| !self.tracked_reply_active(&job.packet, now_ms))
            .map(|job| job.packet.clone())
            .collect();
        let before = self.pending.len();
        self.pending.retain(|pending| {
            pending
                .owner
                .tracked_reply()
                .is_none_or(|packet| !inactive.contains(packet))
        });
        if self.pending.len() != before {
            self.selected = None;
        }
        self.traffic.pending.retain(|pending| {
            pending
                .owner
                .tracked_reply()
                .is_none_or(|packet| !inactive.contains(packet))
        });
    }
    pub(crate) fn settle_reply_fences<T>(
        &mut self,
        carrier: &Carrier<T>,
        voice: &super::super::voice::VoiceRuntime,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        self.advance_tracked_replies(now_ms);
        let mut index = 0;
        while index < self.tracked_replies.jobs.len() {
            let job = &self.tracked_replies.jobs[index];
            if self
                .pending
                .iter()
                .any(|pending| pending.owner.tracked_reply() == Some(&job.packet))
                || self
                    .control_arq
                    .iter()
                    .any(|pending| pending.message.owner.tracked_reply() == Some(&job.packet))
                || self
                    .traffic
                    .pending
                    .iter()
                    .any(|pending| pending.owner.tracked_reply() == Some(&job.packet))
                || job
                    .traffic_token
                    .is_some_and(|token| voice.retains_sms(token))
            {
                index += 1;
                continue;
            }
            let job = &mut self.tracked_replies.jobs[index];
            let fence = job.fence.get_or_insert_with(|| carrier.queue_fence());
            if !carrier
                .fence_complete(fence)
                .map_err(|error| Failure::Fence(Box::new(error)))?
            {
                index += 1;
                continue;
            }
            job.packet.status.lock().map_err(|_| Failure::Wire)?.fenced = true;
            self.tracked_replies.jobs.remove(index);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
