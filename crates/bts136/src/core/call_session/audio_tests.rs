use super::*;
use crate::core::calls::AudioDirection;
use crate::l3::legacy_traffic::CallingNumber;
use crate::{
    core::{
        calls::CallReservations,
        registry::{LocalRegistry, ProvisionedSubscriber},
    },
    l3::{
        broadcast::ProtocolVersion,
        designation::AssignedSlots,
        registration::{AccessMessage, RegistrationType},
    },
    link::{
        rach::{MessageSet, MobileIdentity},
        traffic_order::OrderPolicy,
    },
    phy::channel::Band,
    resources::TrafficResources,
};

fn session(direction: AudioDirection) -> TestSession {
    let identity = MobileIdentity::Min34(1);
    let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
        identity,
        number: "1001".into(),
    }])
    .unwrap();
    registry
        .register(
            &MessageSet {
                identity,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 11,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            0,
        )
        .unwrap();
    let mut calls = CallReservations::new(
        TrafficResources::try_new(Band::Cellular800, 991, &[AssignedSlots::Full14]).unwrap(),
    );
    let reservation = calls
        .reserve_audio(&registry, identity, "2001", direction, None)
        .unwrap();
    let order = OrderPolicy {
        response_timeout_ms: 100,
        overall_timeout_ms: 500,
        max_transmissions: 3,
    };
    let inner = CallSession::try_new_audio(
        reservation,
        SessionPolicy {
            establishment_timeout_ms: 500,
            answer_timeout_ms: 1000,
            release_timeout_ms: 500,
            alert: order,
            release: order,
            measurements: order,
            status: order,
            capabilities: order,
            physical_control: order,
        },
        0,
    )
    .unwrap();
    let party = match direction {
        AudioDirection::MobileOriginated => Party::Caller,
        AudioDirection::MobileTerminated => Party::Callee,
    };
    let handle = inner.assignment(party).unwrap().handle;
    TestSession { inner, handle }
}

fn ack(message: &TrafficMessage) -> TrafficMessage {
    TrafficMessage::Ack {
        ack_message_type: message.message_type(),
        information: Default::default(),
    }
}
fn alert(session: &mut TestSession) -> OrderTransmission {
    session.assignment_started(10).unwrap();
    session.bearer_confirmed(20).unwrap();
    session.order().unwrap().clone()
}

#[test]
fn originated_audio_enters_conversation_only_after_bearer_confirmation() {
    let mut session = session(AudioDirection::MobileOriginated);
    assert!(!session.media_ready());
    assert!(session.bearer_confirmed(1).is_err());
    session.assignment_started(10).unwrap();
    assert!(!session.media_ready());
    session.bearer_confirmed(20).unwrap();
    assert_eq!(session.state(), CallState::Connected);
    assert!(session.order().is_none());
    assert!(session.media_ready());
    session.advance(2000).unwrap();
    assert!(session.media_ready());
}

#[test]
fn terminated_audio_rings_with_caller_id_and_waits_for_transmitted_connect_ack() {
    let mut session = session(AudioDirection::MobileTerminated);
    let alert = alert(&mut session);
    let TrafficMessage::AlertWithInfo { information } = alert.message() else {
        panic!("alert required")
    };
    assert_eq!(
        information.calling_numbers,
        vec![CallingNumber::network("2001").unwrap()]
    );
    assert!(session.receive(&TrafficMessage::Connect, 21).is_err());
    assert!(session.receive(&ack(alert.message()), 22).is_err());
    session.order_transmitted(&alert, 30).unwrap();
    session.receive(&ack(alert.message()), 40).unwrap();
    assert_eq!(session.state(), CallState::WaitingAnswer);
    session.receive(&TrafficMessage::Connect, 50).unwrap();
    assert_eq!(session.state(), CallState::Connecting);
    assert!(!session.media_ready());
    let response = session.response().unwrap().clone();
    session.receive(&TrafficMessage::Connect, 51).unwrap();
    assert_eq!(session.response_count(), 1);
    session.response_transmitted(&response, 60).unwrap();
    assert!(session.media_ready());
    assert!(session.response_transmitted(&response, 61).is_err());
}

#[test]
fn answer_deadline_is_not_extended_by_alert_retransmission() {
    let mut session = session(AudioDirection::MobileTerminated);
    let first = alert(&mut session);
    session.order_transmitted(&first, 30).unwrap();
    session.advance(130).unwrap();
    let retry = session.order().unwrap().clone();
    session.order_transmitted(&retry, 140).unwrap();
    session.receive(&ack(retry.message()), 150).unwrap();
    session.advance(1029).unwrap();
    assert_eq!(session.state(), CallState::WaitingAnswer);
    session.advance(1030).unwrap();
    assert_eq!(
        session.state(),
        CallState::Releasing(EndReason::AnswerTimeout)
    );
    assert!(!session.media_ready());
}

#[test]
fn release_ack_requests_stop_but_does_not_release_the_radio_bearer() {
    let mut session = session(AudioDirection::MobileOriginated);
    session.assignment_started(10).unwrap();
    session.bearer_confirmed(20).unwrap();
    session.end(EndReason::Cancelled, 30).unwrap();
    let release = session.order().unwrap().clone();
    assert!(session.bearer_stopped(31).is_err());
    session.order_transmitted(&release, 40).unwrap();
    session.receive(&ack(release.message()), 50).unwrap();
    assert_eq!(session.bearer_state(), BearerState::Stopping);
    assert_eq!(session.state(), CallState::Releasing(EndReason::Cancelled));
    session.bearer_stopped(60).unwrap();
    assert_eq!(session.state(), CallState::Stopped(EndReason::Cancelled));
}

#[test]
fn mobile_release_ack_completion_precedes_stop_and_stale_tokens_cannot_stop_new_calls() {
    let mut first = session(AudioDirection::MobileOriginated);
    first.assignment_started(10).unwrap();
    first.bearer_confirmed(20).unwrap();
    let release = TrafficMessage::Release {
        information: Default::default(),
    };
    first.receive(&release, 30).unwrap();
    let response = first.response().unwrap().clone();
    first.receive(&release, 31).unwrap();
    assert_eq!(first.response_count(), 1);
    assert_eq!(first.bearer_state(), BearerState::Established);
    let mut second = session(AudioDirection::MobileOriginated);
    second.assignment_started(10).unwrap();
    second.bearer_confirmed(20).unwrap();
    second.receive(&release, 30).unwrap();
    assert!(second.response_transmitted(&response, 40).is_err());
    assert_eq!(second.bearer_state(), BearerState::Established);
    first.response_transmitted(&response, 40).unwrap();
    assert_eq!(first.bearer_state(), BearerState::Stopping);
}

#[test]
fn setup_expiry_and_radio_failure_require_actual_stop_confirmation() {
    let mut expired = session(AudioDirection::MobileOriginated);
    expired.assignment_started(10).unwrap();
    assert!(expired.bearer_confirmed(500).is_err());
    assert_eq!(
        expired.state(),
        CallState::Releasing(EndReason::SetupTimeout)
    );
    assert_eq!(expired.bearer_state(), BearerState::Stopping);
    expired.bearer_stopped(501).unwrap();
    assert_eq!(expired.state(), CallState::Stopped(EndReason::SetupTimeout));
    let mut failed = session(AudioDirection::MobileTerminated);
    alert(&mut failed);
    failed
        .end(EndReason::RadioFailure(Party::Callee), 30)
        .unwrap();
    assert_eq!(failed.bearer_state(), BearerState::Stopping);
    assert!(failed.order().is_none());
    failed.bearer_stopped(31).unwrap();
}

#[test]
fn duplicate_release_does_not_extend_stop_deadline_and_radio_loss_stops_immediately() {
    let mut call = session(AudioDirection::MobileOriginated);
    call.assignment_started(10).unwrap();
    call.bearer_confirmed(20).unwrap();
    let release = TrafficMessage::Release {
        information: Default::default(),
    };
    call.receive(&release, 30).unwrap();
    call.receive(&release, 400).unwrap();
    call.advance(530).unwrap();
    assert_eq!(call.bearer_state(), BearerState::Stopping);
    let mut failed = session(AudioDirection::MobileOriginated);
    failed.assignment_started(10).unwrap();
    failed.bearer_confirmed(20).unwrap();
    failed.end(EndReason::Cancelled, 30).unwrap();
    failed
        .end(EndReason::RadioFailure(Party::Caller), 40)
        .unwrap();
    assert_eq!(failed.bearer_state(), BearerState::Stopping);
    assert!(failed.order().is_none());
}

struct TestSession {
    inner: CallSession,
    handle: BearerHandle,
}
impl std::ops::Deref for TestSession {
    type Target = CallSession;
    fn deref(&self) -> &CallSession {
        &self.inner
    }
}
impl std::ops::DerefMut for TestSession {
    fn deref_mut(&mut self) -> &mut CallSession {
        &mut self.inner
    }
}
impl TestSession {
    fn assignment_started(&mut self, now: u64) -> Result<CallState, SessionError> {
        self.inner.assignment_started(&self.handle, now)
    }
    fn bearer_confirmed(&mut self, now: u64) -> Result<CallState, SessionError> {
        self.inner.bearer_confirmed(&self.handle, now)
    }
    fn order(&self) -> Option<&OrderTransmission> {
        self.inner.order(self.handle.party())
    }
    fn order_transmitted(
        &mut self,
        order: &OrderTransmission,
        now: u64,
    ) -> Result<CallState, SessionError> {
        self.inner.order_transmitted(&self.handle, order, now)
    }
    fn receive(&mut self, message: &TrafficMessage, now: u64) -> Result<CallState, SessionError> {
        self.inner.receive(&self.handle, message, now)
    }
    fn bearer_state(&self) -> BearerState {
        self.inner.bearer_state(self.handle.party())
    }
    fn response(&self) -> Option<&ResponseTransmission> {
        self.inner.response(self.handle.party())
    }
    fn response_count(&self) -> usize {
        self.inner.legs[self.handle.party().index()].responses.len()
    }
    fn bearer_stopped(&mut self, now: u64) -> Result<CallState, SessionError> {
        let stop = self
            .inner
            .stop_request(self.handle.party())
            .cloned()
            .ok_or(SessionError::State)?;
        self.inner.bearer_stopped(&stop, now)
    }
}

#[test]
fn single_phone_sessions_never_expose_or_accept_a_second_mobile_bearer() {
    for direction in [
        AudioDirection::MobileOriginated,
        AudioDirection::MobileTerminated,
    ] {
        let mut call = session(direction);
        let absent = if direction == AudioDirection::MobileOriginated {
            Party::Callee
        } else {
            Party::Caller
        };
        assert!(call.inner.assignment(absent).is_none());
        assert_eq!(call.inner.bearer_state(absent), BearerState::Stopped);
        assert!(call.inner.order(absent).is_none());
        assert!(call.inner.response(absent).is_none());
        assert!(call.inner.protocol_version(absent).is_none());
        assert!(call.inner.active_measurement_handle(absent).is_none());
        assert!(call.inner.extended_measurements(absent).is_none());
        assert!(call.inner.service_state(absent).is_none());
        assert!(call.inner.flash_state(absent).is_none());
        call.assignment_started(10).unwrap();
        call.bearer_confirmed(20).unwrap();
        assert!(call.inner.request_status(absent, 30).is_err());
        assert!(call.inner.request_capabilities(absent, 30).is_err());
        assert!(call.inner.set_measurements(absent, true, 30).is_err());
        assert!(
            call.inner
                .request_physical_control(absent, Default::default(), 30)
                .is_err()
        );
        let foreign = session(if absent == Party::Caller {
            AudioDirection::MobileOriginated
        } else {
            AudioDirection::MobileTerminated
        });
        assert_eq!(
            call.inner
                .bind_protocol(&foreign.handle, ProtocolVersion::Is136RevA),
            Err(SessionError::StaleBearer)
        );
    }
}

#[test]
fn external_originated_audio_waits_for_remote_answer_and_times_out() {
    let mut call = session(AudioDirection::MobileOriginated);
    call.inner.wait_for_remote_answer();
    call.assignment_started(10).unwrap();
    call.bearer_confirmed(20).unwrap();
    assert_eq!(call.state(), CallState::WaitingAnswer);
    assert!(!call.media_ready());
    call.inner.remote_answered(30).unwrap();
    assert_eq!(call.state(), CallState::Connected);
    assert!(call.media_ready());
    let mut unanswered = session(AudioDirection::MobileOriginated);
    unanswered.inner.wait_for_remote_answer();
    unanswered.assignment_started(10).unwrap();
    unanswered.bearer_confirmed(20).unwrap();
    unanswered.advance(1020).unwrap();
    assert!(matches!(
        unanswered.state(),
        CallState::Releasing(EndReason::AnswerTimeout)
    ));
}

#[test]
fn remote_answer_before_assignment_waits_for_mobile_bearer() {
    let mut call = session(AudioDirection::MobileOriginated);
    call.inner.wait_for_remote_answer();
    call.inner.remote_answered(5).unwrap();
    assert!(!call.media_ready());
    call.assignment_started(10).unwrap();
    call.bearer_confirmed(20).unwrap();
    assert_eq!(call.state(), CallState::Connected);
    assert!(call.media_ready());
}
