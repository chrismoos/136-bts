use super::registry::LocalRegistry;
use crate::{
    l3::{
        access_call::{self, AdmissionError, OriginationRequest},
        address::{AddressInfo, AddressValue},
        designation::AssignedSlots,
        origination::{self, Origination, OriginationError},
        page_response::ServiceCode,
    },
    link::rach::{MessageSet, MobileIdentity},
    resources::{AllocationError, TrafficLease, TrafficResources},
};
use std::{error::Error, fmt, sync::Arc};

const UNKNOWN_NUMBERING_PLAN: u8 = 0;
const TELEPHONY_NUMBERING_PLAN: u8 = 1;
const PRIVATE_NUMBERING_PLAN: u8 = 9;
const FIRST_CALL_ID: u64 = 1;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallId(u64);
impl CallId {
    pub fn value(self) -> u64 {
        self.0
    }
}
#[derive(Debug, Clone)]
pub struct CallHandle {
    id: CallId,
    token: Arc<()>,
}
impl CallHandle {
    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.id == other.id && Arc::ptr_eq(&self.token, &other.token)
    }
    pub fn id(&self) -> CallId {
        self.id
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservedParty {
    pub identity: MobileIdentity,
    pub number: String,
    pub channel: u16,
    pub assigned_slots: AssignedSlots,
    pub codec: crate::codec::speech::Codec,
    pub supported_codecs: u8,
    pub protocol_version: Option<crate::l3::broadcast::ProtocolVersion>,
}
struct CallStation {
    identity: MobileIdentity,
    number: String,
    codec: crate::codec::speech::Codec,
    supported_codecs: u8,
    protocol_version: Option<crate::l3::broadcast::ProtocolVersion>,
}
struct OriginatingCall {
    destination: String,
    caller: CallStation,
}
#[derive(Debug, Clone)]
pub struct CallReservation {
    pub handle: CallHandle,
    pub caller: ReservedParty,
    pub callee: ReservedParty,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    UnknownCaller,
    CallerOffline,
    UnknownDestination,
    DestinationOffline,
    Number,
    SelfCall,
    Busy(MobileIdentity),
    Allocation(AllocationError),
    StaleHandle,
    IdExhausted,
    Origination(OriginationError),
    MissingOrigination,
    DuplicateOrigination,
    OptionalElements,
    Options(access_call::OptionsError),
    Admission(AdmissionError),
    Service(ServiceCode),
}
impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "local call admission failed: {self:?}")
    }
}
impl Error for CallError {}
struct ReservedCall {
    reservation: CallReservation,
    leases: [TrafficLease; 2],
}
mod audio;
pub use audio::{AudioDirection, AudioReservation, OriginationRoute};

pub struct CallReservations {
    resources: TrafficResources,
    calls: Vec<ReservedCall>,
    audio: Vec<audio::ReservedAudioCall>,
    next_id: u64,
}
impl CallReservations {
    pub fn new(resources: TrafficResources) -> Self {
        Self {
            resources,
            calls: Vec::new(),
            audio: Vec::new(),
            next_id: FIRST_CALL_ID,
        }
    }
    pub fn reservations(&self) -> impl Iterator<Item = &CallReservation> {
        self.calls.iter().map(|call| &call.reservation)
    }
    pub fn originate(
        &mut self,
        registry: &LocalRegistry,
        set: &MessageSet,
    ) -> Result<CallReservation, CallError> {
        let call = originating_request(registry, set)?;
        self.reserve_originating(registry, call.caller, &call.destination)
    }
    pub fn reserve(
        &mut self,
        registry: &LocalRegistry,
        caller: MobileIdentity,
        destination: &str,
    ) -> Result<CallReservation, CallError> {
        if destination.is_empty() || !destination.bytes().all(|digit| digit.is_ascii_digit()) {
            return Err(CallError::Number);
        }
        let caller = registry
            .subscribers()
            .iter()
            .find(|subscriber| subscriber.provisioning.identity == caller)
            .ok_or(CallError::UnknownCaller)?;
        if caller.registration.is_none() {
            return Err(CallError::CallerOffline);
        }
        let caller = CallStation {
            identity: caller.provisioning.identity,
            number: caller.provisioning.number.clone(),
            codec: reported_codec(caller)?,
            supported_codecs: reported_mask(caller),
            protocol_version: caller
                .registration
                .map(|registration| registration.protocol_version),
        };
        self.reserve_originating(registry, caller, destination)
    }
    fn reserve_originating(
        &mut self,
        registry: &LocalRegistry,
        caller: CallStation,
        destination: &str,
    ) -> Result<CallReservation, CallError> {
        let callee = registry
            .by_number(destination)
            .ok_or(CallError::UnknownDestination)?;
        if caller.identity == callee.provisioning.identity {
            return Err(CallError::SelfCall);
        }
        if callee.registration.is_none() {
            return Err(CallError::DestinationOffline);
        }
        for identity in [caller.identity, callee.provisioning.identity] {
            if self.busy(identity) {
                return Err(CallError::Busy(identity));
            }
        }
        let callee_codec = reported_codec(callee)?;
        let next_id = self.next_id.checked_add(1).ok_or(CallError::IdExhausted)?;
        let leases = self
            .resources
            .allocate_full_rate_pair()
            .map_err(CallError::Allocation)?;
        let reservation = CallReservation {
            handle: CallHandle {
                id: CallId(self.next_id),
                token: Arc::new(()),
            },
            caller: ReservedParty {
                identity: caller.identity,
                number: caller.number.clone(),
                channel: leases[0].channel(),
                assigned_slots: leases[0].assigned_slots(),
                codec: caller.codec,
                supported_codecs: caller.supported_codecs,
                protocol_version: caller.protocol_version,
            },
            callee: ReservedParty {
                identity: callee.provisioning.identity,
                number: callee.provisioning.number.clone(),
                channel: leases[1].channel(),
                assigned_slots: leases[1].assigned_slots(),
                codec: callee_codec,
                supported_codecs: reported_mask(callee),
                protocol_version: callee
                    .registration
                    .map(|registration| registration.protocol_version),
            },
        };
        self.calls.push(ReservedCall {
            reservation: reservation.clone(),
            leases,
        });
        self.next_id = next_id;
        Ok(reservation)
    }
    pub fn select_callee_codec(
        &mut self,
        handle: &CallHandle,
        options: &crate::l3::access_call::CallOptions,
    ) -> Result<CallReservation, CallError> {
        let call = self
            .calls
            .iter_mut()
            .find(|call| call.reservation.handle.matches(handle))
            .ok_or(CallError::StaleHandle)?;
        let supported: Vec<_> = [
            crate::codec::speech::Codec::AcelpCc1,
            crate::codec::speech::Codec::Vselp,
        ]
        .into_iter()
        .filter(|codec| call.reservation.callee.supported_codecs & codec.capability_mask() != 0)
        .collect();
        let codec = options
            .select_codec(&supported)
            .map_err(CallError::Admission)?;
        call.reservation.callee.codec = codec;
        Ok(call.reservation.clone())
    }
    /// Release only after radio procedures have stopped both bearers or before either assignment was transmitted.
    pub fn release(&mut self, handle: &CallHandle) -> Result<CallReservation, CallError> {
        let index = self
            .calls
            .iter()
            .position(|call| call.reservation.handle.matches(handle))
            .ok_or(CallError::StaleHandle)?;
        self.resources
            .release_full_rate_pair(&self.calls[index].leases)
            .map_err(CallError::Allocation)?;
        Ok(self.calls.remove(index).reservation)
    }
}
fn originating_request(
    registry: &LocalRegistry,
    set: &MessageSet,
) -> Result<OriginatingCall, CallError> {
    let mut request: Option<Origination> = None;
    for message in &set.messages {
        match origination::decode_prefix(message) {
            Ok(decoded) => {
                if request.replace(decoded.message).is_some() {
                    return Err(CallError::DuplicateOrigination);
                }
            }
            Err(OriginationError::MessageType) => {}
            Err(error) => return Err(CallError::Origination(error)),
        }
    }
    let request = request.ok_or(CallError::MissingOrigination)?;
    if !matches!(
        request.service_code,
        ServiceCode::DigitalSpeech | ServiceCode::DigitalPreferred | ServiceCode::AnalogPreferred
    ) {
        return Err(CallError::Service(request.service_code));
    }
    let primary = set
        .messages
        .iter()
        .find(|bytes| bytes.first() == Some(&access_call::ORIGINATION_TYPE))
        .ok_or(CallError::MissingOrigination)?;
    let decoded = OriginationRequest::decode(primary).map_err(|error| {
        if error == access_call::OptionsError::Field("optional_reserved") {
            CallError::OptionalElements
        } else {
            CallError::Options(error)
        }
    })?;
    access_call::validate_companions(
        set,
        access_call::Kind::Origination,
        request.protocol_version,
    )
    .map_err(CallError::Options)?;
    let reported = registry
        .subscribers()
        .iter()
        .find(|subscriber| subscriber.provisioning.identity == set.identity)
        .and_then(|subscriber| subscriber.capabilities.as_ref())
        .and_then(|capabilities| capabilities.report.voice_coder_map());
    let supported: Vec<_> = [
        crate::codec::speech::Codec::AcelpCc1,
        crate::codec::speech::Codec::Vselp,
    ]
    .into_iter()
    .filter(|codec| reported.is_none_or(|map| map & codec.capability_mask() != 0))
    .collect();
    let codec = decoded
        .options
        .select_codec(&supported)
        .map_err(CallError::Admission)?;
    if let Some(calling) = &decoded.options.calling_party {
        number(calling).map_err(|_| CallError::Admission(AdmissionError::Addressing))?;
    }
    let destination = number(&request.called_party)?.to_owned();
    let caller_number = registry
        .by_identity(set.identity)
        .map(|subscriber| subscriber.provisioning.number.clone())
        .or_else(|| set.identity.min_digits())
        .or_else(|| {
            decoded
                .options
                .calling_party
                .as_ref()
                .and_then(|address| number(address).ok())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let supported_codecs = supported
        .iter()
        .fold(0, |mask, codec| mask | codec.capability_mask());
    Ok(OriginatingCall {
        destination,
        caller: CallStation {
            identity: set.identity,
            number: caller_number,
            codec,
            supported_codecs,
            protocol_version: Some(
                registry
                    .by_identity(set.identity)
                    .and_then(|subscriber| subscriber.registration)
                    .map_or(request.protocol_version, |registration| {
                        registration
                            .protocol_version
                            .common(request.protocol_version)
                    }),
            ),
        },
    })
}

fn reported_mask(subscriber: &crate::core::registry::Subscriber) -> u8 {
    subscriber
        .capabilities
        .as_ref()
        .and_then(|value| value.report.voice_coder_map())
        .or_else(|| {
            subscriber
                .registration
                .and_then(|registration| registration.access_voice_coder_map)
        })
        .unwrap_or(
            crate::codec::speech::Codec::Vselp.capability_mask()
                | crate::codec::speech::Codec::AcelpCc1.capability_mask(),
        )
}

fn reported_codec(
    subscriber: &crate::core::registry::Subscriber,
) -> Result<crate::codec::speech::Codec, CallError> {
    use crate::codec::speech::Codec;
    let map = subscriber
        .capabilities
        .as_ref()
        .and_then(|value| value.report.voice_coder_map())
        .or_else(|| {
            subscriber
                .registration
                .and_then(|registration| registration.access_voice_coder_map)
        });
    match map {
        None => Ok(Codec::Vselp),
        Some(map) => [Codec::AcelpCc1, Codec::Vselp]
            .into_iter()
            .find(|codec| map & codec.capability_mask() != 0)
            .ok_or(CallError::Admission(AdmissionError::Codec)),
    }
}

fn number(address: &AddressInfo) -> Result<&str, CallError> {
    if !matches!(
        address.numbering_plan,
        UNKNOWN_NUMBERING_PLAN | TELEPHONY_NUMBERING_PLAN | PRIVATE_NUMBERING_PLAN
    ) {
        return Err(CallError::Number);
    }
    match &address.address {
        AddressValue::Tbcd(number)
            if !number.is_empty() && number.bytes().all(|digit| digit.is_ascii_digit()) =>
        {
            Ok(number)
        }
        _ => Err(CallError::Number),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::registry::ProvisionedSubscriber,
        l3::{
            broadcast::ProtocolVersion,
            registration::{AccessMessage, RegistrationType},
        },
        phy::channel::Band,
    };
    pub(super) fn registered_registry(online: &[u64]) -> LocalRegistry {
        let mut registry = LocalRegistry::try_new(
            (1..=3)
                .map(|value| ProvisionedSubscriber {
                    identity: MobileIdentity::Min34(value),
                    number: format!("100{value}"),
                })
                .collect(),
        )
        .unwrap();
        for value in online {
            let set = MessageSet {
                identity: MobileIdentity::Min34(*value),
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 21,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            };
            registry.register(&set, 0).unwrap();
        }
        registry
    }
    pub(super) fn call_reservations() -> CallReservations {
        CallReservations::new(
            TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
        )
    }
    pub(super) fn request() -> Origination {
        Origination {
            protocol_version: ProtocolVersion::Is136RevA,
            emergency_call: false,
            called_party: AddressInfo {
                number_type: 0,
                numbering_plan: TELEPHONY_NUMBERING_PLAN,
                address: AddressValue::Tbcd("1002".into()),
            },
            last_try: false,
            scm: 21,
            service_code: ServiceCode::DigitalSpeech,
        }
    }
    pub(super) fn set(request: &Origination) -> MessageSet {
        MessageSet {
            identity: MobileIdentity::Min34(1),
            messages: vec![request.encode().unwrap()],
        }
    }
    #[test]
    fn local_calls_reserve_two_disjoint_bearers_and_busy_or_stale_attempts_preserve_them() {
        let registry = registered_registry(&[1, 2, 3]);
        let mut calls = call_reservations();
        let first = calls
            .reserve(&registry, MobileIdentity::Min34(1), "1002")
            .unwrap();
        assert_eq!(first.caller.identity, MobileIdentity::Min34(1));
        assert_eq!(first.callee.identity, MobileIdentity::Min34(2));
        assert_eq!(first.caller.assigned_slots, AssignedSlots::Full25);
        assert_eq!(first.callee.assigned_slots, AssignedSlots::Full36);
        assert!(matches!(
            calls.reserve(&registry, MobileIdentity::Min34(2), "1003"),
            Err(CallError::Busy(MobileIdentity::Min34(2)))
        ));
        assert!(matches!(
            calls.reserve(&registry, MobileIdentity::Min34(3), "1002"),
            Err(CallError::Busy(MobileIdentity::Min34(2)))
        ));
        calls.release(&first.handle).unwrap();
        let next = calls
            .reserve(&registry, MobileIdentity::Min34(2), "1001")
            .unwrap();
        assert_ne!(next.handle.id(), first.handle.id());
        assert!(matches!(
            calls.release(&first.handle),
            Err(CallError::StaleHandle)
        ));
        assert_eq!(calls.reservations().count(), 1);
        calls.release(&next.handle).unwrap();
        assert_eq!(calls.reservations().count(), 0);
    }
    #[test]
    fn unavailable_self_unknown_and_invalid_routes_do_not_allocate_any_bearer() {
        let registry = registered_registry(&[1]);
        let mut calls = call_reservations();
        for (caller, number, expected) in [
            (4, "1002", CallError::UnknownCaller),
            (2, "1001", CallError::CallerOffline),
            (1, "1002", CallError::DestinationOffline),
            (1, "1001", CallError::SelfCall),
            (1, "1999", CallError::UnknownDestination),
            (1, "100*", CallError::Number),
        ] {
            assert!(
                matches!(calls.reserve(&registry, MobileIdentity::Min34(caller), number), Err(error) if error == expected)
            );
        }
        assert_eq!(calls.reservations().count(), 0);
        let online = registered_registry(&[1, 2]);
        let admitted = calls
            .reserve(&online, MobileIdentity::Min34(1), "1002")
            .unwrap();
        assert_eq!(admitted.handle.id().value(), FIRST_CALL_ID);
    }
    #[test]
    fn prior_instance_handles_with_the_same_numeric_id_cannot_release_a_new_reservation() {
        let registry = registered_registry(&[1, 2]);
        let mut old = call_reservations();
        let old_call = old
            .reserve(&registry, MobileIdentity::Min34(1), "1002")
            .unwrap();
        let mut new = call_reservations();
        let new_call = new
            .reserve(&registry, MobileIdentity::Min34(1), "1002")
            .unwrap();
        assert_eq!(new_call.handle.id(), old_call.handle.id());
        assert!(matches!(
            new.release(&old_call.handle),
            Err(CallError::StaleHandle)
        ));
        assert_eq!(new.reservations().count(), 1);
        new.release(&new_call.handle).unwrap();
    }
    #[test]
    fn wire_origination_admission_rejects_duplicates_reserved_fields_and_unimplemented_services() {
        let registry = registered_registry(&[1, 2]);
        let mut calls = call_reservations();
        let request = request();
        let mut duplicate = set(&request);
        duplicate.messages.push(duplicate.messages[0].clone());
        assert!(matches!(
            calls.originate(&registry, &duplicate),
            Err(CallError::DuplicateOrigination)
        ));
        let mut extension = set(&request);
        extension.messages[0].push(0);
        assert!(matches!(
            calls.originate(&registry, &extension),
            Err(CallError::OptionalElements)
        ));
        let mut analog = request.clone();
        analog.service_code = ServiceCode::AnalogSpeech;
        assert!(matches!(
            calls.originate(&registry, &set(&analog)),
            Err(CallError::Service(ServiceCode::AnalogSpeech))
        ));
        assert_eq!(calls.reservations().count(), 0);
        let mut emergency = request.clone();
        emergency.emergency_call = true;
        let admitted = calls.originate(&registry, &set(&emergency)).unwrap();
        assert_eq!(admitted.callee.number, "1002");
    }
    #[test]
    fn insufficient_pair_capacity_and_exhausted_identifiers_leave_existing_capacity_untouched() {
        let registry = registered_registry(&[1, 2]);
        let resources = TrafficResources::try_new(
            Band::Cellular800,
            1,
            &[AssignedSlots::Full14, AssignedSlots::Full25],
        )
        .unwrap();
        let mut calls = CallReservations::new(resources);
        assert!(matches!(
            calls.reserve(&registry, MobileIdentity::Min34(1), "1002"),
            Err(CallError::Allocation(AllocationError::Exhausted))
        ));
        assert!(calls.resources.allocate_full_rate().is_ok());
        let mut calls = call_reservations();
        calls.next_id = u64::MAX;
        assert!(matches!(
            calls.reserve(&registry, MobileIdentity::Min34(1), "1002"),
            Err(CallError::IdExhausted)
        ));
        assert!(calls.resources.allocate_full_rate_pair().is_ok());
    }
    #[test]
    fn explicit_voice_preferences_allocate_only_after_complete_supported_admission() {
        use crate::l3::access_call::{
            AdmissionError, Bandwidth, CallOptions, LegacyVoice, OriginationRequest,
        };
        let registry = registered_registry(&[1, 2]);
        let mut calls = call_reservations();
        let mut request = OriginationRequest {
            message: request(),
            options: CallOptions {
                voices: vec![LegacyVoice {
                    coder: 3,
                    privacy: 0,
                }],
                bandwidth: Some(Bandwidth::FullOnly),
                ..Default::default()
            },
        };
        let make_set = |request: &OriginationRequest| MessageSet {
            identity: MobileIdentity::Min34(1),
            messages: vec![request.encode().unwrap()],
        };
        assert!(matches!(
            calls.originate(&registry, &make_set(&request)),
            Err(CallError::Admission(AdmissionError::Codec))
        ));
        assert_eq!(calls.reservations().count(), 0);
        request.options.voices.push(LegacyVoice {
            coder: 1,
            privacy: 0,
        });
        let mut invalid = make_set(&request);
        invalid.messages.push(vec![13, 0]);
        assert!(calls.originate(&registry, &invalid).is_err());
        assert_eq!(calls.reservations().count(), 0);
        let mut wrong_order = make_set(&request);
        wrong_order.messages.insert(
            0,
            crate::l3::registration::AccessMessage::SerialNumber { esn: 42 }
                .encode()
                .unwrap(),
        );
        assert!(calls.originate(&registry, &wrong_order).is_err());
        assert_eq!(calls.reservations().count(), 0);
        request.options.calling_party = Some(AddressInfo {
            number_type: 0,
            numbering_plan: 3,
            address: AddressValue::Tbcd("9999".into()),
        });
        assert!(matches!(
            calls.originate(&registry, &make_set(&request)),
            Err(CallError::Admission(AdmissionError::Addressing))
        ));
        assert_eq!(calls.reservations().count(), 0);
        request
            .options
            .calling_party
            .as_mut()
            .unwrap()
            .numbering_plan = 1;
        request.message.service_code = ServiceCode::AnalogPreferred;
        let call = calls.originate(&registry, &make_set(&request)).unwrap();
        assert_eq!(call.handle.id().value(), FIRST_CALL_ID);
        assert_eq!(call.caller.number, "1001");
        calls.release(&call.handle).unwrap();
        assert_eq!(calls.reservations().count(), 0);
    }
}
