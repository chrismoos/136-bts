mod access_lifetime;
mod auxiliary;
pub use auxiliary::AuxiliaryOutcome;
mod forward_owner;
mod incoming_responses;
mod incoming_sms;
mod ordinary_submission;
mod outgoing_sms;
mod tracked_replies;
use forward_owner::ForwardOwner;
pub use incoming_sms::IncomingSmsError;
pub use tracked_replies::{ReplyPacket, ReplyStatus};
mod control_arq;
mod traffic;
pub mod tsar_ingress;
pub mod tsar_recovery;
pub mod tsar_submission;
use crate::{
    core::{
        delivery::{
            AuxiliaryPacket, AuxiliaryState, AuxiliaryTransport, DeliveryError, DeliveryPolicy,
            DeliveryTransport, PreparedDelivery, SmsDelivery,
        },
        messaging::LocalMessaging,
        relay_ids::IdentifierError,
    },
    l3::{
        relay::{self, Direction, RelayMessage},
        sms::{self, SmsMessage},
    },
    link::{
        rach::{MessageSet, MobileIdentity},
        spach::{Channel, HeaderFlags, SingleMessage},
    },
    phy::control::INFORMATION_BITS,
    scheduler::paging::PagingAllocation,
};
pub(crate) use control_arq::{ArqMessage, ArqOutcome};
use std::{collections::VecDeque, error::Error, fmt};
pub(crate) use traffic::{RelayChannel, TrafficOffer, TrafficPeer, TrafficToken};
const MAX_PENDING: usize = 32;
const MAX_SUBMISSION_HISTORY: usize = 256;
const RDATA: u8 = 9;
const RACCEPT: u8 = 10;
const RREJECT: u8 = 11;
const SPACH_CONFIRMATION: u8 = 15;
const SERIAL_NUMBER: u8 = 13;
const INVALID_MESSAGE_CAUSE: u8 = 95;
const TOO_LONG_CAUSE: u8 = 3;
const UNSUPPORTED_PROTOCOL_CAUSE: u8 = 4;
const RETRY_INTERVAL_MS: u64 = super::timers::SUPERFRAME_MS;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Control,
    Traffic(u64),
}
struct Pending {
    bytes: Vec<u8>,
    identity: MobileIdentity,
    frames: VecDeque<[bool; INFORMATION_BITS]>,
    owner: ForwardOwner,
    first_phase: u8,
    deadline_ms: u64,
    notification: Option<[bool; INFORMATION_BITS]>,
    confirmed: bool,
    started: bool,
    notified: bool,
    last_notification_sf: Option<u64>,
    generation: u64,
    durable_after: Option<u64>,
}
struct Duplicate {
    registration: Option<crate::core::registry::RegistrationHandle>,
    identity: MobileIdentity,
    message: RelayMessage,
    optional_octets: Vec<u8>,
    reply: Vec<u8>,
    until_ms: u64,
}
#[derive(Debug)]
pub enum Failure {
    QueueFull,
    Store(crate::core::store::StoreError),
    Incoming(Box<IncomingSmsError>),
    Wire,
    Clock,
    Fence(Box<crate::radio::carrier::Error>),
    Delivery(DeliveryError),
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SMS radio procedure failed: {self:?}")
    }
}
impl Error for Failure {}
enum Selection {
    Message { superframe: u64, notification: bool },
    Repeat(usize),
}
struct Repeat {
    superframe: u64,
    phase: u8,
    information: [bool; INFORMATION_BITS],
}
pub enum Completion {
    None,
    Notification {
        identity: MobileIdentity,
        generation: u64,
    },
    Delivery(PreparedDelivery),
    Auxiliary(AuxiliaryPacket),
    Reply(ReplyPacket),
    Message {
        identity: MobileIdentity,
        bytes: Vec<u8>,
    },
}
pub struct SmsRuntime {
    submission_retry_guard_ms: u64,
    tracked_replies: tracked_replies::ReplyQueues,
    outgoing_sms: outgoing_sms::OutgoingSms,
    incoming_sms: incoming_sms::IncomingSms,
    incoming_response_jobs: Vec<incoming_responses::IncomingResponseJob>,
    next_incoming_response_scan_ms: u64,
    auxiliary: auxiliary::AuxiliaryQueues,
    protocol: Option<std::sync::Arc<crate::operations::protocol::ProtocolLog>>,
    delivery: SmsDelivery,
    traffic: traffic::TrafficSms,
    attempt_counts: VecDeque<(i64, u64)>,
    pending: VecDeque<Pending>,
    control_arq: Vec<control_arq::Transfer>,
    duplicates: VecDeque<Duplicate>,
    allocation: PagingAllocation,
    next_sweep_ms: u64,
    sequence: u64,
    last_ms: u64,
    selected: Option<Selection>,
    repeats: Vec<Repeat>,
}
impl SmsRuntime {
    pub fn try_new(
        core: LocalMessaging,
        allocation: PagingAllocation,
        _session_key: String,
    ) -> Result<Self, Failure> {
        Ok(Self {
            submission_retry_guard_ms: access_lifetime::AccessLifetime::maximum().retry_guard_ms,
            tracked_replies: tracked_replies::ReplyQueues::default(),
            outgoing_sms: outgoing_sms::OutgoingSms::default(),
            incoming_sms: incoming_sms::IncomingSms::default(),
            incoming_response_jobs: Vec::new(),
            next_incoming_response_scan_ms: 0,
            auxiliary: auxiliary::AuxiliaryQueues::default(),
            protocol: None,
            traffic: traffic::TrafficSms::default(),
            delivery: SmsDelivery::after_restart(
                core,
                DeliveryPolicy {
                    response_timeout_ms: super::timers::SPACH_MS,
                    reuse_guard_ms: super::timers::SMS_ORIGINATION_MS,
                },
                0,
            )
            .map_err(Failure::Delivery)?,
            attempt_counts: VecDeque::new(),
            pending: VecDeque::new(),
            control_arq: Vec::new(),
            duplicates: VecDeque::new(),
            allocation,
            next_sweep_ms: 0,
            sequence: 0,
            last_ms: 0,
            selected: None,
            repeats: Vec::new(),
        })
    }
    pub(crate) fn set_access_parameters(
        &mut self,
        parameters: &crate::l3::broadcast::AccessParameters,
        maximum_rx_age_ms: u64,
    ) -> Result<(), Failure> {
        if self.last_ms != 0 || !self.duplicates.is_empty() {
            return Err(Failure::Wire);
        }
        let lifetime =
            access_lifetime::AccessLifetime::from_parameters(parameters, maximum_rx_age_ms)
                .map_err(|_| Failure::Wire)?;
        self.submission_retry_guard_ms = lifetime.retry_guard_ms;
        tracing::debug!(
            maximum_access_ms = lifetime.maximum_access_ms,
            retry_guard_ms = lifetime.retry_guard_ms,
            "SMS access retry limits configured"
        );
        Ok(())
    }
    pub(crate) fn set_protocol_log(
        &mut self,
        log: std::sync::Arc<crate::operations::protocol::ProtocolLog>,
    ) {
        self.protocol = Some(log);
    }
    fn observe(&mut self, now_ms: u64) -> Result<(), Failure> {
        if now_ms < self.last_ms {
            return Err(Failure::Clock);
        }
        self.last_ms = now_ms;
        Ok(())
    }
    pub fn core_mut(&mut self) -> &mut LocalMessaging {
        self.delivery.core_mut()
    }
    pub fn core(&self) -> &LocalMessaging {
        self.delivery.core()
    }
    pub(crate) fn subscriber_busy(&self, identity: MobileIdentity) -> bool {
        self.delivery.busy(identity)
            || self.arq_busy(identity)
            || self
                .traffic
                .pending
                .iter()
                .any(|pending| pending.offer.token.identity == identity)
            || self
                .pending
                .iter()
                .any(|pending| pending.identity == identity)
    }
    pub(crate) fn power_down(
        &mut self,
        handle: &crate::core::registry::RegistrationHandle,
        now_ms: u64,
    ) -> Result<bool, Failure> {
        self.observe(now_ms)?;
        if !self.core().registry().registration_current(handle) {
            return Ok(false);
        }
        self.delivery
            .sync_subscribers(now_ms)
            .map_err(Failure::Delivery)?;
        if !self.core_mut().power_down(handle) {
            return Ok(false);
        }
        self.cancel_offline(handle.identity(), now_ms)?;
        tracing::info!(identity=%handle.identity(), "Phone powered down on the traffic channel. Registration and volatile delivery work cleared");
        Ok(true)
    }
    pub(crate) fn cancel_offline(
        &mut self,
        identity: MobileIdentity,
        now_ms: u64,
    ) -> Result<bool, Failure> {
        self.observe(now_ms)?;
        if !self
            .core()
            .registry()
            .subscribers()
            .iter()
            .any(|subscriber| {
                subscriber.provisioning.identity == identity && subscriber.registration.is_none()
            })
        {
            return Ok(false);
        }
        self.delivery
            .sync_subscribers(now_ms)
            .map_err(Failure::Delivery)?;
        self.cancel_control_arq(identity, now_ms)?;
        self.delivery
            .cancel(identity, now_ms)
            .map_err(Failure::Delivery)?;
        self.pending.retain(|pending| pending.identity != identity);
        self.traffic
            .pending
            .retain(|pending| pending.offer.token.identity != identity);
        self.traffic
            .attempts
            .retain(|peer| peer.identity != identity);
        self.traffic.peers.retain(|peer| peer.identity != identity);
        self.selected = None;
        Ok(true)
    }
    pub(crate) fn sync_subscribers(&mut self, now_ms: u64) -> Result<(), Failure> {
        self.delivery
            .sync_subscribers(now_ms)
            .map_err(Failure::Delivery)
    }
    #[cfg(test)]
    pub(crate) fn operation_messages(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::operations::proto::Sms>, crate::core::store::StoreError> {
        let mut messages = self.core().store().operation_messages(limit)?;
        self.enrich_operation_messages(&mut messages);
        Ok(messages)
    }
    pub(crate) fn operation_live_messages(
        &self,
        limit: usize,
    ) -> Vec<crate::operations::proto::Sms> {
        let mut ids: Vec<_> = self
            .attempt_counts
            .iter()
            .map(|(id, _)| *id)
            .chain(self.outgoing_message_ids())
            .chain(self.pending.iter().filter_map(|pending| {
                pending
                    .owner
                    .delivery()
                    .map(|delivery| delivery.id().value())
            }))
            .chain(self.traffic.pending.iter().filter_map(|pending| {
                pending
                    .owner
                    .delivery()
                    .map(|delivery| delivery.id().value())
            }))
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        ids.dedup();
        ids.truncate(limit);
        let mut messages: Vec<_> = ids
            .into_iter()
            .map(|id| crate::operations::proto::Sms {
                id,
                ..Default::default()
            })
            .collect();
        self.enrich_operation_messages(&mut messages);
        messages
    }
    fn enrich_operation_messages(&self, messages: &mut [crate::operations::proto::Sms]) {
        use crate::operations::metric;
        for message in messages {
            message
                .measurements
                .extend(self.delivery.operation_measurements(message.id));
            message
                .measurements
                .extend(self.outgoing_measurements(message.id));
            message
                .measurements
                .extend(self.arq_measurements(message.id));
            if let Some((_, count)) = self.attempt_counts.iter().find(|(id, _)| *id == message.id) {
                message
                    .measurements
                    .push(metric("Attempts since startup", count, ""));
            }
            if let Some(pending) = self.pending.iter().find(|pending| {
                pending
                    .owner
                    .delivery()
                    .is_some_and(|delivery| delivery.id().value() == message.id)
            }) {
                message.measurements.extend([
                    metric("Forward fragments remaining", pending.frames.len(), ""),
                    metric("Notification transmitted", pending.notified, ""),
                    metric("Notification confirmed", pending.confirmed, ""),
                    metric("Paging phase", pending.first_phase, ""),
                    metric(
                        "Procedure deadline",
                        pending.deadline_ms,
                        "ms since startup",
                    ),
                ]);
            }
            if let Some(pending) = self.traffic.pending.iter().find(|pending| {
                pending
                    .owner
                    .delivery()
                    .is_some_and(|delivery| delivery.id().value() == message.id)
            }) {
                message.measurements.extend([
                    metric("Delivery channel", "SACCH", ""),
                    metric("Call ID", pending.offer.token.call_id, ""),
                ]);
            }
        }
    }
    fn enqueue(
        &mut self,
        identity: MobileIdentity,
        bytes: Vec<u8>,
        delivery: Option<PreparedDelivery>,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.enqueue_owned(identity, bytes, ForwardOwner::from(delivery), now_ms)
    }
    fn enqueue_owned(
        &mut self,
        identity: MobileIdentity,
        bytes: Vec<u8>,
        owner: ForwardOwner,
        now_ms: u64,
    ) -> Result<(), Failure> {
        if owner
            .tracked_reply()
            .is_some_and(|packet| packet.identity() != identity || packet.bytes() != bytes)
        {
            return Err(Failure::Wire);
        }
        if let Some(packet) = owner.auxiliary()
            && (packet.recipient() != identity
                || packet.transport() != AuxiliaryTransport::Control
                || packet
                    .relay()
                    .encode(Direction::Forward)
                    .map_err(|_| Failure::Wire)?
                    != bytes)
        {
            return Err(Failure::Wire);
        }
        if let Some(prepared) = owner.delivery()
            && (prepared.recipient() != identity
                || prepared
                    .relay()
                    .encode(Direction::Forward)
                    .map_err(|_| Failure::Wire)?
                    != bytes)
        {
            return Err(Failure::Wire);
        }
        if self.pending.len() + self.traffic.pending.len() + self.control_arq.len() >= MAX_PENDING {
            return Err(Failure::QueueFull);
        }
        let first_phase = self.allocation.phase(identity).map_err(|_| Failure::Wire)?;
        let frames = SingleMessage {
            channel: Channel::Smsch,
            flags: HeaderFlags::default(),
            identity,
            message: bytes.clone(),
        }
        .encode_fragmented()
        .map_err(|_| Failure::Wire)?
        .into();
        let notification = if !owner.is_reply() {
            let frames = SingleMessage {
                channel: Channel::Pch,
                flags: HeaderFlags::default(),
                identity,
                message: crate::l3::notification::sms_notification(),
            }
            .encode_fragmented()
            .map_err(|_| Failure::Wire)?;
            let [frame] = frames.as_slice() else {
                return Err(Failure::Wire);
            };
            Some(*frame)
        } else {
            None
        };
        let confirmed = notification.is_none();
        let generation = self.sequence;
        self.sequence = self.sequence.checked_add(1).ok_or(Failure::Clock)?;
        self.pending.push_back(Pending {
            bytes,
            identity,
            frames,
            owner,
            first_phase,
            deadline_ms: now_ms
                .checked_add(super::timers::SPACH_MS)
                .ok_or(Failure::Clock)?,
            notification,
            confirmed,
            started: false,
            notified: false,
            last_notification_sf: None,
            generation,
            durable_after: None,
        });
        Ok(())
    }
    pub fn receive(
        &mut self,
        set: &MessageSet,
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<(), Box<dyn Error>> {
        self.observe(now_ms)?;
        if super::access::classify(set)? != super::access::Procedure::Sms {
            return Err(Failure::Wire.into());
        }
        for bytes in &set.messages {
            if bytes.first() == Some(&SERIAL_NUMBER) {
                let serial = crate::l3::registration::SerialNumber::decode(bytes)?;
                if serial.has_extensions() {
                    let version = self
                        .core()
                        .registry()
                        .subscribers()
                        .iter()
                        .find(|subscriber| subscriber.provisioning.identity == set.identity)
                        .and_then(|subscriber| subscriber.registration)
                        .ok_or(Failure::Wire)?
                        .protocol_version;
                    serial.validate_version(version)?;
                }
            }
        }
        for bytes in &set.messages {
            match bytes.first().copied() {
                Some(SERIAL_NUMBER) => continue,
                Some(SPACH_CONFIRMATION) => {
                    crate::l3::notification::sms_confirmation(bytes)?;
                    let pending = self
                        .pending
                        .iter_mut()
                        .find(|pending| {
                            pending.identity == set.identity
                                && pending.notification.is_some()
                                && pending.notified
                        })
                        .ok_or(Failure::Wire)?;
                    pending.confirmed = true;
                    tracing::info!(identity=%set.identity, "SMS notification confirmed");
                }
                Some(RDATA | RACCEPT | RREJECT) => {
                    self.receive_relay(set.identity, bytes, Route::Control, now_ms, unix_ms)?
                }
                _ => return Err(Failure::Wire.into()),
            }
        }
        Ok(())
    }
    fn receive_relay(
        &mut self,
        identity: MobileIdentity,
        bytes: &[u8],
        route: Route,
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<(), Box<dyn Error>> {
        let decoded = relay::decode_prefix(bytes, Direction::Reverse)?;
        self.receive_fields(
            identity,
            &decoded.message,
            &decoded.optional_octets,
            route,
            now_ms,
            unix_ms,
        )
    }
    fn receive_fields(
        &mut self,
        identity: MobileIdentity,
        message: &RelayMessage,
        optional_octets: &[u8],
        route: Route,
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<(), Box<dyn Error>> {
        self.duplicates.retain(|entry| now_ms < entry.until_ms);
        match message {
            RelayMessage::Accept { .. } | RelayMessage::Reject { .. } => {
                if !optional_octets.is_empty() {
                    return Err(Failure::Wire.into());
                }
                let transaction_id = match message {
                    RelayMessage::Accept { transaction_id }
                    | RelayMessage::Reject { transaction_id, .. } => *transaction_id,
                    _ => unreachable!("response branch contains ACCEPT or REJECT"),
                };
                self.auxiliary_response_route(identity, transaction_id, route)?;
                let bytes = message.encode(Direction::Reverse)?;
                if let Some(reply) = self.delivery.receive_auxiliary(identity, &bytes, now_ms)? {
                    if reply.state.closed() {
                        self.auxiliary_observed(&reply.packet, now_ms);
                    }
                    self.outgoing_lower_reply(&reply, now_ms)?;
                    tracing::info!(%identity, transaction_id=reply.packet.transaction_id(), state=?reply.state, "Auxiliary R-DATA response received");
                    return Ok(());
                }
                let outcome = self.delivery.receive(identity, &bytes, now_ms)?;
                self.traffic
                    .attempts
                    .retain(|peer| peer.identity != identity);
                tracing::info!(%identity, ?outcome, "SMS relay response received");
            }
            RelayMessage::Data {
                transaction_id,
                payload,
                protocol,
            } => {
                if let Some(duplicate) = self.duplicates.iter().find(|entry| {
                    entry.identity == identity
                        && (!matches!(protocol, relay::HigherLayerProtocol::Segmented(_))
                            || match &entry.registration {
                                Some(registration) => {
                                    self.core().registry().registration_current(registration)
                                }
                                None => self
                                    .core()
                                    .registry()
                                    .registration_handle(identity)
                                    .is_none(),
                            })
                        && entry.message == *message
                        && entry.optional_octets == optional_octets
                }) {
                    let reply = duplicate.reply.clone();
                    self.enqueue_response(identity, message, reply, route, now_ms)?;
                    return Ok(());
                }
                if self.pending.len() + self.traffic.pending.len() + self.control_arq.len()
                    >= MAX_PENDING
                    || self.duplicates.len() >= MAX_SUBMISSION_HISTORY
                {
                    return Err(Failure::QueueFull.into());
                }
                let mut duplicate_until_ms = now_ms
                    .checked_add(self.submission_retry_guard_ms)
                    .ok_or(Failure::Clock)?;
                let response = if *protocol
                    == relay::HigherLayerProtocol::Segmented(relay::Teleservice::Sms)
                {
                    match self.receive_outgoing_response(
                        identity,
                        message,
                        optional_octets,
                        now_ms,
                    )? {
                        Some(response) => response,
                        None => self.receive_segmented_sms(
                            identity,
                            message,
                            optional_octets,
                            now_ms,
                            unix_ms,
                        )?,
                    }
                } else if *protocol != relay::HigherLayerProtocol::Sms {
                    RelayMessage::Reject {
                        transaction_id: *transaction_id,
                        cause: UNSUPPORTED_PROTOCOL_CAUSE,
                    }
                } else {
                    match sms::decode_prefix(payload, Direction::Reverse)
                        .map(|decoded| decoded.message)
                    {
                        Ok(SmsMessage::Submit { .. }) => {
                            let result = self.submit_ordinary_sms(
                                identity,
                                message,
                                optional_octets,
                                now_ms,
                                unix_ms,
                            )?;
                            duplicate_until_ms = result.until_ms;
                            result.response
                        }
                        Ok(SmsMessage::DeliveryAck { .. }) => {
                            relay::addressing::decode_fields(message.clone(), optional_octets)
                                .map_err(crate::core::messaging::MessagingError::Addressing)
                                .and_then(|decoded| {
                                    self.delivery.core_mut().accept_acknowledgment(
                                        identity,
                                        decoded,
                                        unix_ms,
                                        crate::core::store::AcknowledgmentKind::Delivery,
                                    )
                                })
                                .map(|receipt| receipt.response)
                                .unwrap_or_else(|error| {
                                    tracing::warn!(%error, "SMS receipt rejected");
                                    RelayMessage::Reject {
                                        transaction_id: *transaction_id,
                                        cause: rejection_cause(&error),
                                    }
                                })
                        }
                        Ok(SmsMessage::ManualAck { .. }) => {
                            relay::addressing::decode_fields(message.clone(), optional_octets)
                                .map_err(crate::core::messaging::MessagingError::Addressing)
                                .and_then(|decoded| {
                                    self.delivery.core_mut().accept_acknowledgment(
                                        identity,
                                        decoded,
                                        unix_ms,
                                        crate::core::store::AcknowledgmentKind::Manual,
                                    )
                                })
                                .map(|receipt| receipt.response)
                                .unwrap_or_else(|error| {
                                    tracing::warn!(%error, "SMS receipt rejected");
                                    RelayMessage::Reject {
                                        transaction_id: *transaction_id,
                                        cause: rejection_cause(&error),
                                    }
                                })
                        }
                        _ => RelayMessage::Reject {
                            transaction_id: *transaction_id,
                            cause: INVALID_MESSAGE_CAUSE,
                        },
                    }
                };
                let reply = response.encode(Direction::Forward)?;
                self.enqueue_response(identity, message, reply.clone(), route, now_ms)?;
                self.duplicates.push_back(Duplicate {
                    registration: self.core().registry().registration_handle(identity),
                    identity,
                    message: message.clone(),
                    optional_octets: optional_octets.to_vec(),
                    reply,
                    until_ms: duplicate_until_ms,
                });
                self.next_sweep_ms = now_ms;
            }
        }
        Ok(())
    }
    pub fn advance(&mut self, now_ms: u64) -> Result<(), Failure> {
        self.observe(now_ms)?;
        self.advance_incoming_sms(now_ms)
            .map_err(|error| Failure::Incoming(Box::new(error)))?;
        self.advance_auxiliary_jobs(now_ms)?;
        self.advance_tracked_replies(now_ms);
        self.expire_traffic(now_ms)?;
        let mut index = 0;
        while index < self.pending.len() {
            if now_ms >= self.pending[index].deadline_ms {
                let pending = self.pending.remove(index).expect("expired index exists");
                match &pending.owner {
                    ForwardOwner::Reply | ForwardOwner::TrackedReply(_) => {}
                    ForwardOwner::Auxiliary(packet) => self.request_auxiliary_cancel(packet),
                    ForwardOwner::Delivery(delivery) => {
                        self.delivery
                            .cancel_prepared(delivery, now_ms)
                            .map_err(Failure::Delivery)?;
                    }
                }
                tracing::warn!(identity=%pending.identity,"SMS transmission expired before completion");
            } else {
                index += 1;
            }
        }
        if now_ms < self.next_sweep_ms {
            return Ok(());
        }
        self.next_sweep_ms = now_ms
            .checked_add(RETRY_INTERVAL_MS)
            .ok_or(Failure::Clock)?;
        let identities: Vec<_> = self
            .delivery
            .core()
            .registry()
            .subscribers()
            .iter()
            .map(|subscriber| subscriber.provisioning.identity)
            .collect();
        for identity in identities {
            if let Some(ended) = self
                .delivery
                .advance(identity, now_ms)
                .map_err(Failure::Delivery)?
            {
                self.traffic
                    .attempts
                    .retain(|peer| peer.identity != identity);
                self.traffic.pending.retain(|pending| {
                    pending.owner.delivery().is_none() || pending.offer.token.identity != identity
                });
                tracing::info!(?ended, "SMS delivery attempt ended");
            }
            if self.pending.len() + self.traffic.pending.len() + self.control_arq.len()
                >= MAX_PENDING
            {
                break;
            }
            let route = match self
                .traffic
                .peers
                .iter()
                .find(|peer| peer.identity == identity)
            {
                Some(peer) if !peer.ready => continue,
                Some(peer) => Route::Traffic(peer.call_id),
                None => Route::Control,
            };
            let transport = match route {
                Route::Control => DeliveryTransport::Control,
                Route::Traffic(_) => DeliveryTransport::Traffic,
            };
            match self.delivery.prepare_for(identity, now_ms, transport) {
                Ok(Some(prepared)) => {
                    let id = prepared.id().value();
                    if let Some((_, count)) = self
                        .attempt_counts
                        .iter_mut()
                        .find(|(existing, _)| *existing == id)
                    {
                        *count += 1;
                    } else {
                        if self.attempt_counts.len() == MAX_SUBMISSION_HISTORY {
                            self.attempt_counts.pop_front();
                        }
                        self.attempt_counts.push_back((id, 1));
                    }
                    let queued = match route {
                        Route::Control => prepared
                            .relay()
                            .encode(Direction::Forward)
                            .map_err(|_| Failure::Wire)
                            .and_then(|bytes| {
                                self.enqueue(identity, bytes, Some(prepared), now_ms)
                            }),
                        Route::Traffic(call_id) => {
                            crate::l3::legacy_traffic::TrafficRelay::from_addressed(
                                prepared.relay(),
                            )
                            .map_err(|_| Failure::Wire)
                            .and_then(|relay| {
                                self.enqueue_traffic(
                                    identity,
                                    call_id,
                                    relay,
                                    Some(prepared),
                                    now_ms,
                                )
                            })
                        }
                    };
                    if let Err(error) = queued {
                        self.delivery
                            .cancel(identity, now_ms)
                            .map_err(Failure::Delivery)?;
                        return Err(error);
                    }
                }
                Ok(None) | Err(DeliveryError::Busy | DeliveryError::RecipientOffline) => {}
                Err(DeliveryError::Attempt(
                    crate::core::relay_attempt::AttemptError::Identifier(
                        IdentifierError::Exhausted,
                    ),
                )) => {}
                Err(error) => return Err(Failure::Delivery(error)),
            }
        }
        Ok(())
    }
    /// Records the state changes each newly queued message depends on. Called after the
    /// pass's flush, so the mark also covers changes made later in the pass that queued it.
    pub(crate) fn stamp_durability(&mut self) {
        let mark = self.delivery.core().store().change_mark();
        for pending in &mut self.pending {
            pending.durable_after.get_or_insert(mark);
        }
        for pending in &mut self.traffic.pending {
            pending.durable_after.get_or_insert(mark);
        }
    }
    /// A handset must not see a message before the state behind it is on disk.
    fn durable(&self, durable_after: Option<u64>) -> bool {
        let store = self.delivery.core().store();
        match durable_after {
            Some(mark) => store.is_durable(mark),
            None => !store.is_replicated(),
        }
    }
    pub(crate) fn in_progress(&self) -> bool {
        self.pending.front().is_some_and(|pending| pending.started)
    }
    pub(crate) fn repeat_count(&self, superframe: u64, phase: u8) -> usize {
        self.repeats
            .iter()
            .filter(|repeat| {
                repeat.superframe > superframe
                    || repeat.superframe == superframe && repeat.phase >= phase
            })
            .count()
    }
    pub fn has_repeat(&self, superframe: u64, phase: u8) -> bool {
        self.repeats
            .iter()
            .any(|repeat| repeat.superframe == superframe && repeat.phase == phase)
    }
    pub fn frame(&mut self, superframe: u64, phase: u8) -> Option<&[bool; INFORMATION_BITS]> {
        self.frame_inner(superframe, phase, true)
    }
    pub(crate) fn non_arq_frame(
        &mut self,
        superframe: u64,
        phase: u8,
    ) -> Option<&[bool; INFORMATION_BITS]> {
        self.frame_inner(superframe, phase, false)
    }
    fn frame_inner(
        &mut self,
        superframe: u64,
        phase: u8,
        fragmented: bool,
    ) -> Option<&[bool; INFORMATION_BITS]> {
        self.repeats
            .retain(|repeat| repeat.superframe >= superframe);
        if let Some(index) = self
            .repeats
            .iter()
            .position(|repeat| repeat.superframe == superframe && repeat.phase == phase)
        {
            self.selected = Some(Selection::Repeat(index));
            return Some(&self.repeats[index].information);
        }
        let position = if self.in_progress() {
            0
        } else {
            self.pending.iter().position(|pending| {
                if !self.durable(pending.durable_after) {
                    return false;
                }
                if pending.confirmed {
                    return fragmented || pending.frames.len() == 1;
                }
                pending.first_phase == phase
                    && pending.last_notification_sf != Some(superframe)
                    && (superframe.is_multiple_of(2)
                        || pending.last_notification_sf == superframe.checked_sub(1))
            })?
        };
        if position != 0 {
            let selected = self
                .pending
                .remove(position)
                .expect("selected message exists");
            self.pending.push_front(selected);
        }
        let pending = self.pending.front()?;
        let notification = !pending.confirmed;
        self.selected = Some(Selection::Message {
            superframe,
            notification,
        });
        if notification {
            pending.notification.as_ref()
        } else {
            pending.frames.front()
        }
    }
    pub(crate) fn frame_count(&self) -> usize {
        match self.selected {
            Some(Selection::Message {
                notification: false,
                ..
            }) => self
                .pending
                .front()
                .map_or(0, |pending| pending.frames.len()),
            Some(_) => 1,
            None => 0,
        }
    }
    pub(crate) fn adds_repeat(&self) -> bool {
        matches!(self.selected, Some(Selection::Message { notification: true, superframe }) if superframe.is_multiple_of(2))
    }
    pub fn completion(&self) -> Completion {
        match self.selected.as_ref() {
            Some(Selection::Message {
                notification: true, ..
            }) => {
                let pending = self.pending.front().expect("notification selected");
                Completion::Notification {
                    identity: pending.identity,
                    generation: pending.generation,
                }
            }
            Some(Selection::Message {
                notification: false,
                ..
            }) => {
                let pending = self.pending.front().expect("SMS message selected");
                if pending.frames.len() == 1 {
                    match &pending.owner {
                        ForwardOwner::Reply => {
                            if self.protocol.is_some() {
                                Completion::Message {
                                    identity: pending.identity,
                                    bytes: pending.bytes.clone(),
                                }
                            } else {
                                Completion::None
                            }
                        }
                        ForwardOwner::TrackedReply(packet) => Completion::Reply(packet.clone()),
                        ForwardOwner::Delivery(delivery) => Completion::Delivery(delivery.clone()),
                        ForwardOwner::Auxiliary(packet) => Completion::Auxiliary(packet.clone()),
                    }
                } else {
                    Completion::None
                }
            }
            _ => Completion::None,
        }
    }
    pub(crate) fn submitted_at(&mut self, now_ms: u64) -> Result<Completion, Failure> {
        self.observe(now_ms)?;
        let completion = self.take_submitted();
        if let Completion::Reply(packet) = &completion {
            self.reply_handed_off(packet, now_ms, now_ms)?;
        }
        if let Completion::Auxiliary(packet) = &completion {
            self.auxiliary_handed_off(packet, now_ms, now_ms)?;
        }
        Ok(completion)
    }
    pub fn submitted(&mut self) -> Result<Completion, Failure> {
        self.submitted_at(self.last_ms)
    }
    fn take_submitted(&mut self) -> Completion {
        let selected = self
            .selected
            .take()
            .expect("SMS slot selected before submission");
        let Selection::Message {
            superframe,
            notification,
        } = selected
        else {
            if let Selection::Repeat(index) = selected {
                self.repeats.remove(index);
            }
            return Completion::None;
        };
        let pending = self
            .pending
            .front_mut()
            .expect("a selected SMS frame exists");
        if notification {
            pending.last_notification_sf = Some(superframe);
            if superframe.is_multiple_of(2) {
                self.repeats.push(Repeat {
                    superframe: superframe + 1,
                    phase: pending.first_phase,
                    information: pending.notification.expect("notification selected"),
                });
            }
            return Completion::Notification {
                identity: pending.identity,
                generation: pending.generation,
            };
        }
        pending.started = true;
        pending.frames.pop_front();
        if pending.frames.is_empty() {
            match self
                .pending
                .pop_front()
                .expect("completed SMS message exists")
                .owner
            {
                ForwardOwner::Delivery(delivery) => Completion::Delivery(delivery),
                ForwardOwner::Auxiliary(packet) => Completion::Auxiliary(packet),
                ForwardOwner::Reply => Completion::None,
                ForwardOwner::TrackedReply(packet) => Completion::Reply(packet),
            }
        } else {
            Completion::None
        }
    }
    pub fn notification_transmitted(&mut self, identity: MobileIdentity, generation: u64) {
        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|p| p.identity == identity && p.generation == generation)
        {
            pending.notified = true;
            if let Some(log) = &self.protocol {
                log.message(
                    "Forward",
                    "PCH",
                    identity,
                    &crate::l3::notification::sms_notification(),
                    "TX completion observed",
                );
            }
        }
    }
    pub fn transmitted(&mut self, prepared: &PreparedDelivery, now_ms: u64) -> Result<(), Failure> {
        self.transmitted_at(prepared, now_ms, now_ms)
    }
    pub fn transmitted_at(
        &mut self,
        prepared: &PreparedDelivery,
        completed_ms: u64,
        now_ms: u64,
    ) -> Result<(), Failure> {
        self.observe(now_ms)?;
        self.delivery
            .transmitted_at(prepared, completed_ms, now_ms)
            .map_err(Failure::Delivery)?;
        debug_delivered_sms(prepared);
        if let Some(log) = &self.protocol
            && let Ok(bytes) = prepared.relay().encode(Direction::Forward)
        {
            log.message(
                "Forward",
                "SMSCH",
                prepared.recipient(),
                &bytes,
                "TX completion observed",
            );
        }
        Ok(())
    }
    pub fn shutdown(&mut self, now_ms: u64) -> Result<(), Failure> {
        self.cancel_all_auxiliary();
        let identities: Vec<_> = self
            .core()
            .registry()
            .subscribers()
            .iter()
            .map(|s| s.provisioning.identity)
            .collect();
        for identity in identities {
            self.delivery
                .cancel(identity, now_ms)
                .map_err(Failure::Delivery)?;
        }
        self.pending.clear();
        self.control_arq.clear();
        self.traffic = traffic::TrafficSms::default();
        Ok(())
    }
}

fn rejection_cause(error: &crate::core::messaging::MessagingError) -> u8 {
    use crate::{
        core::{messaging::MessagingError, store::StoreError},
        l3::legacy_traffic::TrafficMessageError,
    };
    match error {
        MessagingError::Store(StoreError::Traffic(
            TrafficMessageError::Relay(relay::RelayError::Length)
            | TrafficMessageError::OptionalLength,
        )) => TOO_LONG_CAUSE,
        _ => INVALID_MESSAGE_CAUSE,
    }
}

#[cfg(test)]
mod tests;

fn debug_delivered_sms(prepared: &PreparedDelivery) {
    if tracing::enabled!(tracing::Level::DEBUG)
        && let relay::RelayMessage::Data {
            protocol: relay::HigherLayerProtocol::Sms,
            payload,
            ..
        } = &prepared.relay().message
        && let Ok(decoded) = crate::l3::sms::decode_prefix(payload, Direction::Forward)
        && let SmsMessage::Deliver {
            metadata,
            validity,
            display_time,
            ..
        } = decoded.message
    {
        tracing::debug!(identity=%prepared.recipient(), reference=metadata.message_reference,
                ?validity, ?display_time, new_message=metadata.new_message,
                privacy=metadata.privacy, urgency=metadata.urgency,
                "SMS DELIVER transmitted");
    }
}
