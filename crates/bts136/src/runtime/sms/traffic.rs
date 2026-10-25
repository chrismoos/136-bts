use super::*;
use crate::l3::legacy_traffic::{TrafficMessage, TrafficRelay};

// TIA-136-133-E §4.7.2 starts the mobile-terminated response interval after the final transmitted bit.
pub(crate) const SACCH_RESPONSE_MS: u64 = 1200;
const TRAFFIC_INTERVAL_MS: u64 = 20;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayChannel {
    Facch,
    Sacch,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct TrafficPeer {
    pub identity: MobileIdentity,
    pub call_id: u64,
    pub ready: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TrafficToken {
    pub(super) identity: MobileIdentity,
    pub(super) call_id: u64,
    generation: u64,
}
#[derive(Debug, Clone)]
pub(crate) struct TrafficOffer {
    pub token: TrafficToken,
    pub message: TrafficMessage,
    pub channel: RelayChannel,
}
pub(super) struct PendingTraffic {
    pub offer: TrafficOffer,
    pub(super) owner: ForwardOwner,
    pub(super) deadline_ms: u64,
    pub(super) queued: bool,
    pub(super) durable_after: Option<u64>,
}
#[derive(Clone)]
pub(super) struct TrafficAttempt {
    pub(super) identity: MobileIdentity,
    pub(super) call_id: u64,
    delivery: PreparedDelivery,
}
#[derive(Default)]
pub(super) struct TrafficSms {
    pub peers: Vec<TrafficPeer>,
    pub pending: VecDeque<PendingTraffic>,
    pub attempts: Vec<TrafficAttempt>,
}
impl SmsRuntime {
    pub(super) fn enqueue_route(
        &mut self,
        identity: MobileIdentity,
        bytes: Vec<u8>,
        delivery: Option<PreparedDelivery>,
        route: Route,
        now_ms: u64,
    ) -> Result<(), Failure> {
        if self.pending.len() + self.traffic.pending.len() + self.control_arq.len() >= MAX_PENDING {
            return Err(Failure::QueueFull);
        }
        let Route::Traffic(call_id) = route else {
            return self.enqueue(identity, bytes, delivery, now_ms);
        };
        let relay =
            TrafficRelay::from_control(&bytes, Direction::Forward).map_err(|_| Failure::Wire)?;
        self.enqueue_traffic(identity, call_id, relay, delivery, now_ms)
    }
    pub(super) fn enqueue_traffic(
        &mut self,
        identity: MobileIdentity,
        call_id: u64,
        relay: TrafficRelay,
        delivery: Option<PreparedDelivery>,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.enqueue_traffic_owned(
            identity,
            call_id,
            relay,
            ForwardOwner::from(delivery),
            now_ms,
        )
    }
    pub(super) fn enqueue_traffic_owned(
        &mut self,
        identity: MobileIdentity,
        call_id: u64,
        relay: TrafficRelay,
        owner: ForwardOwner,
        now_ms: u64,
    ) -> Result<(), Failure> {
        if let Some(packet) = owner.tracked_reply()
            && (packet.identity() != identity
                || TrafficRelay::from_control(packet.bytes(), Direction::Forward)
                    .map_err(|_| Failure::Wire)?
                    != relay)
        {
            return Err(Failure::Wire);
        }
        if let Some(packet) = owner.auxiliary()
            && (packet.recipient() != identity
                || packet.transport() != AuxiliaryTransport::Traffic
                || TrafficRelay::from_addressed(packet.relay()).map_err(|_| Failure::Wire)?
                    != relay)
        {
            return Err(Failure::Wire);
        }
        if let Some(prepared) = owner.delivery()
            && (prepared.recipient() != identity
                || TrafficRelay::from_addressed(prepared.relay()).map_err(|_| Failure::Wire)?
                    != relay)
        {
            return Err(Failure::Wire);
        }
        if self.pending.len() + self.traffic.pending.len() + self.control_arq.len() >= MAX_PENDING {
            return Err(Failure::QueueFull);
        }
        let message = TrafficMessage::Relay(relay);
        let words = crate::link::traffic::encode(&message)
            .map_err(|_| Failure::Wire)?
            .len();
        let channel = if !owner.is_reply() {
            RelayChannel::Sacch
        } else {
            RelayChannel::Facch
        };
        let budget_ms = match channel {
            // TIA-136-133-E §4.5 uses sms_orig for mobile-originated R-DATA, independently of MT relay timing.
            RelayChannel::Facch => super::super::timers::SMS_ORIGINATION_MS,
            RelayChannel::Sacch => {
                (words as u64 + 2) * crate::phy::sacch::WORD_SLOTS as u64 * TRAFFIC_INTERVAL_MS
                    + super::super::timers::SUPERFRAME_MS
            }
        };
        let token = TrafficToken {
            identity,
            call_id,
            generation: self.sequence,
        };
        self.sequence = self.sequence.checked_add(1).ok_or(Failure::Clock)?;
        if let Some(prepared) = owner.delivery() {
            self.traffic.attempts.push(TrafficAttempt {
                identity,
                call_id,
                delivery: prepared.clone(),
            });
        }
        self.traffic.pending.push_back(PendingTraffic {
            offer: TrafficOffer {
                token,
                message,
                channel,
            },
            owner,
            deadline_ms: now_ms.checked_add(budget_ms).ok_or(Failure::Clock)?,
            queued: false,
            durable_after: None,
        });
        Ok(())
    }
    pub(crate) fn sync_traffic(
        &mut self,
        peers: Vec<TrafficPeer>,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        let previous = std::mem::replace(&mut self.traffic.peers, peers);
        let abandoned: Vec<_> = self
            .traffic
            .attempts
            .iter()
            .filter(|attempt| {
                !self.traffic.peers.iter().any(|peer| {
                    peer.identity == attempt.identity && peer.call_id == attempt.call_id
                })
            })
            .cloned()
            .collect();
        for attempt in abandoned {
            self.delivery
                .cancel_prepared(&attempt.delivery, now_ms)
                .map_err(Failure::Delivery)?;
            self.traffic.attempts.retain(|peer| {
                peer.identity != attempt.identity || peer.call_id != attempt.call_id
            });
        }
        self.traffic.pending.retain(|pending| {
            self.traffic.peers.iter().any(|peer| {
                peer.identity == pending.offer.token.identity
                    && peer.call_id == pending.offer.token.call_id
            })
        });
        let entered: Vec<_> = self
            .traffic
            .peers
            .iter()
            .filter(|peer| {
                !previous
                    .iter()
                    .any(|old| old.identity == peer.identity && old.call_id == peer.call_id)
            })
            .copied()
            .collect();
        for peer in entered {
            // A phone on a DTC cannot complete a pending paging-channel SMS exchange.
            match self.delivery.cancel(peer.identity, now_ms) {
                Ok(_) => {}
                Err(DeliveryError::UnknownRecipient) => {
                    tracing::debug!(identity=%peer.identity, "Voice caller has no SMS delivery context");
                }
                Err(error) => return Err(Failure::Delivery(error)),
            }
            let mut replies = self.detach_control_arq(peer.identity);
            self.pending.retain(|pending| {
                if pending.identity != peer.identity {
                    return true;
                }
                if matches!(pending.owner, ForwardOwner::Reply) {
                    replies.push(pending.bytes.clone());
                }
                false
            });
            self.selected = None;
            for reply in replies {
                self.enqueue_route(
                    peer.identity,
                    reply,
                    None,
                    Route::Traffic(peer.call_id),
                    now_ms,
                )?;
            }
            self.next_sweep_ms = now_ms;
        }
        Ok(())
    }
    pub(crate) fn receive_traffic(
        &mut self,
        peer: TrafficPeer,
        relay: &TrafficRelay,
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<(), Box<dyn Error>> {
        self.observe(now_ms)?;
        if !self
            .traffic
            .peers
            .iter()
            .any(|current| current.identity == peer.identity && current.call_id == peer.call_id)
        {
            return Err(Failure::Wire.into());
        }
        relay.validate()?;
        self.receive_fields(
            peer.identity,
            &relay.message,
            &relay.optional_octets,
            Route::Traffic(peer.call_id),
            now_ms,
            unix_ms,
        )
    }

    pub(crate) fn traffic_offer(
        &self,
        peer: TrafficPeer,
        channel: RelayChannel,
    ) -> Option<TrafficOffer> {
        if self.traffic.pending.iter().any(|pending| {
            pending.queued
                && pending.offer.token.identity == peer.identity
                && pending.offer.channel == channel
        }) {
            return None;
        }
        self.traffic
            .pending
            .iter()
            .find(|pending| {
                !pending.queued
                    && self.durable(pending.durable_after)
                    && pending.offer.token.identity == peer.identity
                    && pending.offer.token.call_id == peer.call_id
                    && pending.offer.channel == channel
            })
            .map(|pending| pending.offer.clone())
    }
    pub(crate) fn traffic_valid(&self, token: TrafficToken) -> bool {
        self.traffic
            .pending
            .iter()
            .any(|pending| pending.offer.token == token)
    }
    pub(crate) fn traffic_queued(&mut self, token: TrafficToken) -> Result<(), Failure> {
        let pending = self
            .traffic
            .pending
            .iter_mut()
            .find(|pending| pending.offer.token == token && !pending.queued)
            .ok_or(Failure::Wire)?;
        pending.queued = true;
        Ok(())
    }
    pub(crate) fn traffic_interrupted(
        &mut self,
        token: TrafficToken,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        if let Some(index) = self
            .traffic
            .pending
            .iter()
            .position(|pending| pending.offer.token == token)
        {
            let pending = self
                .traffic
                .pending
                .remove(index)
                .expect("matched transfer exists");
            match pending.owner {
                ForwardOwner::Reply | ForwardOwner::TrackedReply(_) => {}
                ForwardOwner::Auxiliary(packet) => self.request_auxiliary_cancel(&packet),
                ForwardOwner::Delivery(delivery) => {
                    if self
                        .delivery
                        .cancel_prepared(&delivery, now_ms)
                        .map_err(Failure::Delivery)?
                    {
                        self.traffic
                            .attempts
                            .retain(|peer| peer.identity != token.identity);
                        self.next_sweep_ms = now_ms;
                    }
                }
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn traffic_transmitted(
        &mut self,
        token: TrafficToken,
        now_ms: u64,
    ) -> Result<bool, Failure> {
        self.traffic_transmitted_at(token, now_ms, now_ms)
    }
    pub(crate) fn traffic_transmitted_at(
        &mut self,
        token: TrafficToken,
        completed_ms: u64,
        now_ms: u64,
    ) -> Result<bool, Failure> {
        if completed_ms > now_ms {
            return Err(Failure::Clock);
        }
        self.observe(now_ms)?;
        let Some(index) = self
            .traffic
            .pending
            .iter()
            .position(|pending| pending.offer.token == token && pending.queued)
        else {
            return Ok(false);
        };
        if completed_ms >= self.traffic.pending[index].deadline_ms {
            self.traffic_interrupted(token, now_ms)?;
            return Ok(false);
        }
        let owner = self.traffic.pending[index].owner.clone();
        match owner {
            ForwardOwner::Reply => {}
            ForwardOwner::TrackedReply(packet) => {
                self.reply_transmitted(&packet, completed_ms, now_ms)?
            }
            ForwardOwner::Auxiliary(packet) => {
                self.auxiliary_completed(&packet, completed_ms, now_ms)?;
            }
            ForwardOwner::Delivery(delivery) => {
                if !self.delivery.is_active(&delivery) {
                    tracing::debug!(identity=%token.identity, "Ignoring traffic SMS completion for retired delivery attempt");
                    self.traffic.pending.remove(index);
                    return Ok(false);
                }
                match self.delivery.transmitted_with_timeout_at(
                    &delivery,
                    completed_ms,
                    now_ms,
                    SACCH_RESPONSE_MS,
                ) {
                    Ok(()) => {}
                    Err(DeliveryError::RecipientOffline) => {
                        self.traffic.pending.remove(index);
                        self.traffic
                            .attempts
                            .retain(|peer| peer.identity != token.identity);
                        tracing::info!(identity=%token.identity, "traffic SMS recipient went offline before completion");
                        return Ok(false);
                    }
                    Err(error) => return Err(Failure::Delivery(error)),
                }
            }
        }
        self.traffic.pending.remove(index);
        Ok(true)
    }
    pub(super) fn expire_traffic(&mut self, now_ms: u64) -> Result<(), Failure> {
        let expired: Vec<_> = self
            .traffic
            .pending
            .iter()
            .filter(|pending| now_ms >= pending.deadline_ms)
            .map(|pending| pending.offer.token)
            .collect();
        for token in expired {
            self.traffic_interrupted(token, now_ms)?;
            tracing::warn!(identity=%token.identity, call_id=token.call_id, "traffic SMS transmission expired");
        }
        Ok(())
    }
}
