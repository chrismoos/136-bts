use super::*;
use crate::radio::carrier::{Carrier, QueueFence};

struct Anchor {
    unix_ms: i64,
    deadline_ms: u64,
    guard_ms: u64,
}
pub(super) struct Candidate {
    reply: Option<ReplyPacket>,
    fence: QueueFence,
    anchor: Option<Anchor>,
}
pub(super) fn eligible(stream: &Stream) -> bool {
    matches!(
        stream.sender.state(),
        SenderState::Complete(crate::link::tsar::Completion::PeerReceived)
    ) || closed(stream.sender.state()) && stream.sender.layer3_submissions() == 0
}
impl SmsRuntime {
    /// Dispatch available RX before checking retirement so buffered messages cannot be overtaken by ID reuse.
    pub(crate) fn settle_outgoing_retirements<T>(
        &mut self,
        carrier: &Carrier<T>,
        now_ms: u64,
        receive_pending: impl Fn(MobileIdentity) -> bool,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        self.settle_outgoing_jobs()?;
        let unix_ms = self.outgoing_time(now_ms)?;
        let mut index = 0;
        while index < self.outgoing_sms.streams.len() {
            let stream = &self.outgoing_sms.streams[index];
            let handle = stream.snapshot.handle.clone();
            let peer = stream.snapshot.peer;
            let recovered = stream.recovered;
            let latest = self.latest_outgoing_reply(&handle);
            let ready = eligible(stream)
                && !self
                    .outgoing_sms
                    .jobs
                    .iter()
                    .any(|job| job.handle == handle)
                && !self.reply_work_pending(peer)
                && latest
                    .map(ReplyPacket::status)
                    .transpose()?
                    .is_none_or(|status| status.fenced);
            if !ready {
                self.outgoing_sms.streams[index].retirement = None;
                index += 1;
                continue;
            }
            let changed = stream
                .retirement
                .as_ref()
                .is_none_or(|candidate| candidate.reply.as_ref() != latest);
            if changed {
                let fence = carrier.queue_fence();
                if recovered && fence.scheduled_end_tick().is_none() && !carrier.stop_acknowledged()
                {
                    index += 1;
                    continue;
                }
                let candidate = Candidate {
                    reply: latest.cloned(),
                    fence,
                    anchor: None,
                };
                self.outgoing_sms.streams[index].retirement = Some(candidate);
            }
            let candidate = self.outgoing_sms.streams[index]
                .retirement
                .as_mut()
                .ok_or(Failure::Wire)?;
            if !carrier
                .fence_complete(&candidate.fence)
                .map_err(|error| Failure::Fence(Box::new(error)))?
            {
                index += 1;
                continue;
            }
            if candidate.anchor.is_none() {
                // Recovered journals also wait for bounded RF samples accepted before restart.
                let recovery_guard_ms = if recovered {
                    crate::runtime::timers::SUPERFRAME_MS
                } else {
                    0
                };
                let guard_ms = self
                    .submission_retry_guard_ms
                    .checked_add(recovery_guard_ms)
                    .ok_or(Failure::Clock)?;
                candidate.anchor = Some(Anchor {
                    unix_ms,
                    deadline_ms: now_ms.checked_add(guard_ms).ok_or(Failure::Clock)?,
                    guard_ms,
                });
            }
            let anchor = candidate.anchor.as_ref().ok_or(Failure::Wire)?;
            if now_ms < anchor.deadline_ms || receive_pending(peer) {
                index += 1;
                continue;
            }
            let anchor_unix_ms = anchor.unix_ms;
            let guard_ms = anchor.guard_ms;
            let revision = self.outgoing_sms.streams[index].snapshot.revision();
            // Persist the original source-fence anchor only after its entire guard and receive drainage are proved.
            self.core_mut()
                .store_mut()
                .retire_outgoing_journal_fenced(&handle, revision, anchor_unix_ms, guard_ms)
                .map_err(Failure::Store)?;
            tracing::info!(
                ?peer,
                guard_ms,
                "Outgoing segmented SMS transaction retired"
            );
            self.forget_outgoing_reply(&handle);
            self.outgoing_sms.streams.remove(index);
        }
        Ok(())
    }
}
