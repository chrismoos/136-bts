mod context;
mod retirement;
use super::*;
use crate::{
    core::{
        messaging::MessagingError,
        store::{
            StoreError,
            incoming_journal::{IncomingBinding, IncomingJournal, JournalEvent, JournalLimits},
        },
    },
    l3::{
        broadcast::ProtocolVersion,
        relay::{HigherLayerProtocol, Teleservice},
        tsar::{self, SegmentKind},
    },
    link::tsar::{IncomingLimits, Receiver},
};
use context::Context;
use tsar_recovery::{RecoveryClock, RestoredIncoming};

const SERVICE: HigherLayerProtocol = HigherLayerProtocol::Segmented(Teleservice::Sms);
const TRANSACTION_CAPACITY: usize = crate::link::tsar::IncomingTransactions::TRANSACTION_CAPACITY;
const LIMITS: IncomingLimits = IncomingLimits {
    maximum_message_octets: sms::MAX_SUBMIT_PDU_OCTETS,
    maximum_reserved_payload_octets: sms::MAX_SUBMIT_PDU_OCTETS * TRANSACTION_CAPACITY * 2,
};
const JOURNAL_LIMITS: JournalLimits = JournalLimits {
    maximum_events: 4096,
    maximum_context_octets: 4096,
};
const MAX_STREAMS: usize = MAX_SUBMISSION_HISTORY;
const REUSE_GUARD_MS: u64 = super::super::timers::SMS_ORIGINATION_MS;

#[derive(Debug)]
pub enum IncomingSmsError {
    Context,
    Clock,
    Capacity,
    Store(StoreError),
    Messaging(MessagingError),
    Recovery(Box<tsar_recovery::Error>),
    Transition(Box<tsar_ingress::Error>),
    Submission(Box<tsar_submission::Error>),
    Ownership(crate::link::tsar::IncomingError),
}
impl fmt::Display for IncomingSmsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "incoming segmented SMS failed: {self:?}")
    }
}
impl Error for IncomingSmsError {}
#[derive(Clone, Copy)]
struct Epoch {
    radio_ms: u64,
    unix_ms: i64,
}
impl Epoch {
    fn at(self, radio_ms: u64) -> Result<RecoveryClock, IncomingSmsError> {
        let elapsed = radio_ms
            .checked_sub(self.radio_ms)
            .ok_or(IncomingSmsError::Clock)?;
        let unix_ms = self
            .unix_ms
            .checked_add(i64::try_from(elapsed).map_err(|_| IncomingSmsError::Clock)?)
            .ok_or(IncomingSmsError::Clock)?;
        Ok(RecoveryClock {
            now_monotonic_ms: u64::try_from(unix_ms).map_err(|_| IncomingSmsError::Clock)?,
            now_unix_ms: unix_ms,
        })
    }
}
struct Tracked {
    recovered: bool,
    retirement: Option<retirement::Candidate>,
    complete: bool,
    journal: IncomingJournal,
    deadline_ms: Option<u64>,
    pending_application: bool,
}
#[derive(Default)]
pub(super) struct IncomingSms {
    epoch: Option<Epoch>,
    tracked: Vec<Tracked>,
    next_sweep_ms: u64,
}
impl SmsRuntime {
    pub(super) fn incoming_started(&self) -> bool {
        self.incoming_sms.epoch.is_some()
    }
    pub(super) fn incoming_observed_unix(
        &self,
        now_ms: u64,
        fallback_unix_ms: i64,
    ) -> Result<i64, Failure> {
        match self.incoming_sms.epoch {
            Some(epoch) => epoch
                .at(now_ms)
                .map(|clock| clock.now_unix_ms)
                .map_err(|error| Failure::Incoming(Box::new(error))),
            None => Ok(fallback_unix_ms),
        }
    }
    pub(super) fn incoming_clock(&self, now_ms: u64) -> Result<RecoveryClock, IncomingSmsError> {
        self.incoming_sms
            .epoch
            .ok_or(IncomingSmsError::Clock)?
            .at(now_ms)
    }
    fn remember_incoming(&mut self, incoming: &RestoredIncoming) -> Result<(), IncomingSmsError> {
        for entry in &incoming.transactions {
            let mut tracked = Tracked {
                recovered: false,
                retirement: None,
                complete: incoming
                    .bank
                    .receiver(&entry.transaction)
                    .map_err(IncomingSmsError::Ownership)?
                    .state()
                    == crate::link::tsar::ReceiverState::Complete,
                journal: entry.journal.clone(),
                deadline_ms: incoming
                    .bank
                    .receiver(&entry.transaction)
                    .map_err(IncomingSmsError::Ownership)?
                    .deadline_monotonic_ms(),
                pending_application: incoming
                    .bank
                    .pending_message(&entry.transaction)
                    .map_err(IncomingSmsError::Ownership)?
                    .is_some(),
            };
            if let Some(old) = self
                .incoming_sms
                .tracked
                .iter_mut()
                .find(|old| old.journal.handle == entry.journal.handle)
            {
                tracked.recovered = old.recovered;
                if tracked.journal.revision == old.journal.revision {
                    tracked.retirement = old.retirement.take();
                }
                *old = tracked;
            } else {
                if self.incoming_sms.tracked.len() >= MAX_STREAMS {
                    return Err(IncomingSmsError::Capacity);
                }
                self.incoming_sms.tracked.push(tracked);
            }
        }
        Ok(())
    }
    fn restore_incoming_bank(
        &self,
        journal: &IncomingJournal,
        clock: RecoveryClock,
    ) -> Result<RestoredIncoming, IncomingSmsError> {
        let handles: Vec<_> = self
            .core()
            .store()
            .incoming_bank_journals(journal.binding.peer, SERVICE)
            .map_err(IncomingSmsError::Store)?
            .into_iter()
            .map(|journal| journal.handle)
            .collect();
        tsar_recovery::restore(
            self.core().store(),
            journal.binding.peer,
            SERVICE,
            LIMITS,
            REUSE_GUARD_MS,
            clock,
            &handles,
        )
        .map_err(|error| IncomingSmsError::Recovery(Box::new(error)))
    }
    fn apply_incoming(
        &mut self,
        journal: &IncomingJournal,
        event: JournalEvent,
        now_ms: u64,
    ) -> Result<tsar_ingress::PersistedTransition, IncomingSmsError> {
        let clock = self.incoming_clock(now_ms)?;
        let result = tsar_ingress::apply(
            self.core_mut().store_mut(),
            tsar_ingress::Transition {
                peer: journal.binding.peer,
                protocol: SERVICE,
                limits: LIMITS,
                reuse_guard_ms: REUSE_GUARD_MS,
                clock,
                target: &journal.handle,
                expected_revision: journal.revision,
                event,
            },
        )
        .map_err(|error| IncomingSmsError::Transition(Box::new(error)))?;
        self.remember_incoming(&result.incoming)?;
        self.next_incoming_response_scan_ms = now_ms;
        Ok(result)
    }
    fn finish_incoming_application(
        &mut self,
        journal: &IncomingJournal,
        incoming: &mut RestoredIncoming,
        now_ms: u64,
    ) -> Result<bool, IncomingSmsError> {
        let entry = incoming
            .transactions
            .iter()
            .find(|entry| entry.journal.handle == journal.handle)
            .ok_or(IncomingSmsError::Context)?;
        if let Some(outcome) = incoming
            .bank
            .application_outcome(&entry.transaction)
            .map_err(IncomingSmsError::Ownership)?
        {
            return Ok(outcome == crate::link::tsar::ApplicationOutcome::Rejected);
        }
        if incoming
            .bank
            .pending_message(&entry.transaction)
            .map_err(IncomingSmsError::Ownership)?
            .is_none()
        {
            return Ok(false);
        }
        let context = Context::decode(&journal.binding.context)?;
        let destination = context.destination()?;
        let clock = self.incoming_clock(now_ms)?;
        let key = journal.handle.submission_key();
        let result = tsar_submission::commit_bound_with_sender(
            self.core_mut(),
            &mut incoming.bank,
            &entry.transaction,
            tsar_submission::BoundSubmissionContext {
                destination: &destination,
                submission_key: &key,
                created_at_unix_ms: journal.created_at_unix_ms,
                monotonic_ms: clock.now_monotonic_ms,
            },
            &context.sender_number,
        );
        let (event, rejected) = match result {
            Ok(stored) => {
                tracing::info!(peer=?journal.binding.peer, id=?stored.id, tsar_transaction_id=journal.binding.transaction_id, "Reassembled SMS submission stored");
                (JournalEvent::ApplicationCommitted(stored.id), false)
            }
            Err(tsar_submission::Error::Messaging(error))
                if permanent_application_error(&error) =>
            {
                tracing::warn!(%error, peer=?journal.binding.peer, "Reassembled SMS application rejected");
                (JournalEvent::ApplicationRejected, true)
            }
            Err(error) => return Err(IncomingSmsError::Submission(Box::new(error))),
        };
        self.apply_incoming(journal, event, now_ms)?;
        Ok(rejected)
    }
    pub(crate) fn start_incoming_sms(
        &mut self,
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<(), IncomingSmsError> {
        if self.incoming_sms.epoch.is_some() {
            return Err(IncomingSmsError::Context);
        }
        self.observe(now_ms).map_err(|_| IncomingSmsError::Clock)?;
        self.core()
            .store()
            .validate_ordinary_clock(unix_ms)
            .map_err(IncomingSmsError::Store)?;
        let epoch = Epoch {
            radio_ms: now_ms,
            unix_ms,
        };
        let clock = epoch.at(now_ms)?;
        let journals = self
            .core()
            .store()
            .active_incoming_journals(0, (MAX_STREAMS + 1) as u32)
            .map_err(IncomingSmsError::Store)?;
        if journals.len() > MAX_STREAMS {
            return Err(IncomingSmsError::Capacity);
        }
        self.incoming_sms.epoch = Some(epoch);
        for journal in journals
            .into_iter()
            .filter(|journal| journal.binding.protocol == SERVICE)
        {
            Context::decode(&journal.binding.context)?;
            let incoming = self.restore_incoming_bank(&journal, clock)?;
            self.remember_incoming(&incoming)?;
        }
        for tracked in &mut self.incoming_sms.tracked {
            tracked.recovered = true;
        }
        self.advance_incoming_sms(now_ms)
    }
    pub(super) fn advance_incoming_sms(&mut self, now_ms: u64) -> Result<(), IncomingSmsError> {
        if self.incoming_sms.epoch.is_none() || now_ms < self.incoming_sms.next_sweep_ms {
            return Ok(());
        }
        let clock = self.incoming_clock(now_ms)?;
        self.incoming_sms.next_sweep_ms = now_ms
            .checked_add(RETRY_INTERVAL_MS)
            .ok_or(IncomingSmsError::Clock)?;
        let due: Vec<_> = self
            .incoming_sms
            .tracked
            .iter()
            .filter(|entry| {
                entry.pending_application
                    || entry
                        .deadline_ms
                        .is_some_and(|deadline| clock.now_monotonic_ms >= deadline)
            })
            .map(|entry| entry.journal.clone())
            .collect();
        for mut journal in due {
            let mut incoming = self.restore_incoming_bank(&journal, clock)?;
            let entry = incoming
                .transactions
                .iter()
                .find(|entry| entry.journal.handle == journal.handle)
                .ok_or(IncomingSmsError::Context)?;
            if incoming
                .bank
                .receiver(&entry.transaction)
                .map_err(IncomingSmsError::Ownership)?
                .deadline_monotonic_ms()
                .is_some_and(|deadline| clock.now_monotonic_ms >= deadline)
            {
                incoming = self
                    .apply_incoming(&journal, JournalEvent::Advance, now_ms)?
                    .incoming;
                journal = self
                    .core()
                    .store()
                    .incoming_journal(&journal.handle)
                    .map_err(IncomingSmsError::Store)?;
            }
            self.finish_incoming_application(&journal, &mut incoming, now_ms)?;
        }
        Ok(())
    }
    pub(super) fn receive_segmented_sms(
        &mut self,
        peer: MobileIdentity,
        message: &RelayMessage,
        optional_octets: &[u8],
        now_ms: u64,
        unix_ms: i64,
    ) -> Result<RelayMessage, IncomingSmsError> {
        let RelayMessage::Data {
            transaction_id,
            payload,
            ..
        } = message
        else {
            return Err(IncomingSmsError::Context);
        };
        let reject = |cause| RelayMessage::Reject {
            transaction_id: *transaction_id,
            cause,
        };
        let Some(registration) = self
            .core()
            .registry()
            .subscribers()
            .iter()
            .find(|subscriber| subscriber.provisioning.identity == peer)
            .and_then(|subscriber| subscriber.registration)
        else {
            return Ok(reject(INVALID_MESSAGE_CAUSE));
        };
        let common =
            crate::runtime::capabilities::SERVING_PROTOCOL.common(registration.protocol_version);
        if common.code() < ProtocolVersion::Pv1.code() {
            return Ok(reject(UNSUPPORTED_PROTOCOL_CAUSE));
        }
        let addressed = match relay::addressing::decode_fields(message.clone(), optional_octets) {
            Ok(decoded) if decoded.unparsed_bits.is_empty() => decoded,
            _ => return Ok(reject(INVALID_MESSAGE_CAUSE)),
        };
        let decoded = match tsar::decode_segment(payload, common) {
            Ok(decoded) if decoded.unparsed_octets.is_empty() => decoded,
            _ => return Ok(reject(INVALID_MESSAGE_CAUSE)),
        };
        if self.incoming_sms.epoch.is_none() {
            self.start_incoming_sms(now_ms, unix_ms)?;
        }
        let clock = self.incoming_clock(now_ms)?;
        let existing = self
            .core()
            .store()
            .incoming_bank_journals(peer, SERVICE)
            .map_err(IncomingSmsError::Store)?
            .into_iter()
            .find(|journal| journal.binding.transaction_id == decoded.segment.transaction_id);
        let mut journal = if let Some(journal) = existing {
            if common.code() < journal.binding.version.code() {
                return Ok(reject(UNSUPPORTED_PROTOCOL_CAUSE));
            }
            let context = Context::decode(&journal.binding.context)?;
            match context.matches(self.core(), &addressed.relay.addresses, registration.esn) {
                Ok(true) => {}
                Ok(false) | Err(IncomingSmsError::Context | IncomingSmsError::Messaging(_)) => {
                    return Ok(reject(INVALID_MESSAGE_CAUSE));
                }
                Err(error) => return Err(error),
            }
            journal
        } else {
            if !matches!(decoded.segment.kind, SegmentKind::Begin { .. }) {
                return Ok(reject(INVALID_MESSAGE_CAUSE));
            }
            if self.incoming_sms.tracked.len() >= MAX_STREAMS
                || self
                    .core()
                    .store()
                    .active_incoming_journals(0, (MAX_STREAMS + 1) as u32)
                    .map_err(IncomingSmsError::Store)?
                    .len()
                    >= MAX_STREAMS
            {
                return Err(IncomingSmsError::Capacity);
            }
            let context = match Context::new(self.core(), peer, &addressed.relay.addresses) {
                Ok(context) => context,
                Err(IncomingSmsError::Context | IncomingSmsError::Messaging(_)) => {
                    return Ok(reject(INVALID_MESSAGE_CAUSE));
                }
                Err(error) => return Err(error),
            };
            let mut receiver = Receiver::try_new(
                peer,
                SERVICE,
                decoded.segment.transaction_id,
                common,
                LIMITS.maximum_message_octets,
                clock.now_monotonic_ms,
            )
            .map_err(|_| IncomingSmsError::Context)?;
            receiver
                .receive(peer, SERVICE, &decoded, clock.now_monotonic_ms)
                .map_err(|_| IncomingSmsError::Context)?;
            self.core_mut()
                .store_mut()
                .begin_incoming_journal(
                    IncomingBinding {
                        peer,
                        protocol: SERVICE,
                        transaction_id: decoded.segment.transaction_id,
                        version: common,
                        context: context.encode()?,
                    },
                    JOURNAL_LIMITS,
                    clock.now_unix_ms,
                )
                .map_err(IncomingSmsError::Store)?
        };
        if self
            .incoming_sms
            .tracked
            .iter()
            .find(|entry| entry.journal.handle == journal.handle)
            .and_then(|entry| entry.deadline_ms)
            .is_some_and(|deadline| clock.now_monotonic_ms > deadline)
        {
            self.apply_incoming(&journal, JournalEvent::Advance, now_ms)?;
            journal = self
                .core()
                .store()
                .incoming_journal(&journal.handle)
                .map_err(IncomingSmsError::Store)?;
        }
        let mut incoming =
            match self.apply_incoming(&journal, JournalEvent::Segment(decoded.segment), now_ms) {
                Ok(result) => result.incoming,
                Err(IncomingSmsError::Transition(error))
                    if matches!(
                        error.as_ref(),
                        tsar_ingress::Error::Ownership(crate::link::tsar::IncomingError::Receiver(
                            crate::link::tsar::ReceiveError::Closed
                        ))
                    ) =>
                {
                    return Ok(reject(INVALID_MESSAGE_CAUSE));
                }
                Err(error) => return Err(error),
            };
        journal = self
            .core()
            .store()
            .incoming_journal(&journal.handle)
            .map_err(IncomingSmsError::Store)?;
        let rejected = self.finish_incoming_application(&journal, &mut incoming, now_ms)?;
        Ok(if rejected {
            reject(INVALID_MESSAGE_CAUSE)
        } else {
            RelayMessage::Accept {
                transaction_id: *transaction_id,
            }
        })
    }
}
fn permanent_application_error(error: &MessagingError) -> bool {
    if let MessagingError::Store(StoreError::Hub(error)) = error {
        return !matches!(error, crate::core::store::sms_hub::HubError::QueueFull);
    }
    matches!(
        error,
        MessagingError::Sms(_)
            | MessagingError::Address
            | MessagingError::MessageType
            | MessagingError::UnsupportedOptions
            | MessagingError::UnknownDestination
            | MessagingError::Store(
                StoreError::SenderMismatch
                    | StoreError::Addressing(_)
                    | StoreError::RecipientMismatch
                    | StoreError::DisabledSubscriber
                    | StoreError::MissingSubscriber
            )
    )
}
