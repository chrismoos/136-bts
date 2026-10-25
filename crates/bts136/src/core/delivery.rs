mod auxiliary;
use super::{
    messaging::{LocalMessaging, MessagingError},
    relay_attempt::{AttemptError, AttemptState, RelayAttempt},
    relay_exchange::RelayOutcome,
    relay_ids::{IdentifierError, RelayIdentifiers},
    store::SmsId,
};
use crate::{
    l3::{legacy_traffic::TrafficRelay, relay::addressing::AddressedRelay},
    link::rach::MobileIdentity,
};
pub use auxiliary::{
    AuxiliaryError, AuxiliaryPacket, AuxiliaryReply, AuxiliaryState, AuxiliaryTransport,
};
use std::{error::Error, fmt, sync::Arc};

#[derive(Debug, Clone, Copy)]
pub(crate) enum DeliveryTransport {
    Control,
    Traffic,
}
#[derive(Debug, Clone, Copy)]
pub struct DeliveryPolicy {
    pub response_timeout_ms: u64,
    pub reuse_guard_ms: u64,
}
#[derive(Debug)]
pub enum DeliveryError {
    Policy,
    Messaging(MessagingError),
    Identifier(IdentifierError),
    Attempt(AttemptError),
    Auxiliary(AuxiliaryError),
    UnknownRecipient,
    RecipientOffline,
    Busy,
    StaleTransmission,
    ClockRegression,
}
impl fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SMS delivery failed: {self:?}")
    }
}
impl Error for DeliveryError {}
#[derive(Debug, Clone)]
pub struct PreparedDelivery {
    recipient: MobileIdentity,
    id: SmsId,
    relay: AddressedRelay,
    transmission: Arc<()>,
}
impl PreparedDelivery {
    pub fn recipient(&self) -> MobileIdentity {
        self.recipient
    }
    pub fn id(&self) -> SmsId {
        self.id
    }
    pub fn relay(&self) -> &AddressedRelay {
        &self.relay
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryEnded {
    pub recipient: MobileIdentity,
    pub id: SmsId,
    pub state: AttemptState,
}
#[derive(Debug)]
struct ActiveDelivery {
    id: SmsId,
    transmission: Arc<()>,
    attempt: RelayAttempt,
    submitted: bool,
}
#[derive(Debug)]
struct RecipientDelivery {
    identity: MobileIdentity,
    identifiers: RelayIdentifiers,
    active: Option<ActiveDelivery>,
    auxiliary: Vec<auxiliary::AuxiliaryTransfer>,
}
#[derive(Debug)]
pub struct SmsDelivery {
    core: LocalMessaging,
    policy: DeliveryPolicy,
    recipients: Vec<RecipientDelivery>,
    last_monotonic_ms: u64,
}
impl SmsDelivery {
    pub fn after_restart(
        core: LocalMessaging,
        policy: DeliveryPolicy,
        monotonic_ms: u64,
    ) -> Result<Self, DeliveryError> {
        if policy.response_timeout_ms == 0 || policy.reuse_guard_ms == 0 {
            return Err(DeliveryError::Policy);
        }
        let recipients = core
            .registry()
            .subscribers()
            .iter()
            .map(|subscriber| {
                let identity = subscriber.provisioning.identity;
                Ok(RecipientDelivery {
                    identity,
                    identifiers: RelayIdentifiers::after_restart(
                        identity,
                        policy.reuse_guard_ms,
                        monotonic_ms,
                    )
                    .map_err(DeliveryError::Identifier)?,
                    active: None,
                    auxiliary: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, DeliveryError>>()?;
        Ok(Self {
            core,
            policy,
            recipients,
            last_monotonic_ms: monotonic_ms,
        })
    }
    pub fn core(&self) -> &LocalMessaging {
        &self.core
    }
    pub fn core_mut(&mut self) -> &mut LocalMessaging {
        &mut self.core
    }
    pub(crate) fn busy(&self, identity: MobileIdentity) -> bool {
        self.recipients
            .iter()
            .any(|recipient| recipient.identity == identity && recipient.active.is_some())
    }
    pub(crate) fn sync_subscribers(&mut self, now_ms: u64) -> Result<(), DeliveryError> {
        for subscriber in self.core.registry().subscribers() {
            let identity = subscriber.provisioning.identity;
            if !self
                .recipients
                .iter()
                .any(|recipient| recipient.identity == identity)
            {
                self.recipients.push(RecipientDelivery {
                    identity,
                    identifiers: RelayIdentifiers::after_restart(
                        identity,
                        self.policy.reuse_guard_ms,
                        now_ms,
                    )
                    .map_err(DeliveryError::Identifier)?,
                    active: None,
                    auxiliary: Vec::new(),
                });
            }
        }
        Ok(())
    }
    pub(crate) fn operation_measurements(
        &self,
        id_value: i64,
    ) -> Vec<crate::operations::proto::Metric> {
        use crate::operations::metric;
        let Some(active) = self
            .recipients
            .iter()
            .filter_map(|recipient| recipient.active.as_ref())
            .find(|active| active.id.value() == id_value)
        else {
            return Vec::new();
        };
        let mut metrics = vec![metric(
            "Relay attempt",
            format!("{:?}", active.attempt.state()),
            "",
        )];
        if let crate::l3::relay::RelayMessage::Data { transaction_id, .. } =
            active.attempt.relay().message
        {
            metrics.push(metric("Transaction ID", transaction_id, ""));
        }
        if let AttemptState::AwaitingResponse {
            deadline_monotonic_ms,
        } = active.attempt.state()
        {
            metrics.push(metric(
                "Response deadline",
                deadline_monotonic_ms,
                "ms since startup",
            ));
        }
        metrics
    }
    pub fn prepare(
        &mut self,
        recipient: MobileIdentity,
        monotonic_ms: u64,
    ) -> Result<Option<PreparedDelivery>, DeliveryError> {
        self.prepare_for(recipient, monotonic_ms, DeliveryTransport::Control)
    }
    pub(crate) fn prepare_for(
        &mut self,
        recipient: MobileIdentity,
        monotonic_ms: u64,
        transport: DeliveryTransport,
    ) -> Result<Option<PreparedDelivery>, DeliveryError> {
        self.observe_clock(monotonic_ms)?;
        let peer = self
            .recipients
            .iter_mut()
            .find(|peer| peer.identity == recipient)
            .ok_or(DeliveryError::UnknownRecipient)?;
        if peer.active.is_some() {
            return Err(DeliveryError::Busy);
        }
        let Some(offer) = self
            .core
            .deliveries_for(recipient)
            .map_err(DeliveryError::Messaging)?
            .into_iter()
            .find(|offer| match transport {
                DeliveryTransport::Control => offer
                    .relay(0)
                    .encode(crate::l3::relay::Direction::Forward)
                    .is_ok(),
                DeliveryTransport::Traffic => TrafficRelay::from_addressed(&offer.relay(0))
                    .is_ok_and(|relay| {
                        crate::link::traffic::encode(
                            &crate::l3::legacy_traffic::TrafficMessage::Relay(relay),
                        )
                        .is_ok()
                    }),
            })
        else {
            return Ok(None);
        };
        let id = offer.queued.id;
        let attempt = RelayAttempt::try_new(offer, &mut peer.identifiers, monotonic_ms)
            .map_err(DeliveryError::Attempt)?;
        let transmission = Arc::new(());
        let prepared = PreparedDelivery {
            recipient,
            id,
            relay: attempt.relay(),
            transmission: transmission.clone(),
        };
        peer.active = Some(ActiveDelivery {
            id,
            transmission,
            attempt,
            submitted: false,
        });
        Ok(Some(prepared))
    }
    pub(crate) fn is_active(&self, prepared: &PreparedDelivery) -> bool {
        self.recipients.iter().any(|peer| {
            peer.identity == prepared.recipient
                && peer
                    .active
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(&active.transmission, &prepared.transmission))
        })
    }
    /// Call only after every frame of this exact message has been submitted.
    pub(crate) fn submitted(&mut self, prepared: &PreparedDelivery) -> bool {
        let Some(active) = self
            .recipients
            .iter_mut()
            .filter(|peer| peer.identity == prepared.recipient)
            .filter_map(|peer| peer.active.as_mut())
            .find(|active| Arc::ptr_eq(&active.transmission, &prepared.transmission))
        else {
            return false;
        };
        active.submitted = true;
        true
    }
    pub(crate) fn cancel_prepared(
        &mut self,
        prepared: &PreparedDelivery,
        now_ms: u64,
    ) -> Result<bool, DeliveryError> {
        if !self.is_active(prepared) {
            return Ok(false);
        }
        self.cancel(prepared.recipient, now_ms)?;
        Ok(true)
    }
    /// The callback must identify the exact prepared transmission whose final burst completed.
    pub fn transmitted(
        &mut self,
        prepared: &PreparedDelivery,
        monotonic_ms: u64,
    ) -> Result<(), DeliveryError> {
        self.transmitted_with_timeout(prepared, monotonic_ms, self.policy.response_timeout_ms)
    }
    pub(crate) fn transmitted_with_timeout(
        &mut self,
        prepared: &PreparedDelivery,
        monotonic_ms: u64,
        response_timeout_ms: u64,
    ) -> Result<(), DeliveryError> {
        self.transmitted_with_timeout_at(prepared, monotonic_ms, monotonic_ms, response_timeout_ms)
    }
    pub fn transmitted_at(
        &mut self,
        prepared: &PreparedDelivery,
        completed_monotonic_ms: u64,
        observed_monotonic_ms: u64,
    ) -> Result<(), DeliveryError> {
        self.transmitted_with_timeout_at(
            prepared,
            completed_monotonic_ms,
            observed_monotonic_ms,
            self.policy.response_timeout_ms,
        )
    }
    pub(crate) fn transmitted_with_timeout_at(
        &mut self,
        prepared: &PreparedDelivery,
        completed_monotonic_ms: u64,
        observed_monotonic_ms: u64,
        response_timeout_ms: u64,
    ) -> Result<(), DeliveryError> {
        if response_timeout_ms == 0 {
            return Err(DeliveryError::Policy);
        }
        self.observe_clock(observed_monotonic_ms)?;
        let is_registered = self.core.registry().subscribers().iter().any(|subscriber| {
            subscriber.provisioning.identity == prepared.recipient
                && subscriber.registration.is_some()
        });
        let peer = self
            .recipients
            .iter_mut()
            .find(|peer| peer.identity == prepared.recipient)
            .ok_or(DeliveryError::StaleTransmission)?;
        let active = peer
            .active
            .as_mut()
            .ok_or(DeliveryError::StaleTransmission)?;
        if !Arc::ptr_eq(&active.transmission, &prepared.transmission) {
            return Err(DeliveryError::StaleTransmission);
        }
        if !is_registered {
            active
                .attempt
                .cancel(&mut peer.identifiers, observed_monotonic_ms)
                .map_err(DeliveryError::Attempt)?;
            peer.active = None;
            return Err(DeliveryError::RecipientOffline);
        }
        active
            .attempt
            .transmitted_at(
                completed_monotonic_ms,
                observed_monotonic_ms,
                response_timeout_ms,
            )
            .map_err(DeliveryError::Attempt)?;
        active
            .attempt
            .advance(&mut peer.identifiers, observed_monotonic_ms)
            .map_err(DeliveryError::Attempt)?;
        Ok(())
    }
    pub fn receive(
        &mut self,
        recipient: MobileIdentity,
        bytes: &[u8],
        monotonic_ms: u64,
    ) -> Result<RelayOutcome, DeliveryError> {
        self.observe_clock(monotonic_ms)?;
        let peer = self
            .recipients
            .iter_mut()
            .find(|peer| peer.identity == recipient)
            .ok_or(DeliveryError::UnknownRecipient)?;
        let active = peer
            .active
            .as_mut()
            .ok_or(DeliveryError::StaleTransmission)?;
        let outcome = if active.submitted {
            active.attempt.receive_submitted(
                self.core.store_mut(),
                &mut peer.identifiers,
                recipient,
                bytes,
                monotonic_ms,
            )
        } else {
            active.attempt.receive(
                self.core.store_mut(),
                &mut peer.identifiers,
                recipient,
                bytes,
                monotonic_ms,
            )
        }
        .map_err(DeliveryError::Attempt)?;
        peer.active = None;
        Ok(outcome)
    }
    pub fn advance(
        &mut self,
        recipient: MobileIdentity,
        monotonic_ms: u64,
    ) -> Result<Option<DeliveryEnded>, DeliveryError> {
        self.observe_clock(monotonic_ms)?;
        let is_registered = self.core.registry().subscribers().iter().any(|subscriber| {
            subscriber.provisioning.identity == recipient && subscriber.registration.is_some()
        });
        let peer = self
            .recipients
            .iter_mut()
            .find(|peer| peer.identity == recipient)
            .ok_or(DeliveryError::UnknownRecipient)?;
        let Some(active) = peer.active.as_mut() else {
            return Ok(None);
        };
        let state = active
            .attempt
            .advance(&mut peer.identifiers, monotonic_ms)
            .map_err(DeliveryError::Attempt)?;
        if !is_registered
            && matches!(
                state,
                AttemptState::Prepared | AttemptState::AwaitingResponse { .. }
            )
        {
            active
                .attempt
                .cancel(&mut peer.identifiers, monotonic_ms)
                .map_err(DeliveryError::Attempt)?;
        }
        let state = active.attempt.state();
        if !matches!(
            state,
            AttemptState::Expired | AttemptState::Cancelled | AttemptState::Completed(_)
        ) {
            return Ok(None);
        }
        let ended = DeliveryEnded {
            recipient,
            id: active.id,
            state,
        };
        peer.active = None;
        Ok(Some(ended))
    }
    pub fn cancel(
        &mut self,
        recipient: MobileIdentity,
        monotonic_ms: u64,
    ) -> Result<Option<DeliveryEnded>, DeliveryError> {
        self.observe_clock(monotonic_ms)?;
        let peer = self
            .recipients
            .iter_mut()
            .find(|peer| peer.identity == recipient)
            .ok_or(DeliveryError::UnknownRecipient)?;
        let Some(active) = peer.active.as_mut() else {
            return Ok(None);
        };
        if matches!(
            active.attempt.state(),
            AttemptState::Prepared | AttemptState::AwaitingResponse { .. }
        ) {
            active
                .attempt
                .cancel(&mut peer.identifiers, monotonic_ms)
                .map_err(DeliveryError::Attempt)?;
        }
        let ended = DeliveryEnded {
            recipient,
            id: active.id,
            state: active.attempt.state(),
        };
        peer.active = None;
        Ok(Some(ended))
    }
    fn observe_clock(&mut self, monotonic_ms: u64) -> Result<(), DeliveryError> {
        if monotonic_ms < self.last_monotonic_ms {
            return Err(DeliveryError::ClockRegression);
        }
        self.last_monotonic_ms = monotonic_ms;
        for peer in &mut self.recipients {
            peer.auxiliary
                .retain(|transfer| transfer.retained_at(monotonic_ms));
        }
        Ok(())
    }
}
