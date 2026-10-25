use super::*;
use crate::{
    core::call_session::CallState,
    link::traffic_order::OrderState,
    runtime::sms::{RelayChannel, SmsRuntime, TrafficPeer},
};
impl VoiceRuntime {
    pub(crate) fn retains_received_signaling(
        &self,
        identity: crate::link::rach::MobileIdentity,
    ) -> bool {
        self.sms_inbox
            .iter()
            .any(|(peer, _)| peer.identity == identity)
            || self.calls.iter().any(|active| {
                active.legs.iter().any(|leg| {
                    leg.assignment.subscriber.identity == identity && leg.rx.has_pending_signaling()
                })
            })
    }
    fn sms_peers(&self) -> Vec<TrafficPeer> {
        self.calls
            .iter()
            .filter(|active| {
                !matches!(
                    active.session.state(),
                    CallState::Releasing(_) | CallState::Stopped(_)
                )
            })
            .flat_map(|active| {
                active
                    .legs
                    .present()
                    .into_iter()
                    .flatten()
                    .map(move |party| (active, party))
            })
            .map(|(active, party)| {
                let leg = &active.legs[index(party)];
                TrafficPeer {
                    identity: leg.assignment.subscriber.identity,
                    call_id: active.session.handle().id().value(),
                    ready: active.session.state() == CallState::Connected
                        && matches!(
                            active.session.measurement_state(party),
                            Some(
                                OrderState::Acknowledged
                                    | OrderState::TimedOut
                                    | OrderState::Cancelled
                            )
                        ),
                }
            })
            .collect()
    }
    pub(crate) fn exchange_sms(
        &mut self,
        sms: &mut SmsRuntime,
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<(), Box<dyn Error>> {
        for handoff in self.sms_handoffs.drain(..) {
            sms.traffic_handed_off(handoff.token, handoff.observed_ms, now_ms)?;
        }
        for token in self.sms_interrupted.drain(..) {
            sms.traffic_interrupted(token, now_ms)?;
        }
        for completion in self.sms_completed.drain(..) {
            sms.traffic_transmitted_at(completion.token, completion.completed_ms, now_ms)?;
        }
        for (peer, relay) in self.sms_inbox.drain(..) {
            if let Err(error) = sms.receive_traffic(peer, &relay, now_ms, unix_ms) {
                tracing::warn!(%error, identity=%peer.identity, "traffic SMS rejected");
            }
        }
        // A relay response decoded before Release must reach the queue before bearer teardown cancels its attempt.
        sms.sync_traffic(self.sms_peers(), now_ms)?;
        for active in &mut self.calls {
            for party in active.legs.present().into_iter().flatten() {
                let leg = &mut active.legs[index(party)];
                if leg
                    .sms_facch
                    .as_ref()
                    .is_some_and(|(_, offer)| !sms.traffic_valid(offer.token))
                {
                    let (handle, _) = leg.sms_facch.as_ref().expect("obsolete transfer exists");
                    if !leg.tx.cancel_facch(handle)?.in_flight {
                        leg.sms_facch = None;
                    }
                }
                if leg
                    .sms_sacch
                    .as_ref()
                    .is_some_and(|(_, offer)| !sms.traffic_valid(offer.token))
                {
                    let (handle, _) = leg.sms_sacch.as_ref().expect("obsolete transfer exists");
                    if !leg.tx.cancel_sacch(handle)?.in_flight {
                        leg.sms_sacch = None;
                    }
                }
                if active.session.bearer_state(party) != BearerState::Established {
                    continue;
                }
                let peer = TrafficPeer {
                    identity: leg.assignment.subscriber.identity,
                    call_id: active.session.handle().id().value(),
                    ready: false,
                };
                if leg.sms_facch.is_none()
                    && leg.order.is_none()
                    && leg.response.is_none()
                    && leg.flash.is_none()
                    && leg.service.is_none()
                    && let Some(offer) = sms.traffic_offer(peer, RelayChannel::Facch)
                    && let Some(handle) = super::queue::facch(&mut leg.tx, &offer.message)?
                {
                    sms.traffic_queued(offer.token)?;
                    leg.sms_facch = Some((handle, offer));
                }
                if leg.sms_sacch.is_none()
                    && let Some(offer) = sms.traffic_offer(peer, RelayChannel::Sacch)
                    && let Some(handle) = super::queue::sacch(&mut leg.tx, &offer.message)?
                {
                    sms.traffic_queued(offer.token)?;
                    leg.sms_sacch = Some((handle, offer));
                }
            }
        }
        Ok(())
    }
}
