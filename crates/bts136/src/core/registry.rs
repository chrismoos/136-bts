mod activity;
use crate::{
    l3::{
        broadcast::ProtocolVersion,
        capability_report::{self, CapabilityReport, ReportError},
        registration::{
            self, AcceptOptions, AccessMessage, OptionsError, RegistrationError,
            RegistrationOptions, RegistrationRequest, RegistrationResponse, RegistrationType,
            RejectCause,
        },
    },
    link::rach::{MessageSet, MobileIdentity},
};
pub(crate) use activity::CallActivity;
use std::{collections::HashMap, error::Error, fmt, sync::Arc};
const NO_MESSAGE_ENCRYPTION: u8 = 0;
const ENCRYPTION_DOMAIN_A: u8 = 1;
const ENCRYPTION_KEY_A: u8 = 1;
const BASELINE_PFC_MINUS_ONE: u8 = 0;
const DIRECTORY_NUMBER_REQUESTED: u8 = 1;
const UNKNOWN_NUMBER_TYPE: u8 = 0;
const TELEPHONY_PLAN: u8 = 1;
const MAX_UNPROVISIONED_REGISTRATIONS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedSubscriber {
    pub identity: MobileIdentity,
    pub number: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration {
    pub protocol_version: ProtocolVersion,
    pub scm: u8,
    pub esn: Option<u32>,
    pub last_registration_superframe: u64,
    pub last_observed_access_superframe: Option<u64>,
    pub access_voice_coder_map: Option<u8>,
}
#[derive(Debug, Clone)]
pub struct RegistrationHandle {
    identity: MobileIdentity,
    owner: Arc<()>,
}
impl RegistrationHandle {
    pub fn identity(&self) -> MobileIdentity {
        self.identity
    }
}
#[derive(Debug, Clone)]
pub(crate) struct SubscriberHandle {
    identity: MobileIdentity,
    owner: Arc<()>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedCapabilities {
    pub report: CapabilityReport,
    pub received_superframe: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscriber {
    pub provisioning: ProvisionedSubscriber,
    pub registration: Option<Registration>,
    pub capabilities: Option<ReportedCapabilities>,
    pub registration_options: Option<RegistrationOptions>,
    pub reported_serial: Option<registration::SerialNumber>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationReply {
    pub identity: MobileIdentity,
    pub message: RegistrationResponse,
    pub options: AcceptOptions,
}
impl RegistrationReply {
    pub fn encode(&self) -> Result<Vec<u8>, OptionsError> {
        match self.message {
            RegistrationResponse::Accept => self.options.encode(),
            RegistrationResponse::Reject { cause } if self.options == AcceptOptions::default() => {
                crate::l3::call_control::Response::registration_reject(cause).encode()
            }
            _ => Err(OptionsError::Field("reject_accept_options")),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    Identity,
    Number,
    DuplicateIdentity,
    DuplicateNumber,
    MissingRegistration,
    DuplicateMessage,
    Wire(RegistrationError),
    Options(OptionsError),
    Capability(ReportError),
    CapabilityContext,
    UnknownSubscriber,
    NotRegistered,
    SerialConflict,
    CallAdmission(crate::l3::access_call::AdmissionError),
}
impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "subscriber registry failed: {self:?}")
    }
}
impl Error for RegistryError {}
#[derive(Debug)]
pub struct LocalRegistry {
    subscribers: Vec<Subscriber>,
    unprovisioned: Vec<Subscriber>,
    allow_unknown: bool,
    registration_owners: HashMap<MobileIdentity, Arc<()>>,
    subscriber_owners: HashMap<MobileIdentity, Arc<()>>,
}
impl LocalRegistry {
    pub fn try_new(provisioning: Vec<ProvisionedSubscriber>) -> Result<Self, RegistryError> {
        let mut subscribers: Vec<Subscriber> = Vec::new();
        for subscriber in provisioning {
            let (_, width, value) = subscriber.identity.parts();
            if value == 0 || value >= 1 << width {
                return Err(RegistryError::Identity);
            }
            if subscriber.number.is_empty()
                || !subscriber
                    .number
                    .bytes()
                    .all(|digit| digit.is_ascii_digit())
            {
                return Err(RegistryError::Number);
            }
            if subscribers
                .iter()
                .any(|existing| existing.provisioning.identity == subscriber.identity)
            {
                return Err(RegistryError::DuplicateIdentity);
            }
            if subscribers
                .iter()
                .any(|existing| existing.provisioning.number == subscriber.number)
            {
                return Err(RegistryError::DuplicateNumber);
            }
            subscribers.push(Subscriber {
                provisioning: subscriber,
                registration: None,
                capabilities: None,
                registration_options: None,
                reported_serial: None,
            });
        }
        let subscriber_owners = subscribers
            .iter()
            .map(|subscriber| (subscriber.provisioning.identity, Arc::new(())))
            .collect();
        Ok(Self {
            subscriber_owners,
            unprovisioned: Vec::new(),
            allow_unknown: false,
            subscribers,
            registration_owners: HashMap::new(),
        })
    }
    pub(crate) fn replace_provisioning(
        &mut self,
        provisioning: Vec<ProvisionedSubscriber>,
    ) -> Result<(), RegistryError> {
        let mut next = Self::try_new(provisioning)?;
        for subscriber in &mut next.subscribers {
            subscriber.reported_serial = self
                .subscribers
                .iter()
                .chain(self.unprovisioned.iter())
                .find(|old| old.provisioning.identity == subscriber.provisioning.identity)
                .and_then(|old| old.reported_serial.clone());
            subscriber.registration_options = self
                .subscribers
                .iter()
                .chain(self.unprovisioned.iter())
                .find(|old| old.provisioning.identity == subscriber.provisioning.identity)
                .and_then(|old| old.registration_options.clone());
            subscriber.capabilities = self
                .subscribers
                .iter()
                .chain(self.unprovisioned.iter())
                .find(|old| old.provisioning.identity == subscriber.provisioning.identity)
                .and_then(|old| old.capabilities.clone());
            subscriber.registration = self
                .subscribers
                .iter()
                .chain(self.unprovisioned.iter())
                .find(|old| old.provisioning.identity == subscriber.provisioning.identity)
                .and_then(|old| old.registration);
        }
        for subscriber in &next.subscribers {
            if self
                .subscribers
                .iter()
                .any(|old| old.provisioning == subscriber.provisioning)
                && let Some(owner) = self
                    .subscriber_owners
                    .get(&subscriber.provisioning.identity)
            {
                next.subscriber_owners
                    .insert(subscriber.provisioning.identity, owner.clone());
            }
            if subscriber.registration.is_some() {
                let identity = subscriber.provisioning.identity;
                let owner = self
                    .subscribers
                    .iter()
                    .find(|old| old.provisioning == subscriber.provisioning)
                    .and_then(|_| self.registration_owners.get(&identity))
                    .cloned()
                    .unwrap_or_else(|| Arc::new(()));
                next.registration_owners.insert(identity, owner);
            }
        }
        self.unprovisioned
            .retain(|phone| next.by_identity(phone.provisioning.identity).is_none());
        self.subscriber_owners = next.subscriber_owners;
        self.registration_owners = next.registration_owners;
        self.subscribers = next.subscribers;
        Ok(())
    }
    pub(crate) fn subscriber_handle(&self, identity: MobileIdentity) -> Option<SubscriberHandle> {
        Some(SubscriberHandle {
            identity,
            owner: self.subscriber_owners.get(&identity)?.clone(),
        })
    }
    pub(crate) fn subscriber_current(&self, handle: &SubscriberHandle) -> bool {
        self.subscriber_owners
            .get(&handle.identity)
            .is_some_and(|owner| Arc::ptr_eq(owner, &handle.owner))
    }
    pub fn registration_handle(&self, identity: MobileIdentity) -> Option<RegistrationHandle> {
        self.subscribers
            .iter()
            .find(|subscriber| subscriber.provisioning.identity == identity)?
            .registration?;
        Some(RegistrationHandle {
            identity,
            owner: self.registration_owners.get(&identity)?.clone(),
        })
    }
    pub fn registration_current(&self, handle: &RegistrationHandle) -> bool {
        self.registration_owners
            .get(&handle.identity)
            .is_some_and(|owner| Arc::ptr_eq(owner, &handle.owner))
    }
    pub(crate) fn power_down(&mut self, handle: &RegistrationHandle) -> bool {
        if !self.registration_current(handle) {
            return false;
        }
        let Some(subscriber) = self
            .subscribers
            .iter_mut()
            .find(|subscriber| subscriber.provisioning.identity == handle.identity)
        else {
            return false;
        };
        self.registration_owners.remove(&handle.identity);
        clear_registration(subscriber);
        true
    }
    pub(crate) fn set_allow_unknown(&mut self, allow: bool) {
        self.allow_unknown = allow;
    }
    pub(crate) fn unprovisioned_phone(&self, identity: MobileIdentity) -> Option<&Subscriber> {
        self.unprovisioned
            .iter()
            .find(|phone| phone.provisioning.identity == identity)
    }
    pub(crate) fn unprovisioned_by_esn(&self, esn: u32) -> Option<MobileIdentity> {
        self.unprovisioned
            .iter()
            .find(|phone| {
                phone
                    .registration
                    .is_some_and(|registration| registration.esn == Some(esn))
            })
            .map(|phone| phone.provisioning.identity)
    }
    pub fn subscribers(&self) -> &[Subscriber] {
        &self.subscribers
    }
    pub(crate) fn by_identity(&self, identity: MobileIdentity) -> Option<&Subscriber> {
        self.subscribers
            .iter()
            .find(|subscriber| subscriber.provisioning.identity == identity)
    }
    pub fn by_number(&self, number: &str) -> Option<&Subscriber> {
        self.subscribers
            .iter()
            .find(|subscriber| subscriber.provisioning.number == number)
    }

    pub(crate) fn record_capabilities(
        &mut self,
        identity: MobileIdentity,
        report: CapabilityReport,
        superframe: u64,
    ) -> Result<(), RegistryError> {
        report.encode().map_err(RegistryError::Capability)?;
        let subscriber = self
            .subscribers
            .iter_mut()
            .find(|subscriber| subscriber.provisioning.identity == identity)
            .ok_or(RegistryError::UnknownSubscriber)?;
        if subscriber.registration.is_none() {
            return Err(RegistryError::NotRegistered);
        }
        subscriber.capabilities = Some(ReportedCapabilities {
            report,
            received_superframe: superframe,
        });
        Ok(())
    }
    /// Input must be an assembled, CRC-validated RACH message set.
    pub fn register(
        &mut self,
        set: &MessageSet,
        superframe: u64,
    ) -> Result<RegistrationReply, RegistryError> {
        let mut request = None;
        let mut serial = None;
        let mut capabilities = None;
        for bytes in &set.messages {
            if bytes.first() == Some(&capability_report::MESSAGE_TYPE) {
                if request.is_none() {
                    return Err(RegistryError::MissingRegistration);
                }
                let report = capability_report::decode(bytes).map_err(RegistryError::Capability)?;
                if capabilities.replace(report).is_some() {
                    return Err(RegistryError::DuplicateMessage);
                }
                continue;
            }
            match registration::decode_prefix(bytes) {
                Ok(decoded) => match decoded.message {
                    AccessMessage::Registration { .. } => {
                        let decoded =
                            RegistrationRequest::decode(bytes).map_err(RegistryError::Options)?;
                        if request.replace(decoded).is_some() {
                            return Err(RegistryError::DuplicateMessage);
                        }
                    }
                    AccessMessage::SerialNumber { .. } => {
                        if request.is_none() {
                            return Err(RegistryError::MissingRegistration);
                        }
                        let report = registration::SerialNumber::decode(bytes)
                            .map_err(RegistryError::Options)?;
                        if serial.replace(report).is_some() {
                            return Err(RegistryError::DuplicateMessage);
                        }
                    }
                },
                Err(error) => return Err(RegistryError::Wire(error)),
            }
        }
        let RegistrationRequest {
            registration_type,
            scm,
            protocol_version,
            options,
        } = request.ok_or(RegistryError::MissingRegistration)?;
        if let Some(serial) = &serial {
            serial
                .validate_version(protocol_version)
                .map_err(RegistryError::Options)?;
        }
        if capabilities.is_some()
            && !matches!(
                registration_type,
                RegistrationType::PowerUp | RegistrationType::NewSystem | RegistrationType::Forced
            )
        {
            return Err(RegistryError::CapabilityContext);
        }
        let reject = |cause| RegistrationReply {
            identity: set.identity,
            message: RegistrationResponse::Reject { cause },
            options: AcceptOptions::default(),
        };
        if options.encryption.is_some_and(|mode| {
            mode.algorithm != NO_MESSAGE_ENCRYPTION
                || mode.domain != ENCRYPTION_DOMAIN_A
                || mode.key != ENCRYPTION_KEY_A
        }) {
            return Ok(reject(RejectCause::EncryptionModeTemporarilyDenied));
        }
        if options.group.is_some() || registration_type == RegistrationType::UserGroup {
            return Ok(reject(RejectCause::UserGroupBarred));
        }
        if options.selected_system.is_some()
            || options.system_value.is_some()
            || options.target_soc.is_some()
            || options.c_number.is_some()
            || options.multilingual_number.is_some()
            || options.subaddress.is_some()
        {
            return Ok(reject(RejectCause::AddressingNotSupported));
        }
        let (_, width, value) = set.identity.parts();
        if value == 0 || value >= 1 << width {
            return Err(RegistryError::Identity);
        }
        let known = self
            .subscribers
            .iter()
            .any(|subscriber| subscriber.provisioning.identity == set.identity);
        if !known && !self.allow_unknown {
            return Ok(reject(RejectCause::UnknownMsid));
        }
        if !known && self.unprovisioned_phone(set.identity).is_none() {
            if self.unprovisioned.len() == MAX_UNPROVISIONED_REGISTRATIONS {
                self.unprovisioned.remove(0);
            }
            self.unprovisioned.push(Subscriber {
                provisioning: ProvisionedSubscriber {
                    identity: set.identity,
                    number: String::new(),
                },
                registration: None,
                capabilities: None,
                registration_options: None,
                reported_serial: None,
            });
        }
        let subscriber = self
            .subscribers
            .iter_mut()
            .chain(self.unprovisioned.iter_mut())
            .find(|subscriber| subscriber.provisioning.identity == set.identity)
            .ok_or(RegistryError::UnknownSubscriber)?;
        let reply = RegistrationReply {
            identity: set.identity,
            message: RegistrationResponse::Accept,
            options: AcceptOptions {
                // The cell implements only paging frame class 1.
                pfc: options.pfc.map(|_| BASELINE_PFC_MINUS_ONE),
                directory_address: (known
                    && options.number_request == Some(DIRECTORY_NUMBER_REQUESTED))
                .then(|| crate::l3::address::AddressInfo {
                    number_type: UNKNOWN_NUMBER_TYPE,
                    numbering_plan: TELEPHONY_PLAN,
                    address: crate::l3::address::AddressValue::Tbcd(
                        subscriber.provisioning.number.clone(),
                    ),
                }),
                ..Default::default()
            },
        };
        if reply.encode().is_err() {
            return Ok(reject(RejectCause::AddressingNotSupported));
        }
        if matches!(
            registration_type,
            RegistrationType::PowerDown | RegistrationType::Deregistration
        ) {
            self.registration_owners.remove(&set.identity);
            clear_registration(subscriber);
        } else {
            subscriber.registration_options = Some(options);
            if let Some(report) = capabilities {
                subscriber.capabilities = Some(ReportedCapabilities {
                    report,
                    received_superframe: superframe,
                });
            }
            let previous_esn = subscriber
                .registration
                .and_then(|registration| registration.esn);
            let esn = serial.as_ref().map(|serial| serial.esn).or(previous_esn);
            if let Some(serial) = serial {
                subscriber.reported_serial = Some(serial);
            }
            if known {
                self.registration_owners.insert(set.identity, Arc::new(()));
            }
            subscriber.registration = Some(Registration {
                protocol_version,
                scm,
                esn,
                last_registration_superframe: superframe,
                last_observed_access_superframe: None,
                access_voice_coder_map: subscriber
                    .registration
                    .and_then(|registration| registration.access_voice_coder_map),
            });
        }
        Ok(reply)
    }
}
fn clear_registration(subscriber: &mut Subscriber) {
    subscriber.registration = None;
    subscriber.capabilities = None;
    subscriber.registration_options = None;
    subscriber.reported_serial = None;
}
#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn request(identity: MobileIdentity, kind: RegistrationType) -> MessageSet {
        MessageSet {
            identity,
            messages: vec![
                AccessMessage::Registration {
                    registration_type: kind,
                    scm: 21,
                    protocol_version: ProtocolVersion::Is136RevA,
                }
                .encode()
                .unwrap(),
            ],
        }
    }
    #[test]
    fn provisioned_identity_registers_refreshes_and_deregisters_without_authentication() {
        let identity = MobileIdentity::Min34(1);
        let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
            identity,
            number: "1001".into(),
        }])
        .unwrap();
        let mut set = request(identity, RegistrationType::PowerUp);
        set.messages
            .push(AccessMessage::SerialNumber { esn: 42 }.encode().unwrap());
        assert_eq!(
            registry.register(&set, 100).unwrap().message,
            RegistrationResponse::Accept
        );
        assert_eq!(
            registry
                .by_number("1001")
                .unwrap()
                .registration
                .unwrap()
                .esn,
            Some(42)
        );
        registry
            .register(&request(identity, RegistrationType::Periodic), 200)
            .unwrap();
        let state = registry.by_number("1001").unwrap().registration.unwrap();
        assert_eq!(state.esn, Some(42));
        assert_eq!(state.last_registration_superframe, 200);
        registry
            .register(&request(identity, RegistrationType::PowerDown), 201)
            .unwrap();
        assert!(registry.by_number("1001").unwrap().registration.is_none());
        registry
            .register(&request(identity, RegistrationType::PowerUp), 202)
            .unwrap();
        registry
            .register(&request(identity, RegistrationType::Deregistration), 203)
            .unwrap();
        assert!(registry.by_number("1001").unwrap().registration.is_none());
    }
    #[test]
    fn unknown_identity_and_malformed_message_sets_do_not_mutate_provisioned_state() {
        let identity = MobileIdentity::Min34(1);
        let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
            identity,
            number: "1001".into(),
        }])
        .unwrap();
        assert_eq!(
            registry
                .register(
                    &request(MobileIdentity::Min34(2), RegistrationType::PowerUp),
                    0
                )
                .unwrap()
                .message,
            RegistrationResponse::Reject {
                cause: RejectCause::UnknownMsid
            }
        );
        let mut duplicate = request(identity, RegistrationType::PowerUp);
        duplicate.messages.push(duplicate.messages[0].clone());
        assert_eq!(
            registry.register(&duplicate, 0),
            Err(RegistryError::DuplicateMessage)
        );
        assert!(registry.subscribers()[0].registration.is_none());
        assert_eq!(
            registry.register(
                &MessageSet {
                    identity,
                    messages: vec![]
                },
                0
            ),
            Err(RegistryError::MissingRegistration)
        );
        assert!(matches!(
            LocalRegistry::try_new(vec![ProvisionedSubscriber {
                identity,
                number: "".into()
            }]),
            Err(RegistryError::Number)
        ));
    }
}
#[cfg(test)]
mod capability_tests {
    use super::*;
    use crate::l3::capability_report::{ReportOptions, SupportedServices};
    fn report() -> CapabilityReport {
        CapabilityReport {
            protocol_version: ProtocolVersion::Pv1,
            scm: 15,
            software_vintage: 2,
            firmware_vintage: 3,
            model: 4,
            manufacturer: 0x20,
            max_supported_pfc: 5,
            supported_bands: 3,
            services: SupportedServices {
                ira: true,
                ..Default::default()
            },
            options: ReportOptions::default(),
        }
    }
    fn set(kind: RegistrationType, report: Option<Vec<u8>>) -> MessageSet {
        let mut messages = vec![
            AccessMessage::Registration {
                registration_type: kind,
                scm: 15,
                protocol_version: ProtocolVersion::Is136RevA,
            }
            .encode()
            .unwrap(),
        ];
        if let Some(report) = report {
            messages.push(report);
        }
        MessageSet {
            identity: MobileIdentity::Min34(1),
            messages,
        }
    }
    fn registry() -> LocalRegistry {
        LocalRegistry::try_new(vec![ProvisionedSubscriber {
            identity: MobileIdentity::Min34(1),
            number: "1001".into(),
        }])
        .unwrap()
    }
    #[test]
    fn registration_capabilities_remain_reported_metadata_and_survive_provisioning_edits() {
        let mut registry = registry();
        let report = report();
        registry
            .register(
                &set(RegistrationType::PowerUp, Some(report.encode().unwrap())),
                10,
            )
            .unwrap();
        let subscriber = &registry.subscribers()[0];
        assert_eq!(
            subscriber.registration.unwrap().protocol_version,
            ProtocolVersion::Is136RevA
        );
        assert_eq!(subscriber.capabilities.as_ref().unwrap().report, report);
        assert_eq!(
            subscriber
                .capabilities
                .as_ref()
                .unwrap()
                .received_superframe,
            10
        );
        registry
            .replace_provisioning(vec![ProvisionedSubscriber {
                identity: MobileIdentity::Min34(1),
                number: "2001".into(),
            }])
            .unwrap();
        registry
            .register(&set(RegistrationType::Periodic, None), 20)
            .unwrap();
        assert_eq!(
            registry.subscribers()[0]
                .capabilities
                .as_ref()
                .unwrap()
                .received_superframe,
            10
        );
        registry
            .register(&set(RegistrationType::PowerDown, None), 30)
            .unwrap();
        assert!(registry.subscribers()[0].capabilities.is_none());
        assert!(registry.subscribers()[0].registration.is_none());
    }
    #[test]
    fn invalid_or_duplicate_capability_reports_do_not_change_registration_or_metadata() {
        let mut registry = registry();
        registry
            .register(
                &set(RegistrationType::PowerUp, Some(report().encode().unwrap())),
                10,
            )
            .unwrap();
        let previous = registry.subscribers()[0].clone();
        let mut duplicate = set(RegistrationType::Forced, Some(report().encode().unwrap()));
        duplicate.messages.push(report().encode().unwrap());
        assert_eq!(
            registry.register(&duplicate, 20),
            Err(RegistryError::DuplicateMessage)
        );
        assert!(matches!(
            registry.register(&set(RegistrationType::Forced, Some(vec![5])), 20),
            Err(RegistryError::Capability(_))
        ));
        assert_eq!(
            registry.register(
                &set(RegistrationType::Periodic, Some(report().encode().unwrap())),
                20
            ),
            Err(RegistryError::CapabilityContext)
        );
        assert_eq!(registry.subscribers()[0], previous);
    }
}
#[cfg(test)]
mod standalone_capability_tests {
    use super::*;
    #[test]
    fn standalone_capabilities_update_metadata_without_creating_or_refreshing_registration() {
        let id = MobileIdentity::Min34(1);
        let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
            identity: id,
            number: "1001".into(),
        }])
        .unwrap();
        let report =
            capability_report::decode(&[5, 0x57, 0x84, 0x1a, 0x10, 0x50, 0, 0xe0, 0]).unwrap();
        assert_eq!(
            registry.record_capabilities(id, report.clone(), 1),
            Err(RegistryError::NotRegistered)
        );
        assert!(registry.subscribers()[0].registration.is_none());
        registry
            .register(
                &MessageSet {
                    identity: id,
                    messages: vec![
                        AccessMessage::Registration {
                            registration_type: RegistrationType::PowerUp,
                            scm: 15,
                            protocol_version: ProtocolVersion::Is136RevA,
                        }
                        .encode()
                        .unwrap(),
                    ],
                },
                2,
            )
            .unwrap();
        let previous = registry.subscribers()[0].registration;
        registry
            .record_capabilities(id, report.clone(), 20)
            .unwrap();
        assert_eq!(registry.subscribers()[0].registration, previous);
        assert_eq!(
            registry.subscribers()[0]
                .capabilities
                .as_ref()
                .unwrap()
                .received_superframe,
            20
        );
        assert_eq!(
            registry.record_capabilities(MobileIdentity::Min34(2), report, 30),
            Err(RegistryError::UnknownSubscriber)
        );
    }
}
#[cfg(test)]
mod option_tests;
#[cfg(test)]
mod extended_capability_tests {
    use super::*;
    #[test]
    fn malformed_later_capability_fields_preserve_all_registration_and_report_state() {
        let identity = MobileIdentity::Min34(1);
        let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
            identity,
            number: "1001".into(),
        }])
        .unwrap();
        let wire = vec![
            0x05, 0xb7, 0x84, 0x1a, 0x10, 0x50, 0x00, 0xe0, 0x38, 0xdc, 0x80, 0xe4, 0x5c, 0xce,
            0x88, 0xe7, 0x90, 0xba, 0x87, 0xbb, 0x00, 0xbb, 0xf8, 0x00, 0x00,
        ];
        let mut set = MessageSet {
            identity,
            messages: vec![
                AccessMessage::Registration {
                    registration_type: RegistrationType::PowerUp,
                    scm: 15,
                    protocol_version: ProtocolVersion::Is136RevA,
                }
                .encode()
                .unwrap(),
                wire,
            ],
        };
        registry.register(&set, 10).unwrap();
        let previous = registry.subscribers()[0].clone();
        assert_eq!(
            previous.registration.unwrap().protocol_version,
            ProtocolVersion::Is136RevA
        );
        assert_eq!(
            previous
                .capabilities
                .as_ref()
                .unwrap()
                .report
                .options
                .extensions
                .voice
                .as_ref()
                .unwrap()
                .modes[0]
                .coder,
            6
        );
        *set.messages[1].last_mut().unwrap() = 0x80;
        assert!(matches!(
            registry.register(&set, 20),
            Err(RegistryError::Capability(_))
        ));
        assert_eq!(registry.subscribers()[0], previous);
        let mut report = previous.capabilities.as_ref().unwrap().report.clone();
        report.options.extensions.multilingual = Some(1);
        assert!(matches!(
            registry.record_capabilities(identity, report, 20),
            Err(RegistryError::Capability(_))
        ));
        assert_eq!(registry.subscribers()[0], previous);
    }
    #[test]
    fn power_down_ownership_survives_only_unchanged_provisioning() {
        let identity = MobileIdentity::Min34(1);
        let provisioning = ProvisionedSubscriber {
            identity,
            number: "1001".into(),
        };
        let mut registry = LocalRegistry::try_new(vec![provisioning.clone()]).unwrap();
        let registration = super::tests::request(identity, RegistrationType::PowerUp);
        registry.register(&registration, 0).unwrap();
        let old = registry.registration_handle(identity).unwrap();
        let snapshot = registry.subscribers()[0].registration;
        registry.register(&registration, 0).unwrap();
        assert_eq!(registry.subscribers()[0].registration, snapshot);
        assert!(!registry.power_down(&old));
        let current = registry.registration_handle(identity).unwrap();
        registry
            .replace_provisioning(vec![provisioning.clone()])
            .unwrap();
        assert!(registry.registration_current(&current));
        let mut foreign = LocalRegistry::try_new(vec![provisioning.clone()]).unwrap();
        foreign.register(&registration, 0).unwrap();
        assert!(!foreign.power_down(&current));
        registry
            .replace_provisioning(vec![ProvisionedSubscriber {
                identity,
                number: "2001".into(),
            }])
            .unwrap();
        assert!(!registry.power_down(&current));
        let renamed = registry.registration_handle(identity).unwrap();
        registry.replace_provisioning(vec![]).unwrap();
        registry.replace_provisioning(vec![provisioning]).unwrap();
        registry.register(&registration, 0).unwrap();
        assert!(!registry.power_down(&renamed));
        let current = registry.registration_handle(identity).unwrap();
        assert!(registry.power_down(&current));
        assert!(!registry.power_down(&current));
        assert!(registry.registration_handle(identity).is_none());
        assert!(registry.subscribers()[0].registration.is_none());
    }
}

#[cfg(test)]
mod concatenation_tests;
