mod device_subscribers;
use super::{
    registry::{LocalRegistry, RegistrationReply, RegistryError},
    store::{
        AcknowledgmentKind, AcknowledgmentReference, NewSms, QueuedSms, SmsId, StateStore,
        StoreError,
    },
};
use crate::{
    l3::{
        address::{AddressInfo, AddressValue},
        registration::RegistrationResponse,
        relay::{
            Direction, HigherLayerProtocol, RelayMessage,
            addressing::{
                self, AddressedRelay, AddressingError, DecodedAddressedRelay, RelayAddresses,
            },
        },
        sms::{self, DisplayTime, SmsError, SmsMessage, Validity},
    },
    link::rach::{MessageSet, MobileIdentity},
};
use std::{error::Error, fmt};

const UNKNOWN_PLAN: u8 = 0;
const TELEPHONY_PLAN: u8 = 1;
const PRIVATE_PLAN: u8 = 9;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmsDestination {
    recipient: Option<MobileIdentity>,
    number: String,
}
impl SmsDestination {
    /// Restore the recipient and number from the immutable admission binding. New queue insertion checks both against SQLite provisioning.
    pub fn try_from_binding(
        recipient: MobileIdentity,
        number: String,
    ) -> Result<Self, MessagingError> {
        let (_, width, value) = recipient.parts();
        if value == 0
            || value >= 1 << width
            || number.is_empty()
            || !number.bytes().all(|digit| digit.is_ascii_digit())
        {
            return Err(MessagingError::Address);
        }
        Ok(Self {
            recipient: Some(recipient),
            number,
        })
    }
    pub fn recipient(&self) -> Option<MobileIdentity> {
        self.recipient
    }
    pub(crate) fn external(number: String) -> Result<Self, MessagingError> {
        if number.is_empty() || !number.bytes().all(|digit| digit.is_ascii_digit()) {
            return Err(MessagingError::Address);
        }
        Ok(Self {
            recipient: None,
            number,
        })
    }
    pub fn number(&self) -> &str {
        &self.number
    }
}
pub struct BoundSmsSubmission<'a> {
    pub sender: MobileIdentity,
    pub sender_number: &'a str,
    pub destination: &'a SmsDestination,
}
struct SubmissionParties<'a> {
    recipient: MobileIdentity,
    expected_recipient_number: Option<&'a str>,
    expected_sender_number: Option<&'a str>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryOffer {
    pub recipient: MobileIdentity,
    pub queued: QueuedSms,
    /// Cell time zone for the Message Center Time Stamp. Without it the DELIVER has no time stamp.
    pub time_zone: Option<crate::l3::time_date::TimeZoneOffset>,
}
impl DeliveryOffer {
    pub fn relay(&self, transaction_id: u8) -> AddressedRelay {
        AddressedRelay {
            message: RelayMessage::Data {
                transaction_id,
                protocol: HigherLayerProtocol::Sms,
                payload: self
                    .time_zone
                    .and_then(|time_zone| {
                        sms::with_time_stamp(
                            &self.queued.sms_pdu,
                            self.queued.created_at_unix_ms,
                            time_zone,
                        )
                    })
                    .unwrap_or_else(|| self.queued.sms_pdu.clone()),
            },
            addresses: RelayAddresses {
                originating: Some(AddressInfo {
                    number_type: 0,
                    numbering_plan: TELEPHONY_PLAN,
                    address: AddressValue::Tbcd(self.queued.sender_number.clone()),
                }),
                ..RelayAddresses::default()
            },
        }
    }
}
#[derive(Debug)]
pub struct Submission {
    pub id: SmsId,
    pub response: RelayMessage,
    pub delivery: Option<DeliveryOffer>,
}
#[derive(Debug)]
pub struct CompleteSmsSubmission {
    pub id: SmsId,
    pub delivery: Option<DeliveryOffer>,
}
enum SubmissionScope {
    SingleRelay,
    CompleteTeleservice,
}
#[derive(Debug)]
pub struct ReceiptDispatch {
    pub original_id: SmsId,
    pub receipt_id: SmsId,
    pub response: RelayMessage,
    pub delivery: Option<DeliveryOffer>,
}
#[derive(Debug)]
pub struct RegistrationDispatch {
    pub reply: RegistrationReply,
    pub pending: Vec<DeliveryOffer>,
}
#[derive(Debug)]
pub enum MessagingError {
    Store(StoreError),
    Registry(RegistryError),
    Addressing(AddressingError),
    Sms(SmsError),
    DeviceProvisioning(&'static str),
    UnknownSender,
    UnknownDestination,
    Address,
    Protocol,
    MessageType,
    UnsupportedOptions,
    UnknownReceipt,
    AmbiguousReceipt,
}
impl fmt::Display for MessagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "subscriber core request failed: {self:?}")
    }
}
impl Error for MessagingError {}
impl From<StoreError> for MessagingError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<RegistryError> for MessagingError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}
impl From<AddressingError> for MessagingError {
    fn from(error: AddressingError) -> Self {
        Self::Addressing(error)
    }
}
impl From<SmsError> for MessagingError {
    fn from(error: SmsError) -> Self {
        Self::Sms(error)
    }
}
#[derive(Debug)]
pub struct LocalMessaging {
    sms_hub_enabled: bool,
    display_time: DisplayTime,
    time_zone: Option<crate::l3::time_date::TimeZoneOffset>,
    sms_hub_status: Option<crate::sms_hub::StatusHandle>,
    devices: Vec<super::store::DeviceSubscriber>,
    registry: LocalRegistry,
    store: StateStore,
}
impl LocalMessaging {
    pub fn from_store(store: StateStore) -> Result<Self, MessagingError> {
        let registry = LocalRegistry::try_new(store.enabled_subscribers()?)?;
        let devices = store.device_subscribers()?;
        for device in &devices {
            let consistent = match device.identity {
                Some(identity) => registry
                    .by_identity(identity)
                    .is_some_and(|subscriber| subscriber.provisioning.number == device.number),
                None => registry.by_number(&device.number).is_none(),
            };
            if !consistent {
                return Err(StoreError::CorruptState.into());
            }
        }
        Ok(Self {
            registry,
            store,
            devices,
            sms_hub_enabled: false,
            display_time: crate::config::SmsDisplayTime::default().wire(),
            time_zone: None,
            sms_hub_status: None,
        })
    }
    pub(crate) fn set_sms_hub_status(&mut self, status: crate::sms_hub::StatusHandle) {
        self.sms_hub_status = Some(status);
    }
    pub(crate) fn sms_hub_status(&self) -> Option<crate::sms_hub::Status> {
        self.sms_hub_status
            .as_ref()
            .map(|s| s.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }
    pub(crate) fn set_allow_unknown_registration(&mut self, allow: bool) {
        self.registry.set_allow_unknown(allow);
    }
    pub(crate) fn set_display_time(&mut self, display_time: DisplayTime) {
        self.display_time = display_time;
    }
    pub(crate) fn set_time_zone(&mut self, time_zone: crate::l3::time_date::TimeZoneOffset) {
        self.time_zone = Some(time_zone);
    }
    pub(crate) fn time_zone(&self) -> Option<crate::l3::time_date::TimeZoneOffset> {
        self.time_zone
    }
    pub(crate) fn enable_sms_hub(&mut self, enabled: bool) {
        self.sms_hub_enabled = enabled;
    }
    pub(crate) fn power_down(&mut self, handle: &super::registry::RegistrationHandle) -> bool {
        self.registry.power_down(handle)
    }
    pub fn registry(&self) -> &LocalRegistry {
        &self.registry
    }
    pub(crate) fn record_capabilities(
        &mut self,
        identity: MobileIdentity,
        report: crate::l3::capability_report::CapabilityReport,
        superframe: u64,
    ) -> Result<(), RegistryError> {
        self.registry
            .record_capabilities(identity, report, superframe)
    }
    pub fn store(&self) -> &StateStore {
        &self.store
    }
    pub(crate) fn store_mut(&mut self) -> &mut StateStore {
        &mut self.store
    }
    pub(crate) fn save_subscriber(
        &mut self,
        subscriber: super::registry::ProvisionedSubscriber,
    ) -> Result<(), MessagingError> {
        let mut provisioning: Vec<_> = self
            .registry
            .subscribers()
            .iter()
            .filter(|old| old.provisioning.identity != subscriber.identity)
            .map(|old| old.provisioning.clone())
            .collect();
        provisioning.push(subscriber.clone());
        LocalRegistry::try_new(provisioning.clone())?;
        if self.devices.iter().any(|device| {
            device.number == subscriber.number && device.identity != Some(subscriber.identity)
        }) {
            return Err(RegistryError::DuplicateNumber.into());
        }
        if self
            .devices
            .iter()
            .any(|device| device.identity == Some(subscriber.identity))
        {
            return Err(MessagingError::DeviceProvisioning(
                "Edit this subscriber using its ESN",
            ));
        }
        self.store.save_subscriber(&subscriber)?;
        self.registry.replace_provisioning(provisioning)?;
        Ok(())
    }
    pub(crate) fn delete_subscriber(
        &mut self,
        identity: MobileIdentity,
    ) -> Result<(), MessagingError> {
        self.store.disable_subscriber(identity)?;
        let provisioning = self
            .registry
            .subscribers()
            .iter()
            .filter(|old| old.provisioning.identity != identity)
            .map(|old| old.provisioning.clone())
            .collect();
        self.registry.replace_provisioning(provisioning)?;
        self.devices = self.store.device_subscribers()?;
        Ok(())
    }
    pub fn register(
        &mut self,
        set: &MessageSet,
        superframe: u64,
    ) -> Result<RegistrationDispatch, MessagingError> {
        if let Some(reply) = self.resolve_device_registration(set, superframe)? {
            return Ok(RegistrationDispatch {
                reply,
                pending: Vec::new(),
            });
        }
        let reply = self.registry.register(set, superframe)?;
        let pending = if reply.message == RegistrationResponse::Accept {
            self.deliveries_for(set.identity)?
        } else {
            Vec::new()
        };
        Ok(RegistrationDispatch { reply, pending })
    }
    pub fn deliveries_for(
        &self,
        recipient: MobileIdentity,
    ) -> Result<Vec<DeliveryOffer>, MessagingError> {
        if !self.registry.subscribers().iter().any(|subscriber| {
            subscriber.provisioning.identity == recipient && subscriber.registration.is_some()
        }) {
            return Ok(Vec::new());
        }
        let mut offers = Vec::new();
        for queued in self.store.pending_for(recipient)? {
            if !self.store.outgoing_owns_sms(queued.id)? {
                offers.push(DeliveryOffer {
                    recipient,
                    queued,
                    time_zone: self.time_zone,
                });
            }
        }
        Ok(offers)
    }

    pub fn accept_delivery_ack(
        &mut self,
        actor: MobileIdentity,
        bytes: &[u8],
        created_at_unix_ms: i64,
    ) -> Result<ReceiptDispatch, MessagingError> {
        self.accept_acknowledgment(
            actor,
            addressing::decode(bytes, Direction::Reverse)?,
            created_at_unix_ms,
            AcknowledgmentKind::Delivery,
        )
    }
    pub fn accept_manual_ack(
        &mut self,
        actor: MobileIdentity,
        bytes: &[u8],
        created_at_unix_ms: i64,
    ) -> Result<ReceiptDispatch, MessagingError> {
        self.accept_acknowledgment(
            actor,
            addressing::decode(bytes, Direction::Reverse)?,
            created_at_unix_ms,
            AcknowledgmentKind::Manual,
        )
    }
    pub(crate) fn accept_acknowledgment(
        &mut self,
        actor: MobileIdentity,
        decoded: DecodedAddressedRelay,
        created_at_unix_ms: i64,
        expected: AcknowledgmentKind,
    ) -> Result<ReceiptDispatch, MessagingError> {
        if !self
            .registry
            .subscribers()
            .iter()
            .any(|subscriber| subscriber.provisioning.identity == actor)
        {
            return Err(MessagingError::UnknownSender);
        }
        if !decoded.unparsed_bits.is_empty()
            || decoded.relay.addresses.restricts_originating_presentation()
        {
            return Err(MessagingError::UnsupportedOptions);
        }
        let RelayMessage::Data {
            transaction_id,
            protocol,
            payload,
        } = decoded.relay.message
        else {
            return Err(MessagingError::MessageType);
        };
        if protocol != HigherLayerProtocol::Sms {
            return Err(MessagingError::Protocol);
        }
        let destination = decoded
            .relay
            .addresses
            .destination
            .as_ref()
            .ok_or(MessagingError::UnknownReceipt)?;
        let number = directory_number(destination)?;
        let acknowledgment = sms::decode_prefix(&payload, Direction::Reverse)?;
        if !acknowledgment.optional_octets.is_empty() {
            tracing::debug!(
                octets = acknowledgment.optional_octets.len(),
                "SMS acknowledgment optional elements ignored"
            );
        }
        let AcknowledgmentReference {
            kind,
            message_reference,
        } = AcknowledgmentKind::from_message(&acknowledgment.message)
            .map_err(|_| MessagingError::MessageType)?;
        if kind != expected {
            return Err(MessagingError::MessageType);
        }
        let mut matched = None;
        for queued in self.store.receipt_candidates_for(actor)? {
            if queued.sender_number != number {
                continue;
            }
            let SmsMessage::Deliver { metadata, .. } =
                sms::decode_prefix(&queued.sms_pdu, Direction::Forward)?.message
            else {
                continue;
            };
            if metadata.message_reference != message_reference || !kind.requested(&metadata) {
                continue;
            }
            if matched.replace(queued.id).is_some() {
                return Err(MessagingError::AmbiguousReceipt);
            }
        }
        let original_id = matched.ok_or(MessagingError::UnknownReceipt)?;
        let receipt_id = self.store.record_acknowledgment(
            original_id,
            actor,
            &acknowledgment.message,
            created_at_unix_ms,
        )?;
        let mut delivery = None;
        for subscriber in self.registry.subscribers() {
            if subscriber.registration.is_none() {
                continue;
            }
            if let Some(queued) = self
                .store
                .pending_for(subscriber.provisioning.identity)?
                .into_iter()
                .find(|queued| queued.id == receipt_id)
            {
                delivery = Some(DeliveryOffer {
                    recipient: subscriber.provisioning.identity,
                    queued,
                    time_zone: self.time_zone,
                });
                break;
            }
        }
        Ok(ReceiptDispatch {
            original_id,
            receipt_id,
            response: RelayMessage::Accept { transaction_id },
            delivery,
        })
    }

    /// The caller supplies a stable submission key. Air-interface replay windows are handled separately.
    pub fn submit_mobile_sms(
        &mut self,
        sender: MobileIdentity,
        bytes: &[u8],
        submission_key: &str,
        created_at_unix_ms: i64,
    ) -> Result<Submission, MessagingError> {
        self.submit_addressed_sms(
            sender,
            addressing::decode(bytes, Direction::Reverse)?,
            submission_key,
            created_at_unix_ms,
        )
    }
    pub(crate) fn submit_addressed_sms(
        &mut self,
        sender: MobileIdentity,
        decoded: DecodedAddressedRelay,
        submission_key: &str,
        created_at_unix_ms: i64,
    ) -> Result<Submission, MessagingError> {
        if !self
            .registry
            .subscribers()
            .iter()
            .any(|subscriber| subscriber.provisioning.identity == sender)
        {
            return Err(MessagingError::UnknownSender);
        }
        if !decoded.unparsed_bits.is_empty()
            || decoded.relay.addresses.restricts_originating_presentation()
        {
            return Err(MessagingError::UnsupportedOptions);
        }
        let RelayMessage::Data {
            transaction_id,
            protocol,
            payload,
        } = decoded.relay.message
        else {
            return Err(MessagingError::MessageType);
        };
        if protocol != HigherLayerProtocol::Sms {
            return Err(MessagingError::Protocol);
        }
        let destination = decoded
            .relay
            .addresses
            .destination
            .as_ref()
            .ok_or(MessagingError::UnknownDestination)?;
        let stored = self.submit_sms_contents(
            sender,
            destination,
            &payload,
            submission_key,
            created_at_unix_ms,
            SubmissionScope::SingleRelay,
        )?;
        // IS-136.1 §§7.1.2.1,6.4.3.12 permit acceptance after durable submission.
        Ok(Submission {
            id: stored.id,
            response: RelayMessage::Accept { transaction_id },
            delivery: stored.delivery,
        })
    }
    pub fn bind_sms_destination(
        &self,
        destination: &AddressInfo,
    ) -> Result<SmsDestination, MessagingError> {
        let number = directory_number(destination)?;
        if let Some(subscriber) = self.registry.by_number(&number) {
            return SmsDestination::try_from_binding(subscriber.provisioning.identity, number);
        }
        if self.sms_hub_enabled && !self.store.number_is_local(&number)? {
            return SmsDestination::external(number);
        }
        Err(MessagingError::UnknownDestination)
    }
    /// Persist this destination binding at admission. Exact committed retries retain their SMS ID even after provisioning changes.
    pub fn submit_bound_sms(
        &mut self,
        sender: MobileIdentity,
        destination: &SmsDestination,
        payload: &[u8],
        submission_key: &str,
        created_at_unix_ms: i64,
    ) -> Result<CompleteSmsSubmission, MessagingError> {
        if destination.recipient.is_none() {
            return self.submit_external(
                sender,
                destination,
                None,
                payload,
                submission_key,
                created_at_unix_ms,
            );
        }
        self.submit_to(
            sender,
            SubmissionParties {
                recipient: destination
                    .recipient
                    .ok_or(MessagingError::UnknownDestination)?,
                expected_recipient_number: Some(&destination.number),
                expected_sender_number: None,
            },
            payload,
            submission_key,
            created_at_unix_ms,
            SubmissionScope::CompleteTeleservice,
        )
    }
    /// Retain the admitted party numbers across retries. Existing records compare those original values rather than current provisioning.
    pub fn submit_frozen_sms(
        &mut self,
        binding: BoundSmsSubmission<'_>,
        payload: &[u8],
        submission_key: &str,
        created_at_unix_ms: i64,
    ) -> Result<CompleteSmsSubmission, MessagingError> {
        if binding.destination.recipient.is_none() {
            return self.submit_external(
                binding.sender,
                binding.destination,
                Some(binding.sender_number),
                payload,
                submission_key,
                created_at_unix_ms,
            );
        }
        self.submit_to(
            binding.sender,
            SubmissionParties {
                recipient: binding
                    .destination
                    .recipient
                    .ok_or(MessagingError::UnknownDestination)?,
                expected_recipient_number: Some(&binding.destination.number),
                expected_sender_number: Some(binding.sender_number),
            },
            payload,
            submission_key,
            created_at_unix_ms,
            SubmissionScope::CompleteTeleservice,
        )
    }
    /// Supply the complete CMT PDU and validated destination, with a stable key across application retries and restarts. This does not acknowledge a radio transaction.
    pub fn submit_complete_sms(
        &mut self,
        sender: MobileIdentity,
        destination: &AddressInfo,
        payload: &[u8],
        submission_key: &str,
        created_at_unix_ms: i64,
    ) -> Result<CompleteSmsSubmission, MessagingError> {
        self.submit_sms_contents(
            sender,
            destination,
            payload,
            submission_key,
            created_at_unix_ms,
            SubmissionScope::CompleteTeleservice,
        )
    }
    fn submit_sms_contents(
        &mut self,
        sender: MobileIdentity,
        destination: &AddressInfo,
        payload: &[u8],
        submission_key: &str,
        created_at_unix_ms: i64,
        scope: SubmissionScope,
    ) -> Result<CompleteSmsSubmission, MessagingError> {
        if !self
            .registry
            .subscribers()
            .iter()
            .any(|subscriber| subscriber.provisioning.identity == sender)
        {
            return Err(MessagingError::UnknownSender);
        }
        let destination = self.bind_sms_destination(destination)?;
        let Some(recipient) = destination.recipient else {
            return self.submit_external(
                sender,
                &destination,
                None,
                payload,
                submission_key,
                created_at_unix_ms,
            );
        };
        self.submit_to(
            sender,
            SubmissionParties {
                recipient,
                expected_recipient_number: None,
                expected_sender_number: None,
            },
            payload,
            submission_key,
            created_at_unix_ms,
            scope,
        )
    }
    fn submit_external(
        &mut self,
        sender: MobileIdentity,
        destination: &SmsDestination,
        sender_number: Option<&str>,
        payload: &[u8],
        key: &str,
        created_at_unix_ms: i64,
    ) -> Result<CompleteSmsSubmission, MessagingError> {
        let id = self
            .store
            .enqueue_external_sms(super::store::sms_hub::ExternalSubmission {
                sender,
                expected_sender: sender_number,
                destination: destination.number(),
                payload,
                key,
                created_ms: created_at_unix_ms,
                enabled: self.sms_hub_enabled,
            })?;
        Ok(CompleteSmsSubmission { id, delivery: None })
    }
    fn submit_to(
        &mut self,
        sender: MobileIdentity,
        parties: SubmissionParties<'_>,
        payload: &[u8],
        submission_key: &str,
        created_at_unix_ms: i64,
        scope: SubmissionScope,
    ) -> Result<CompleteSmsSubmission, MessagingError> {
        let recipient = parties.recipient;
        let decoded_sms = sms::decode_prefix(payload, Direction::Reverse)?;
        if !decoded_sms.optional_octets.is_empty() {
            tracing::debug!(
                octets = decoded_sms.optional_octets.len(),
                "SMS SUBMIT optional elements ignored"
            );
        }
        let SmsMessage::Submit {
            metadata,
            user_data,
        } = decoded_sms.message
        else {
            return Err(MessagingError::MessageType);
        };
        let deliver = SmsMessage::Deliver {
            metadata,
            validity: Validity::Indefinite,
            display_time: self.display_time,
            user_data,
        };
        let sms = NewSms {
            dedup_key: submission_key,
            sender,
            recipient,
            message: &deliver,
            created_at_unix_ms,
        };
        let id = match scope {
            SubmissionScope::SingleRelay => self.store.enqueue(sms)?,
            SubmissionScope::CompleteTeleservice => match parties.expected_recipient_number {
                Some(number) => self.store.enqueue_bound_teleservice(
                    sms,
                    number,
                    parties.expected_sender_number,
                )?,
                None => self.store.enqueue_teleservice(sms)?,
            },
        };
        let delivery = self
            .deliveries_for(recipient)?
            .into_iter()
            .find(|offer| offer.queued.id == id);
        Ok(CompleteSmsSubmission { id, delivery })
    }
}
pub(crate) fn directory_number(address: &AddressInfo) -> Result<String, MessagingError> {
    if !matches!(
        address.numbering_plan,
        UNKNOWN_PLAN | TELEPHONY_PLAN | PRIVATE_PLAN
    ) {
        return Err(MessagingError::Address);
    }
    let number = match &address.address {
        AddressValue::Tbcd(number) => number.clone(),
        AddressValue::Ira(bytes) => {
            String::from_utf8(bytes.clone()).map_err(|_| MessagingError::Address)?
        }
        AddressValue::DpcSsn(_) | AddressValue::BinaryInternet(_) => {
            return Err(MessagingError::Address);
        }
    };
    if number.is_empty() || !number.bytes().all(|digit| digit.is_ascii_digit()) {
        return Err(MessagingError::Address);
    }
    Ok(number)
}

#[cfg(test)]
mod nokia_tests {
    use super::*;
    use crate::core::registry::ProvisionedSubscriber;
    #[test]
    fn nokia_mo_sms_with_presentation_allowed_is_durably_queued() {
        let sender = MobileIdentity::Min34(3410151054);
        let recipient = MobileIdentity::Min34(2);
        let mut store = StateStore::open(":memory:").unwrap();
        store
            .reconcile(&[
                ProvisionedSubscriber {
                    identity: sender,
                    number: "1001".into(),
                },
                ProvisionedSubscriber {
                    identity: recipient,
                    number: "22222".into(),
                },
            ])
            .unwrap();
        let mut core = LocalMessaging::from_store(store).unwrap();
        let bytes = [
            0x09, 0x01, 0x07, 0x01, 0x00, 0x00, 0x09, 0x02, 0x08, 0x4a, 0x20, 0x40, 0x12, 0x22,
            0x22, 0x04, 0x06, 0x01, 0x31, 0x43, 0x78, 0x17, 0x65, 0x60,
        ];
        let decoded = addressing::decode(&bytes, Direction::Reverse).unwrap();
        let submitted = core
            .submit_addressed_sms(sender, decoded, "nokia-mo", 0)
            .unwrap();
        assert_eq!(
            submitted.response,
            RelayMessage::Accept { transaction_id: 1 }
        );
        let queued = core.store().pending_for(recipient).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].sender_number, "1001");
        let decoded = sms::decode_prefix(&queued[0].sms_pdu, Direction::Forward).unwrap();
        let SmsMessage::Deliver {
            user_data,
            display_time,
            ..
        } = decoded.message
        else {
            panic!()
        };
        assert_eq!(display_time, DisplayTime::Invoke);
        assert_eq!(user_data.text().as_deref(), Some("J"));
        let mut restricted = bytes;
        *restricted.last_mut().unwrap() = 0x64;
        let result = core.submit_addressed_sms(
            sender,
            addressing::decode(&restricted, Direction::Reverse).unwrap(),
            "restricted",
            0,
        );
        assert!(matches!(result, Err(MessagingError::UnsupportedOptions)));
        assert_eq!(core.store().pending_for(recipient).unwrap().len(), 1);
    }
}
