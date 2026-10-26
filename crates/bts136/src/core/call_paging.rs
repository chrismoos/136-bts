use std::{error::Error, fmt};

use super::{
    calls::{CallError, CallHandle, CallReservation, CallReservations},
    registry::LocalRegistry,
};
use crate::{
    l3::page_response::{self, PageResponse, PageResponseError, ServiceCode},
    link::rach::{MessageSet, MobileIdentity},
    resources::TrafficResources,
};

#[derive(Debug, Clone)]
pub enum CallStart {
    Paging(PagingRequest),
    Audio(super::calls::AudioReservation),
}
#[derive(Debug, Clone)]
pub struct PagingRequest {
    pub handle: CallHandle,
    pub recipient: MobileIdentity,
    pub deadline_monotonic_ms: u64,
}
#[derive(Debug, Clone)]
pub enum PagingOutcome {
    Ready {
        reservation: CallReservation,
        response: PageResponse,
    },
    AudioReady {
        reservation: super::calls::AudioReservation,
        response: PageResponse,
    },
    AudioRejected(super::calls::AudioReservation),
    AudioExpired(super::calls::AudioReservation),
    Rejected(CallReservation),
    Expired(CallReservation),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagingError {
    Call(CallError),
    Wire(PageResponseError),
    ClockRegression,
    TimeoutDuration,
    TimeOverflow,
    UnknownAttempt,
    Recipient,
    NotTransmitted,
    MissingResponse,
    DuplicateResponse,
    Options(crate::l3::access_call::OptionsError),
    Admission(crate::l3::access_call::AdmissionError),
    Service,
    StillPaging,
    OutputCapacity,
}
impl fmt::Display for PagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "call paging failed: {self:?}")
    }
}
impl Error for PagingError {}
struct PendingPage {
    request: PagingRequest,
    transmitted: bool,
}

/// No traffic assignment may be transmitted until a Ready outcome hands off its reservation.
pub struct CallPaging {
    calls: CallReservations,
    pending: Vec<PendingPage>,
    last_monotonic_ms: u64,
}
impl CallPaging {
    pub fn new(resources: TrafficResources) -> Self {
        Self {
            calls: CallReservations::new(resources),
            pending: Vec::new(),
            last_monotonic_ms: 0,
        }
    }
    pub fn requests(&self) -> impl Iterator<Item = &PagingRequest> {
        self.pending.iter().map(|page| &page.request)
    }
    pub fn reservations(&self) -> impl Iterator<Item = &CallReservation> {
        self.calls.reservations()
    }
    /// The absolute policy deadline includes time waiting for the page to reach the air.
    pub fn begin(
        &mut self,
        registry: &LocalRegistry,
        origination: &MessageSet,
        monotonic_ms: u64,
        setup_timeout_ms: u64,
    ) -> Result<PagingRequest, PagingError> {
        self.check_clock(monotonic_ms)?;
        if setup_timeout_ms == 0 {
            return Err(PagingError::TimeoutDuration);
        }
        let deadline_monotonic_ms = monotonic_ms
            .checked_add(setup_timeout_ms)
            .ok_or(PagingError::TimeOverflow)?;
        let reservation = self
            .calls
            .originate(registry, origination)
            .map_err(PagingError::Call)?;
        let request = PagingRequest {
            handle: reservation.handle,
            recipient: reservation.callee.identity,
            deadline_monotonic_ms,
        };
        self.pending.push(PendingPage {
            request: request.clone(),
            transmitted: false,
        });
        self.last_monotonic_ms = monotonic_ms;
        Ok(request)
    }
    pub fn begin_audio(
        &mut self,
        registry: &LocalRegistry,
        identity: MobileIdentity,
        calling_number: &str,
        monotonic_ms: u64,
        setup_timeout_ms: u64,
    ) -> Result<PagingRequest, PagingError> {
        self.check_clock(monotonic_ms)?;
        if setup_timeout_ms == 0 {
            return Err(PagingError::TimeoutDuration);
        }
        let deadline_monotonic_ms = monotonic_ms
            .checked_add(setup_timeout_ms)
            .ok_or(PagingError::TimeOverflow)?;
        let reservation = self
            .calls
            .reserve_audio(
                registry,
                identity,
                calling_number,
                super::calls::AudioDirection::MobileTerminated,
                None,
            )
            .map_err(PagingError::Call)?;
        let request = PagingRequest {
            handle: reservation.handle,
            recipient: identity,
            deadline_monotonic_ms,
        };
        self.pending.push(PendingPage {
            request: request.clone(),
            transmitted: false,
        });
        self.last_monotonic_ms = monotonic_ms;
        Ok(request)
    }
    pub fn is_audio(&self, handle: &CallHandle) -> bool {
        self.calls
            .audio_reservations()
            .any(|call| call.handle.matches(handle))
    }
    pub fn cancel_audio(
        &mut self,
        handle: &CallHandle,
        monotonic_ms: u64,
    ) -> Result<super::calls::AudioReservation, PagingError> {
        self.check_clock(monotonic_ms)?;
        let index = self.index(handle)?;
        let reservation = self
            .calls
            .release_audio(handle)
            .map_err(PagingError::Call)?;
        self.pending.remove(index);
        self.last_monotonic_ms = monotonic_ms;
        Ok(reservation)
    }
    pub fn begin_route(
        &mut self,
        registry: &LocalRegistry,
        origination: &MessageSet,
        monotonic_ms: u64,
        setup_timeout_ms: u64,
    ) -> Result<CallStart, PagingError> {
        self.check_clock(monotonic_ms)?;
        if setup_timeout_ms == 0 {
            return Err(PagingError::TimeoutDuration);
        }
        let deadline_monotonic_ms = monotonic_ms
            .checked_add(setup_timeout_ms)
            .ok_or(PagingError::TimeOverflow)?;
        let route = self
            .calls
            .route_origination(registry, origination)
            .map_err(PagingError::Call)?;
        self.last_monotonic_ms = monotonic_ms;
        match route {
            super::calls::OriginationRoute::SampleAudio(reservation) => {
                Ok(CallStart::Audio(reservation))
            }
            super::calls::OriginationRoute::Mobile(reservation) => {
                let request = PagingRequest {
                    handle: reservation.handle,
                    recipient: reservation.callee.identity,
                    deadline_monotonic_ms,
                };
                self.pending.push(PendingPage {
                    request: request.clone(),
                    transmitted: false,
                });
                Ok(CallStart::Paging(request))
            }
        }
    }
    pub fn audio_reservations(&self) -> impl Iterator<Item = &super::calls::AudioReservation> {
        self.calls.audio_reservations()
    }
    /// The physical bearer and its queued radio work must be stopped before releasing its lease.
    pub fn release_audio_stopped(
        &mut self,
        handle: &CallHandle,
    ) -> Result<super::calls::AudioReservation, PagingError> {
        if self.index(handle).is_ok() {
            return Err(PagingError::StillPaging);
        }
        self.calls.release_audio(handle).map_err(PagingError::Call)
    }
    /// Report actual page transmission. Repeated transmissions do not extend the deadline.
    pub fn transmitted(
        &mut self,
        handle: &CallHandle,
        monotonic_ms: u64,
    ) -> Result<Option<PagingOutcome>, PagingError> {
        self.check_clock(monotonic_ms)?;
        let index = self.index(handle)?;
        if monotonic_ms >= self.pending[index].request.deadline_monotonic_ms {
            return self.expire(index, monotonic_ms).map(Some);
        }
        self.pending[index].transmitted = true;
        self.last_monotonic_ms = monotonic_ms;
        Ok(None)
    }
    /// Input is an assembled CRC-validated RACH message set.
    pub fn response(
        &mut self,
        handle: &CallHandle,
        set: &MessageSet,
        monotonic_ms: u64,
    ) -> Result<PagingOutcome, PagingError> {
        self.check_clock(monotonic_ms)?;
        let index = self.index(handle)?;
        let pending = &self.pending[index];
        if set.identity != pending.request.recipient {
            return Err(PagingError::Recipient);
        }
        if monotonic_ms >= pending.request.deadline_monotonic_ms {
            return self.expire(index, monotonic_ms);
        }
        if !pending.transmitted {
            return Err(PagingError::NotTransmitted);
        }
        let response = decode_response(set)?;
        let outcome = match response.service_code {
            ServiceCode::DigitalSpeech
            | ServiceCode::DigitalPreferred
            | ServiceCode::AnalogPreferred => {
                let primary = set
                    .messages
                    .iter()
                    .find(|message| {
                        message.first() == Some(&crate::l3::access_call::PAGE_RESPONSE_TYPE)
                    })
                    .ok_or(PagingError::MissingResponse)?;
                let options = crate::l3::access_call::PageResponseRequest::decode(primary)
                    .map_err(PagingError::Options)?
                    .options;
                if self.is_audio(handle) {
                    let reservation = self
                        .calls
                        .select_audio_codec(handle, &options)
                        .map_err(map_call_error)?;
                    PagingOutcome::AudioReady {
                        reservation,
                        response,
                    }
                } else {
                    let reservation = self
                        .calls
                        .select_callee_codec(handle, &options)
                        .map_err(map_call_error)?;
                    PagingOutcome::Ready {
                        reservation,
                        response,
                    }
                }
            }
            ServiceCode::ServiceRejected => {
                if self.is_audio(handle) {
                    PagingOutcome::AudioRejected(
                        self.calls
                            .release_audio(handle)
                            .map_err(PagingError::Call)?,
                    )
                } else {
                    PagingOutcome::Rejected(self.calls.release(handle).map_err(PagingError::Call)?)
                }
            }
            _ => return Err(PagingError::Service),
        };
        self.pending.remove(index);
        self.last_monotonic_ms = monotonic_ms;
        Ok(outcome)
    }
    pub fn advance(&mut self, monotonic_ms: u64) -> Result<Vec<PagingOutcome>, PagingError> {
        self.check_clock(monotonic_ms)?;
        let mut expired = Vec::with_capacity(self.expired_count(monotonic_ms));
        self.advance_into(monotonic_ms, &mut expired)?;
        Ok(expired)
    }
    pub fn advance_into(
        &mut self,
        monotonic_ms: u64,
        expired: &mut Vec<PagingOutcome>,
    ) -> Result<(), PagingError> {
        self.check_clock(monotonic_ms)?;
        if expired.capacity() - expired.len() < self.expired_count(monotonic_ms) {
            return Err(PagingError::OutputCapacity);
        }
        let mut index = 0;
        while index < self.pending.len() {
            if monotonic_ms >= self.pending[index].request.deadline_monotonic_ms {
                expired.push(self.expire(index, monotonic_ms)?);
            } else {
                index += 1;
            }
        }
        self.last_monotonic_ms = monotonic_ms;
        Ok(())
    }
    fn expired_count(&self, monotonic_ms: u64) -> usize {
        self.pending
            .iter()
            .filter(|page| monotonic_ms >= page.request.deadline_monotonic_ms)
            .count()
    }
    pub fn cancel(
        &mut self,
        handle: &CallHandle,
        monotonic_ms: u64,
    ) -> Result<CallReservation, PagingError> {
        self.check_clock(monotonic_ms)?;
        let index = self.index(handle)?;
        let reservation = self.calls.release(handle).map_err(PagingError::Call)?;
        self.pending.remove(index);
        self.last_monotonic_ms = monotonic_ms;
        Ok(reservation)
    }
    /// Call only after both handed-off radio bearers have stopped, or before either assignment was transmitted.
    pub fn release_stopped(&mut self, handle: &CallHandle) -> Result<CallReservation, PagingError> {
        if self.index(handle).is_ok() {
            return Err(PagingError::StillPaging);
        }
        self.calls.release(handle).map_err(PagingError::Call)
    }
    fn index(&self, handle: &CallHandle) -> Result<usize, PagingError> {
        self.pending
            .iter()
            .position(|page| page.request.handle.matches(handle))
            .ok_or(PagingError::UnknownAttempt)
    }
    fn check_clock(&self, monotonic_ms: u64) -> Result<(), PagingError> {
        if monotonic_ms < self.last_monotonic_ms {
            return Err(PagingError::ClockRegression);
        }
        Ok(())
    }
    fn expire(&mut self, index: usize, monotonic_ms: u64) -> Result<PagingOutcome, PagingError> {
        let handle = self.pending[index].request.handle.clone();
        let outcome = if self.is_audio(&handle) {
            PagingOutcome::AudioExpired(
                self.calls
                    .release_audio(&handle)
                    .map_err(PagingError::Call)?,
            )
        } else {
            PagingOutcome::Expired(self.calls.release(&handle).map_err(PagingError::Call)?)
        };
        self.pending.remove(index);
        self.last_monotonic_ms = monotonic_ms;
        Ok(outcome)
    }
}
fn map_call_error(error: CallError) -> PagingError {
    match error {
        CallError::Admission(error) => PagingError::Admission(error),
        error => PagingError::Call(error),
    }
}

fn decode_response(set: &MessageSet) -> Result<PageResponse, PagingError> {
    let mut response = None;
    for message in &set.messages {
        match page_response::decode_prefix(message) {
            Ok(decoded) => {
                if response.replace(decoded.message).is_some() {
                    return Err(PagingError::DuplicateResponse);
                }
            }
            Err(PageResponseError::MessageType) => {}
            Err(error) => return Err(PagingError::Wire(error)),
        }
    }
    let response = response.ok_or(PagingError::MissingResponse)?;
    if !matches!(
        response.service_code,
        ServiceCode::DigitalSpeech
            | ServiceCode::DigitalPreferred
            | ServiceCode::AnalogPreferred
            | ServiceCode::ServiceRejected
    ) {
        return Err(PagingError::Service);
    }
    let primary = set
        .messages
        .iter()
        .find(|bytes| bytes.first() == Some(&crate::l3::access_call::PAGE_RESPONSE_TYPE))
        .ok_or(PagingError::MissingResponse)?;
    let decoded = crate::l3::access_call::PageResponseRequest::decode(primary)
        .map_err(PagingError::Options)?;
    crate::l3::access_call::validate_companions(
        set,
        crate::l3::access_call::Kind::PageResponse,
        response.protocol_version,
    )
    .map_err(PagingError::Options)?;
    if response.service_code != ServiceCode::ServiceRejected {
        decoded
            .options
            .select_codec(&[
                crate::codec::speech::Codec::AcelpCc1,
                crate::codec::speech::Codec::Vselp,
            ])
            .map_err(PagingError::Admission)?;
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::registry::ProvisionedSubscriber,
        l3::{
            address::{AddressInfo, AddressValue},
            broadcast::ProtocolVersion,
            designation::AssignedSlots,
            origination::Origination,
            registration::{AccessMessage, RegistrationType},
        },
        phy::channel::Band,
    };
    fn registry() -> LocalRegistry {
        let mut registry = LocalRegistry::try_new(
            (1..=2)
                .map(|id| ProvisionedSubscriber {
                    identity: MobileIdentity::Min34(id),
                    number: format!("100{id}"),
                })
                .collect(),
        )
        .unwrap();
        let message = AccessMessage::Registration {
            registration_type: RegistrationType::PowerUp,
            scm: 21,
            protocol_version: ProtocolVersion::Is136RevA,
        }
        .encode()
        .unwrap();
        for id in 1..=2 {
            registry
                .register(
                    &MessageSet {
                        identity: MobileIdentity::Min34(id),
                        messages: vec![message.clone()],
                    },
                    0,
                )
                .unwrap();
        }
        registry
    }
    fn paging() -> CallPaging {
        CallPaging::new(
            TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
        )
    }
    fn origination() -> MessageSet {
        MessageSet {
            identity: MobileIdentity::Min34(1),
            messages: vec![
                Origination {
                    protocol_version: ProtocolVersion::Is136RevA,
                    emergency_call: false,
                    called_party: AddressInfo {
                        number_type: 0,
                        numbering_plan: 1,
                        address: AddressValue::Tbcd("1002".into()),
                    },
                    last_try: false,
                    scm: 21,
                    service_code: ServiceCode::DigitalSpeech,
                }
                .encode()
                .unwrap(),
            ],
        }
    }
    fn response(service_code: ServiceCode) -> MessageSet {
        MessageSet {
            identity: MobileIdentity::Min34(2),
            messages: vec![
                PageResponse {
                    protocol_version: ProtocolVersion::Is136RevA,
                    last_try: false,
                    scm: 21,
                    service_code,
                }
                .encode()
                .unwrap(),
            ],
        }
    }
    #[test]
    fn bounded_expiry_preserves_state_on_full_output_and_appends_without_growth() {
        let registry = registry();
        let mut paging = paging();
        let cancelled = paging.begin(&registry, &origination(), 0, 500).unwrap();
        let reservation = paging.cancel(&cancelled.handle, 0).unwrap();
        let request = paging.begin(&registry, &origination(), 100, 500).unwrap();
        let mut expired = Vec::with_capacity(1);
        expired.push(PagingOutcome::Rejected(reservation));
        assert!(matches!(
            paging.advance_into(600, &mut expired),
            Err(PagingError::OutputCapacity)
        ));
        assert_eq!(expired.len(), 1);
        assert_eq!(paging.requests().count(), 1);
        assert_eq!(paging.reservations().count(), 1);
        paging.transmitted(&request.handle, 110).unwrap();
        expired.reserve(1);
        let capacity = expired.capacity();
        paging.advance_into(600, &mut expired).unwrap();
        assert_eq!(expired.capacity(), capacity);
        assert_eq!(expired.len(), 2);
        let PagingOutcome::Expired(reservation) = &expired[1] else {
            panic!("expected expiry");
        };
        assert!(reservation.handle.matches(&request.handle));
        assert_eq!(paging.requests().count(), 0);
        assert_eq!(paging.reservations().count(), 0);
        paging.advance_into(600, &mut expired).unwrap();
        assert_eq!(expired.len(), 2);
        assert!(matches!(
            paging.advance_into(599, &mut expired),
            Err(PagingError::ClockRegression)
        ));
        paging.begin(&registry, &origination(), 601, 500).unwrap();
    }
    #[test]
    fn valid_response_hands_off_both_reservations_and_only_stopped_release_restores_capacity() {
        let mut paging = paging();
        let registry = registry();
        let request = paging.begin(&registry, &origination(), 100, 500).unwrap();
        assert_eq!(request.recipient, MobileIdentity::Min34(2));
        assert_eq!(request.deadline_monotonic_ms, 600);
        assert!(matches!(
            paging.release_stopped(&request.handle),
            Err(PagingError::StillPaging)
        ));
        assert!(matches!(
            paging.response(&request.handle, &response(ServiceCode::DigitalSpeech), 101),
            Err(PagingError::NotTransmitted)
        ));
        paging.transmitted(&request.handle, 110).unwrap();
        let PagingOutcome::Ready {
            reservation,
            response,
        } = paging
            .response(
                &request.handle,
                &response(ServiceCode::DigitalPreferred),
                120,
            )
            .unwrap()
        else {
            panic!("digital response must be ready")
        };
        assert_eq!(response.service_code, ServiceCode::DigitalPreferred);
        assert_eq!(reservation.caller.assigned_slots, AssignedSlots::Full25);
        assert_eq!(reservation.callee.assigned_slots, AssignedSlots::Full36);
        assert_eq!(paging.requests().count(), 0);
        assert_eq!(paging.reservations().count(), 1);
        assert!(paging.advance(1000).unwrap().is_empty());
        assert!(matches!(
            paging.begin(&registry, &origination(), 1000, 500),
            Err(PagingError::Call(CallError::Busy(_)))
        ));
        paging.release_stopped(&reservation.handle).unwrap();
        let next = paging.begin(&registry, &origination(), 1000, 500).unwrap();
        assert!(matches!(
            paging.cancel(&request.handle, 1000),
            Err(PagingError::UnknownAttempt)
        ));
        paging.cancel(&next.handle, 1000).unwrap();
        assert_eq!(paging.reservations().count(), 0);
    }
    #[test]
    fn queued_and_retransmitted_pages_expire_at_the_same_absolute_deadline() {
        let registry = registry();
        for transmitted in [false, true] {
            let mut paging = paging();
            let request = paging.begin(&registry, &origination(), 100, 500).unwrap();
            if transmitted {
                paging.transmitted(&request.handle, 110).unwrap();
                paging.transmitted(&request.handle, 599).unwrap();
            }
            assert!(paging.advance(599).unwrap().is_empty());
            let outcomes = paging.advance(600).unwrap();
            assert!(
                matches!(&outcomes[..], [PagingOutcome::Expired(call)] if call.handle.id() == request.handle.id())
            );
            assert_eq!(paging.requests().count(), 0);
            assert_eq!(paging.reservations().count(), 0);
            assert!(paging.begin(&registry, &origination(), 600, 500).is_ok());
        }
    }
    #[test]
    fn deadline_response_and_late_transmission_release_unassigned_bearers() {
        let registry = registry();
        for late_response in [false, true] {
            let mut paging = paging();
            let request = paging.begin(&registry, &origination(), 100, 500).unwrap();
            let outcome = if late_response {
                paging.transmitted(&request.handle, 110).unwrap();
                paging
                    .response(&request.handle, &response(ServiceCode::DigitalSpeech), 600)
                    .unwrap()
            } else {
                paging.transmitted(&request.handle, 600).unwrap().unwrap()
            };
            assert!(matches!(outcome, PagingOutcome::Expired(_)));
            assert_eq!(paging.reservations().count(), 0);
            assert!(paging.begin(&registry, &origination(), 600, 500).is_ok());
        }
    }
    #[test]
    fn wrong_identity_malformed_duplicates_and_unsupported_services_preserve_the_wait() {
        let mut paging = paging();
        let registry = registry();
        let request = paging.begin(&registry, &origination(), 100, 500).unwrap();
        paging.transmitted(&request.handle, 110).unwrap();
        let digital = response(ServiceCode::DigitalSpeech);
        let mut wrong = digital.clone();
        wrong.identity = MobileIdentity::Min34(1);
        let mut duplicate = digital.clone();
        duplicate.messages.push(duplicate.messages[0].clone());
        let mut truncated = digital.clone();
        truncated.messages[0].truncate(2);
        for (set, expected) in [
            (wrong, PagingError::Recipient),
            (duplicate, PagingError::DuplicateResponse),
            (response(ServiceCode::AnalogSpeech), PagingError::Service),
        ] {
            assert!(
                matches!(paging.response(&request.handle, &set, 120), Err(error) if error == expected)
            );
        }
        assert!(matches!(
            paging.response(&request.handle, &truncated, 120),
            Err(PagingError::Wire(PageResponseError::Truncated { .. }))
        ));
        assert_eq!(paging.requests().count(), 1);
        assert_eq!(paging.reservations().count(), 1);
        assert!(matches!(
            paging.response(
                &request.handle,
                &response(ServiceCode::ServiceRejected),
                120
            ),
            Ok(PagingOutcome::Rejected(_))
        ));
        assert_eq!(paging.reservations().count(), 0);
        assert!(paging.begin(&registry, &origination(), 120, 500).is_ok());
    }
    #[test]
    fn clock_overflow_regression_and_foreign_handles_cannot_mutate_live_attempts() {
        let registry = registry();
        let mut paging = paging();
        assert!(matches!(
            paging.begin(&registry, &origination(), 100, 0),
            Err(PagingError::TimeoutDuration)
        ));
        assert!(matches!(
            paging.begin(&registry, &origination(), u64::MAX, 1),
            Err(PagingError::TimeOverflow)
        ));
        let request = paging.begin(&registry, &origination(), 100, 500).unwrap();
        let mut foreign = CallPaging::new(
            TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
        );
        let foreign = foreign.begin(&registry, &origination(), 100, 500).unwrap();
        assert_eq!(request.handle.id(), foreign.handle.id());
        assert!(matches!(
            paging.cancel(&foreign.handle, 100),
            Err(PagingError::UnknownAttempt)
        ));
        assert!(matches!(
            paging.advance(99),
            Err(PagingError::ClockRegression)
        ));
        assert!(matches!(
            paging.transmitted(&request.handle, 99),
            Err(PagingError::ClockRegression)
        ));
        assert_eq!(paging.requests().count(), 1);
        paging.cancel(&request.handle, 100).unwrap();
        assert_eq!(paging.reservations().count(), 0);
    }
    #[test]
    fn page_response_preferences_preserve_pending_resources_until_a_valid_ready_handoff() {
        use crate::l3::access_call::{
            AdmissionError, Bandwidth, CallOptions, LegacyVoice, PageResponseRequest,
        };
        let registry = registry();
        let mut paging = paging();
        let request = paging.begin(&registry, &origination(), 100, 500).unwrap();
        paging.transmitted(&request.handle, 110).unwrap();
        let mut response = PageResponseRequest {
            message: PageResponse {
                protocol_version: ProtocolVersion::Is136RevA,
                last_try: false,
                scm: 15,
                service_code: ServiceCode::AnalogPreferred,
            },
            options: CallOptions {
                voices: vec![LegacyVoice {
                    coder: 3,
                    privacy: 0,
                }],
                bandwidth: Some(Bandwidth::FullOnly),
                ..Default::default()
            },
        };
        let make_set = |response: &PageResponseRequest| MessageSet {
            identity: MobileIdentity::Min34(2),
            messages: vec![response.encode().unwrap()],
        };
        assert!(matches!(
            paging.response(&request.handle, &make_set(&response), 120),
            Err(PagingError::Admission(AdmissionError::Codec))
        ));
        assert_eq!(paging.reservations().count(), 1);
        assert_eq!(paging.requests().count(), 1);
        response.options.voices.push(LegacyVoice {
            coder: 1,
            privacy: 0,
        });
        let mut invalid = make_set(&response);
        invalid.messages.push(vec![13, 0]);
        assert!(paging.response(&request.handle, &invalid, 120).is_err());
        assert_eq!(paging.requests().count(), 1);
        assert!(matches!(
            paging.response(&request.handle, &make_set(&response), 120),
            Ok(PagingOutcome::Ready { .. })
        ));
        assert_eq!(paging.requests().count(), 0);
        assert_eq!(paging.reservations().count(), 1);
        paging.release_stopped(&request.handle).unwrap();
        assert_eq!(paging.reservations().count(), 0);
    }
}
