use super::*;
use crate::radio::carrier::{Carrier, QueueFence};

struct Anchor {
    unix_ms: i64,
    deadline_ms: u64,
    guard_ms: u64,
}
pub(super) struct Candidate {
    response: crate::core::store::incoming_journal::ResponseClosure,
    reply: Option<ReplyPacket>,
    fence: QueueFence,
    anchor: Option<Anchor>,
}
impl SmsRuntime {
    /// Dispatch available RX before checking retirement so buffered messages cannot be overtaken by ID reuse.
    pub(crate) fn settle_incoming_retirements<T>(
        &mut self,
        carrier: &Carrier<T>,
        now_ms: u64,
        receive_pending: impl Fn(MobileIdentity) -> bool,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        self.settle_incoming_responses(now_ms, 0)?;
        let unix_ms = self
            .incoming_clock(now_ms)
            .map_err(|error| Failure::Incoming(Box::new(error)))?
            .now_unix_ms;
        let mut index = 0;
        while index < self.incoming_sms.tracked.len() {
            let stream = &self.incoming_sms.tracked[index];
            let handle = stream.journal.handle.clone();
            let peer = stream.journal.binding.peer;
            let recovered = stream.recovered;
            let latest = self.latest_incoming_reply(&handle);
            let ready = stream.complete
                && !stream.pending_application
                && !self.incoming_response_pending(&handle)
                && !self.reply_work_pending(peer)
                && latest
                    .map(ReplyPacket::status)
                    .transpose()?
                    .is_none_or(|status| status.fenced);
            if !ready {
                self.incoming_sms.tracked[index].retirement = None;
                index += 1;
                continue;
            }
            let waiting = stream.retirement.as_ref().is_some_and(|candidate| {
                candidate.reply.as_ref() == latest
                    && candidate
                        .anchor
                        .as_ref()
                        .is_some_and(|anchor| now_ms < anchor.deadline_ms)
            });
            if waiting {
                index += 1;
                continue;
            }
            let response = self
                .core()
                .store()
                .incoming_completion_response(&handle)
                .map_err(Failure::Store)?;
            if response.is_none() {
                self.incoming_sms.tracked[index].retirement = None;
                index += 1;
                continue;
            }
            if response.is_some_and(|proof| proof.settled_at_unix_ms > unix_ms) {
                return Err(Failure::Clock);
            }
            let changed = stream.retirement.as_ref().is_none_or(|candidate| {
                candidate.reply.as_ref() != latest || Some(candidate.response) != response
            });
            if changed {
                let fence = carrier.queue_fence();
                if recovered && fence.scheduled_end_tick().is_none() && !carrier.stop_acknowledged()
                {
                    index += 1;
                    continue;
                }
                let candidate = Candidate {
                    response: response.ok_or(Failure::Wire)?,
                    reply: latest.cloned(),
                    fence,
                    anchor: None,
                };
                self.incoming_sms.tracked[index].retirement = Some(candidate);
            }
            let candidate = self.incoming_sms.tracked[index]
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
            let revision = self.incoming_sms.tracked[index].journal.revision;
            // Persist the original source-fence anchor only after its entire guard and receive drainage are proved.
            self.core_mut()
                .store_mut()
                .retire_incoming_journal_fenced(&handle, revision, anchor_unix_ms, guard_ms)
                .map_err(Failure::Store)?;
            tracing::info!(
                ?peer,
                guard_ms,
                "Incoming segmented SMS transaction retired"
            );
            self.forget_incoming_reply(&handle);
            self.incoming_sms.tracked.remove(index);
        }
        Ok(())
    }
}
