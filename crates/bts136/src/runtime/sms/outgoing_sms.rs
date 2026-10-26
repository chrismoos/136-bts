mod retirement;
use super::*;
use crate::{
    core::{
        delivery::AuxiliaryReply,
        messaging::DeliveryOffer,
        store::{
            QueuedSms, StoreError,
            outgoing_journal::{
                JournalEvent, JournalHandle, JournalSnapshot, OutgoingRequest, SubmittedFragment,
            },
        },
    },
    l3::{
        address::{self, AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        legacy_traffic::{TrafficMessage, TrafficRelay},
        relay::{
            HigherLayerProtocol, Teleservice,
            addressing::{AddressedRelay, RelayAddresses},
        },
        tsar::{self, AcknowledgmentWindow},
    },
    link::tsar::{Sender, SenderState},
};

const CONTEXT_VERSION: u8 = 1;
const MAX_CONTEXT_OCTETS: u32 = 4096;
const MAX_EVENTS: u32 = 4096;
const TELEPHONY_PLAN: u8 = 1;
const RESPONSE_TYPE_MASK: u8 = 0x38;
const RESPONSE_TYPE: u8 = 0x18;
const TRANSACTION_ID_MASK: u8 = 7;
const SERVICE: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
struct Stream {
    recovered: bool,
    retirement: Option<retirement::Candidate>,
    snapshot: JournalSnapshot,
    sender: Sender,
    pending: Option<SubmittedFragment>,
    expired_submission: Option<u32>,
    originating: AddressInfo,
}
impl Stream {
    fn restore(snapshot: JournalSnapshot) -> Result<Self, Failure> {
        let originating = decode_context(&snapshot.context)?;
        let sender = snapshot.replay().map_err(Failure::Store)?;
        let pending = snapshot.submitted_fragment().map_err(Failure::Store)?;
        Ok(Self {
            recovered: false,
            retirement: None,
            snapshot,
            sender,
            pending,
            expired_submission: None,
            originating,
        })
    }
}
struct Job {
    handle: JournalHandle,
    submission_ordinal: u32,
    packet: AuxiliaryPacket,
    reported: bool,
}
#[derive(Default)]
pub(super) struct OutgoingSms {
    started: bool,
    streams: Vec<Stream>,
    jobs: Vec<Job>,
    next_scan_ms: u64,
}
fn decode_context(bytes: &[u8]) -> Result<AddressInfo, Failure> {
    let Some((&CONTEXT_VERSION, bytes)) = bytes.split_first() else {
        return Err(Failure::Wire);
    };
    let decoded = address::decode_prefix(bytes).map_err(|_| Failure::Wire)?;
    if decoded.consumed_octets != bytes.len() {
        return Err(Failure::Wire);
    }
    Ok(decoded.address)
}
pub(super) fn is_response(payload: &[u8]) -> bool {
    payload
        .first()
        .is_some_and(|header| header & RESPONSE_TYPE_MASK == RESPONSE_TYPE)
}
fn closed(state: SenderState) -> bool {
    matches!(
        state,
        SenderState::Complete(_) | SenderState::Failed(_) | SenderState::Cancelled
    )
}
fn relay(originating: &AddressInfo, payload: Vec<u8>) -> AddressedRelay {
    AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: 0,
            protocol: SERVICE,
            payload,
        },
        addresses: RelayAddresses {
            originating: Some(originating.clone()),
            ..RelayAddresses::default()
        },
    }
}
fn fits(route: Route, message: &AddressedRelay) -> bool {
    match route {
        Route::Control => message.encode(Direction::Forward).is_ok(),
        Route::Traffic(_) => TrafficRelay::from_addressed(message)
            .is_ok_and(|relay| crate::link::traffic::encode(&TrafficMessage::Relay(relay)).is_ok()),
    }
}
impl SmsRuntime {
    pub(super) fn outgoing_reply_handle(
        &self,
        peer: MobileIdentity,
        payload: &[u8],
    ) -> Option<JournalHandle> {
        let header = *payload.first()?;
        if !is_response(payload) {
            return None;
        }
        self.outgoing_sms
            .streams
            .iter()
            .find(|stream| {
                stream.snapshot.peer == peer
                    && stream.snapshot.plan.segments()[0].transaction_id
                        == header & TRANSACTION_ID_MASK
            })
            .map(|stream| stream.snapshot.handle.clone())
    }
    pub(super) fn outgoing_message_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.outgoing_sms
            .streams
            .iter()
            .map(|stream| stream.snapshot.sms_id.value())
    }
    pub(super) fn outgoing_measurements(&self, id: i64) -> Vec<crate::operations::proto::Metric> {
        let Some(stream) = self
            .outgoing_sms
            .streams
            .iter()
            .find(|stream| stream.snapshot.sms_id.value() == id)
        else {
            return Vec::new();
        };
        let mut measurements = vec![
            crate::operations::metric(
                "Segmentation state",
                format!("{:?}", stream.sender.state()),
                "",
            ),
            crate::operations::metric("Segments", stream.snapshot.plan.segments().len(), ""),
            crate::operations::metric(
                "Segments handed to Layer 3",
                stream.sender.layer3_submissions(),
                "",
            ),
        ];
        if closed(stream.sender.state()) && !retirement::eligible(stream) {
            measurements.push(crate::operations::metric(
                "Transaction ID",
                "Reserved until peer closure is proved",
                "",
            ));
        }
        measurements
    }
    fn outgoing_time(&self, now_ms: u64) -> Result<i64, Failure> {
        self.incoming_clock(now_ms)
            .map(|clock| clock.now_unix_ms)
            .map_err(|error| Failure::Incoming(Box::new(error)))
    }
    pub(crate) fn start_outgoing_sms(&mut self, now_ms: u64) -> Result<(), Failure> {
        if self.outgoing_sms.started {
            return Err(Failure::Wire);
        }
        let unix_ms = self.outgoing_time(now_ms)?;
        let journals = self
            .core_mut()
            .store_mut()
            .active_outgoing_journals(0, (MAX_SUBMISSION_HISTORY + 1) as u32)
            .map_err(Failure::Store)?;
        if journals.len() > MAX_SUBMISSION_HISTORY {
            return Err(Failure::QueueFull);
        }
        let mut streams = Vec::with_capacity(journals.len());
        for snapshot in journals {
            if snapshot.observed_at_unix_ms > unix_ms {
                return Err(Failure::Clock);
            }
            let mut stream = Stream::restore(snapshot)?;
            stream.recovered = true;
            streams.push(stream);
        }
        self.outgoing_sms.streams = streams;
        self.outgoing_sms.started = true;
        for index in 0..self.outgoing_sms.streams.len() {
            self.advance_outgoing(index, unix_ms)?;
        }
        Ok(())
    }
    fn apply_outgoing(
        &mut self,
        index: usize,
        event: JournalEvent,
        unix_ms: i64,
    ) -> Result<(), Failure> {
        let stream = &self.outgoing_sms.streams[index];
        let handle = stream.snapshot.handle.clone();
        let revision = stream.snapshot.revision();
        let expired = stream.expired_submission;
        let recovered = stream.recovered;
        let previous_state = stream.sender.state();
        let snapshot = self
            .core_mut()
            .store_mut()
            .append_outgoing_event(&handle, revision, event, unix_ms)
            .map_err(Failure::Store)?;
        let mut stream = Stream::restore(snapshot)?;
        stream.expired_submission = expired;
        stream.recovered = recovered;
        if stream.sender.state() != previous_state {
            tracing::info!(sms_id=stream.snapshot.sms_id.value(), peer=?stream.snapshot.peer, previous=?previous_state, current=?stream.sender.state(), "Outgoing segmented SMS state changed");
        }
        self.outgoing_sms.streams[index] = stream;
        Ok(())
    }
    fn advance_outgoing(&mut self, index: usize, unix_ms: i64) -> Result<(), Failure> {
        let now = u64::try_from(unix_ms).map_err(|_| Failure::Clock)?;
        if self.outgoing_sms.streams[index]
            .sender
            .deadline_monotonic_ms()
            .is_some_and(|deadline| now >= deadline)
        {
            self.apply_outgoing(index, JournalEvent::Advance, unix_ms)?;
        }
        Ok(())
    }
    pub(super) fn owns_outgoing_fragment(&self, packet: &AuxiliaryPacket) -> bool {
        self.outgoing_sms
            .jobs
            .iter()
            .any(|job| job.packet == *packet)
    }
    pub(super) fn outgoing_lower_reply(
        &mut self,
        reply: &AuxiliaryReply,
        now_ms: u64,
    ) -> Result<(), Failure> {
        let Some(job_index) = self
            .outgoing_sms
            .jobs
            .iter()
            .position(|job| job.packet == reply.packet)
        else {
            return Ok(());
        };
        let handle = self.outgoing_sms.jobs[job_index].handle.clone();
        let ordinal = self.outgoing_sms.jobs[job_index].submission_ordinal;
        let index = self
            .outgoing_sms
            .streams
            .iter()
            .position(|stream| stream.snapshot.handle == handle)
            .ok_or(Failure::Wire)?;
        let unix_ms = self.outgoing_time(now_ms)?;
        self.advance_outgoing(index, unix_ms)?;
        if self.outgoing_sms.streams[index]
            .pending
            .as_ref()
            .is_some_and(|pending| pending.submission_ordinal == ordinal)
        {
            let event = match reply.state {
                AuxiliaryState::Accepted => Some(JournalEvent::Accepted {
                    submission_ordinal: ordinal,
                }),
                AuxiliaryState::Rejected { cause } => Some(JournalEvent::Rejected {
                    submission_ordinal: ordinal,
                    cause,
                }),
                _ => None,
            };
            if let Some(event) = event {
                self.apply_outgoing(index, event, unix_ms)?;
            }
        }
        self.outgoing_sms.jobs[job_index].reported = true;
        self.withdraw_outgoing(index, now_ms)
    }
    fn withdraw_outgoing(&mut self, index: usize, now_ms: u64) -> Result<(), Failure> {
        let stream = &self.outgoing_sms.streams[index];
        let ordinal = stream
            .pending
            .as_ref()
            .map(|pending| pending.submission_ordinal);
        let packets: Vec<_> = self
            .outgoing_sms
            .jobs
            .iter()
            .filter(|job| {
                job.handle == stream.snapshot.handle
                    && (closed(stream.sender.state()) || Some(job.submission_ordinal) != ordinal)
            })
            .map(|job| job.packet.clone())
            .collect();
        if packets.is_empty() {
            return Ok(());
        }
        for packet in packets {
            self.request_auxiliary_cancel(&packet);
        }
        self.advance_auxiliary_jobs(now_ms)
    }
    fn settle_outgoing_jobs(&mut self) -> Result<(), Failure> {
        let mut index = 0;
        while index < self.outgoing_sms.jobs.len() {
            let job = &self.outgoing_sms.jobs[index];
            let Some(state) = self.auxiliary.outcome(&job.packet) else {
                index += 1;
                continue;
            };
            if matches!(
                state,
                AuxiliaryState::Accepted | AuxiliaryState::Rejected { .. }
            ) && !job.reported
            {
                return Err(Failure::Wire);
            }
            if state == AuxiliaryState::Expired {
                let stream = self
                    .outgoing_sms
                    .streams
                    .iter_mut()
                    .find(|stream| stream.snapshot.handle == job.handle)
                    .ok_or(Failure::Wire)?;
                stream.expired_submission = Some(job.submission_ordinal);
            }
            let packet = job.packet.clone();
            self.auxiliary.take_outcome(&packet);
            self.outgoing_sms.jobs.remove(index);
        }
        Ok(())
    }
    fn outgoing_route(&self, peer: MobileIdentity) -> Option<Route> {
        match self
            .traffic
            .peers
            .iter()
            .find(|candidate| candidate.identity == peer)
        {
            Some(peer) if !peer.ready => None,
            Some(peer) => Some(Route::Traffic(peer.call_id)),
            None => Some(Route::Control),
        }
    }
    fn outgoing_version(&self, peer: MobileIdentity) -> Option<ProtocolVersion> {
        self.core()
            .registry()
            .subscribers()
            .iter()
            .find(|subscriber| subscriber.provisioning.identity == peer)
            .and_then(|subscriber| subscriber.registration)
            .map(|registration| {
                crate::runtime::capabilities::SERVING_PROTOCOL.common(registration.protocol_version)
            })
    }
    fn admit_outgoing(
        &mut self,
        peer: MobileIdentity,
        queued: QueuedSms,
        route: Route,
        version: ProtocolVersion,
        unix_ms: i64,
    ) -> Result<(), Failure> {
        if self
            .core()
            .store()
            .outgoing_owns_sms(queued.id)
            .map_err(Failure::Store)?
        {
            return Ok(());
        }
        let offer = DeliveryOffer {
            recipient: peer,
            queued,
            time_zone: self.core().time_zone(),
        };
        if fits(route, &offer.relay(0)) {
            return Ok(());
        }
        let originating = AddressInfo {
            number_type: 0,
            numbering_plan: TELEPHONY_PLAN,
            address: AddressValue::Tbcd(offer.queued.sender_number.clone()),
        };
        let budget = (1..=relay::MAX_DATA_PAYLOAD_OCTETS)
            .rev()
            .find(|budget| fits(route, &relay(&originating, vec![0; *budget])))
            .ok_or(Failure::Wire)?;
        let mut context = vec![CONTEXT_VERSION];
        context.extend(originating.encode().map_err(|_| Failure::Wire)?);
        let request = OutgoingRequest {
            sms_id: offer.queued.id,
            peer,
            version,
            payload_budget_octets: budget,
            acknowledgment_window: AcknowledgmentWindow::Last,
            context,
            maximum_context_octets: MAX_CONTEXT_OCTETS,
            maximum_events: MAX_EVENTS,
        };
        let snapshot = match self
            .core_mut()
            .store_mut()
            .begin_outgoing_journal(request, unix_ms)
        {
            Ok(snapshot) => snapshot,
            Err(StoreError::OutgoingJournal(
                crate::core::store::outgoing_journal::JournalError::Capacity,
            )) => return Ok(()),
            Err(error) => return Err(Failure::Store(error)),
        };
        tracing::info!(
            sms_id = snapshot.sms_id.value(),
            ?peer,
            transaction_id = snapshot.plan.segments()[0].transaction_id,
            segments = snapshot.plan.segments().len(),
            payload_budget_octets = budget,
            "Outgoing segmented SMS admitted"
        );
        self.outgoing_sms.streams.push(Stream::restore(snapshot)?);
        Ok(())
    }
    pub(crate) fn dispatch_outgoing_sms(&mut self, now_ms: u64) -> Result<(), Failure> {
        self.observe(now_ms)?;
        if !self.outgoing_sms.started {
            self.start_outgoing_sms(now_ms)?;
        }
        let unix_ms = self.outgoing_time(now_ms)?;
        self.settle_outgoing_jobs()?;
        if now_ms >= self.outgoing_sms.next_scan_ms {
            self.outgoing_sms.next_scan_ms = now_ms
                .checked_add(RETRY_INTERVAL_MS)
                .ok_or(Failure::Clock)?;
            let peers: Vec<_> = self
                .core()
                .registry()
                .subscribers()
                .iter()
                .map(|subscriber| subscriber.provisioning.identity)
                .collect();
            for peer in peers {
                if self.delivery.busy(peer) {
                    continue;
                }
                let Some(version) = self
                    .outgoing_version(peer)
                    .filter(|version| version.code() >= ProtocolVersion::Pv1.code())
                else {
                    continue;
                };
                let Some(route) = self.outgoing_route(peer) else {
                    continue;
                };
                let queued = self
                    .core()
                    .store()
                    .pending_for(peer)
                    .map_err(Failure::Store)?;
                for queued in queued {
                    if self.outgoing_sms.streams.len() >= MAX_SUBMISSION_HISTORY {
                        break;
                    }
                    self.admit_outgoing(peer, queued, route, version, unix_ms)?;
                }
            }
        }
        for index in 0..self.outgoing_sms.streams.len() {
            self.advance_outgoing(index, unix_ms)?;
            let stream = &self.outgoing_sms.streams[index];
            let handle = stream.snapshot.handle.clone();
            let ordinal = stream
                .pending
                .as_ref()
                .map(|pending| pending.submission_ordinal);
            self.withdraw_outgoing(index, now_ms)?;
            if closed(self.outgoing_sms.streams[index].sender.state()) {
                continue;
            }
            if self
                .outgoing_sms
                .jobs
                .iter()
                .any(|job| job.handle == handle && Some(job.submission_ordinal) == ordinal)
            {
                continue;
            }
            let stream = &self.outgoing_sms.streams[index];
            let Some(route) = self.outgoing_route(stream.snapshot.peer) else {
                continue;
            };
            if self
                .outgoing_version(stream.snapshot.peer)
                .is_none_or(|version| version.code() < stream.snapshot.version.code())
            {
                continue;
            }
            let (segment, submission_ordinal, newly_submitted) =
                match (&stream.pending, stream.sender.submission()) {
                    (Some(pending), _)
                        if stream.expired_submission != Some(pending.submission_ordinal) =>
                    {
                        (pending.segment.clone(), pending.submission_ordinal, false)
                    }
                    (None, Some(submission)) => (
                        submission.segment().clone(),
                        stream.snapshot.revision(),
                        true,
                    ),
                    _ => continue,
                };
            let message = relay(
                &stream.originating,
                segment
                    .encode(stream.snapshot.version)
                    .map_err(|_| Failure::Wire)?,
            );
            if !fits(route, &message) {
                continue;
            }
            let packet = match self.send_auxiliary(stream.snapshot.peer, message, now_ms) {
                Ok(packet) => packet,
                Err(
                    Failure::QueueFull
                    | Failure::Delivery(
                        DeliveryError::RecipientOffline
                        | DeliveryError::UnknownRecipient
                        | DeliveryError::Busy
                        | DeliveryError::Identifier(IdentifierError::Exhausted),
                    ),
                ) => continue,
                Err(error) => return Err(error),
            };
            if newly_submitted {
                let event = JournalEvent::Submitted {
                    sequence: segment.sequence_number,
                    queued_at_unix_ms: unix_ms,
                };
                if let Err(error) = self.apply_outgoing(index, event, unix_ms) {
                    self.cancel_auxiliary(&packet, now_ms)?;
                    return Err(error);
                }
            }
            self.outgoing_sms.jobs.push(Job {
                handle,
                submission_ordinal,
                packet,
                reported: false,
            });
        }
        self.advance_auxiliary_jobs(now_ms)
    }
    pub(super) fn receive_outgoing_response(
        &mut self,
        peer: MobileIdentity,
        message: &RelayMessage,
        optional_octets: &[u8],
        now_ms: u64,
    ) -> Result<Option<RelayMessage>, Failure> {
        let RelayMessage::Data {
            transaction_id,
            payload,
            ..
        } = message
        else {
            return Ok(None);
        };
        if !is_response(payload) {
            return Ok(None);
        }
        let reject = || {
            Some(RelayMessage::Reject {
                transaction_id: *transaction_id,
                cause: INVALID_MESSAGE_CAUSE,
            })
        };
        let Some(version) = self
            .outgoing_version(peer)
            .filter(|version| version.code() >= ProtocolVersion::Pv1.code())
        else {
            return Ok(reject());
        };
        if !relay::addressing::decode_fields(message.clone(), optional_octets)
            .is_ok_and(|decoded| decoded.unparsed_bits.is_empty())
        {
            return Ok(reject());
        }
        if !self.outgoing_sms.started {
            self.start_outgoing_sms(now_ms)?;
        }
        let Ok(decoded) = tsar::decode_response(payload, version) else {
            return Ok(reject());
        };
        if !decoded.unparsed_bits.is_empty() {
            return Ok(reject());
        }
        let Some(index) = self.outgoing_sms.streams.iter().position(|stream| {
            stream.snapshot.peer == peer
                && stream.snapshot.plan.segments()[0].transaction_id
                    == decoded.response.transaction_id
        }) else {
            return Ok(reject());
        };
        let stream = &self.outgoing_sms.streams[index];
        if version.code() < stream.snapshot.version.code() {
            return Ok(reject());
        }
        let unix_ms = self.outgoing_time(now_ms)?;
        if closed(stream.sender.state()) {
            if stream.snapshot.events.iter().any(|event| matches!(&event.event,JournalEvent::Response { payload:previous } if previous == payload)) { return Ok(Some(RelayMessage::Accept { transaction_id:*transaction_id })); }
            return Ok(reject());
        }
        let mut candidate = stream.snapshot.replay().map_err(Failure::Store)?;
        if candidate
            .receive_response(
                peer,
                SERVICE,
                &decoded,
                u64::try_from(unix_ms).map_err(|_| Failure::Clock)?,
            )
            .is_err()
        {
            return Ok(reject());
        }
        self.apply_outgoing(
            index,
            JournalEvent::Response {
                payload: payload.clone(),
            },
            unix_ms,
        )?;
        self.withdraw_outgoing(index, now_ms)?;
        Ok(Some(RelayMessage::Accept {
            transaction_id: *transaction_id,
        }))
    }
}

#[cfg(test)]
mod tests;
