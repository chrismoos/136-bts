use super::*;
mod measurements;
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
    link::rach::{MessageSet, MobileIdentity},
    phy::channel::Band,
    resources::TrafficResources,
};
fn policy() -> SessionPolicy {
    let order = OrderPolicy {
        response_timeout_ms: 100,
        overall_timeout_ms: 500,
        max_transmissions: 3,
    };
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
    }
}
fn reservation() -> CallReservation {
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
    let mut calls = CallReservations::new(
        TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
    );
    calls
        .reserve(&registry, MobileIdentity::Min34(1), "1002")
        .unwrap()
}
fn session() -> CallSession {
    CallSession::try_new(reservation(), policy(), 0).unwrap()
}
fn establish(session: &mut CallSession) -> [BearerHandle; 2] {
    let handles = [
        session.assignment(Party::Caller).unwrap().handle,
        session.assignment(Party::Callee).unwrap().handle,
    ];
    for handle in &handles {
        session.assignment_started(handle, 10).unwrap();
    }
    session.bearer_confirmed(&handles[0], 20).unwrap();
    assert!(session.order(Party::Callee).is_none());
    session.bearer_confirmed(&handles[1], 30).unwrap();
    assert_eq!(session.state(), CallState::Alerting);
    handles
}
fn mobile_ack(kind: OrderKind) -> TrafficMessage {
    let message = match kind {
        OrderKind::ExtendedMeasurements => {
            panic!("extended measurements require their own target context")
        }
        OrderKind::PhysicalControl(control) => {
            return TrafficMessage::PhysicalControlAck(
                crate::l3::legacy_traffic::PhysicalControlFields {
                    control,
                    ..Default::default()
                },
            );
        }
        OrderKind::QueryStatus => return status_report(),
        OrderKind::QueryCapabilities(_) => return capability_report(),
        OrderKind::MeasureServingChannel => {
            return TrafficMessage::MeasurementOrderAck {
                rf_channels: Vec::new(),
            };
        }
        OrderKind::StopMeasurements => TrafficMessage::StopMeasurements,
        OrderKind::Alert => TrafficMessage::AlertWithInfo {
            information: Default::default(),
        },
        OrderKind::Release => TrafficMessage::Release {
            information: Default::default(),
        },
    };
    TrafficMessage::Ack {
        ack_message_type: message.message_type(),
        information: Default::default(),
    }
}
fn alert(session: &mut CallSession, handles: &[BearerHandle; 2]) {
    let order = session.order(Party::Callee).unwrap().clone();
    session.order_transmitted(&handles[1], &order, 40).unwrap();
}
fn connect(session: &mut CallSession, handles: &[BearerHandle; 2]) {
    alert(session, handles);
    session
        .receive(&handles[1], &mobile_ack(OrderKind::Alert), 50)
        .unwrap();
    session
        .receive(&handles[1], &TrafficMessage::Connect, 60)
        .unwrap();
    let ack = session.response(Party::Callee).unwrap().clone();
    session.response_transmitted(&ack, 70).unwrap();
    assert!(session.media_ready());
}
#[test]
fn unsolicited_measurement_and_noise_messages_do_not_complete_orders_or_change_media() {
    use crate::l3::legacy_traffic::{
        ExtendedChannelQualityReport, ExtendedCurrentQuality, ExtendedMeasurementTargets,
        ExtendedMeasurementType,
    };
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    session.advance(71).unwrap();
    let order = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &order, 75).unwrap();
    let measurement_state = session.measurement_state(Party::Caller);
    let report = TrafficMessage::ExtendedChannelQuality(ExtendedChannelQualityReport::First {
        current: ExtendedCurrentQuality {
            ber_code: 0,
            measurement_code: 0,
            rssi_code: 0,
        },
        neighbor_rssi_codes: [0; 4],
    });
    let ack = TrafficMessage::ExtendedMeasurementAck(
        ExtendedMeasurementTargets::try_new(ExtendedMeasurementType::CarrierInterference, vec![])
            .unwrap(),
    );
    for (message, expected) in [
        (report, SessionError::Message),
        (
            TrafficMessage::AmrControl(crate::l3::legacy_traffic::AmrControlMessage::Thresholds(
                crate::l3::legacy_traffic::AmrThresholds {
                    lower: [4, 17, 50],
                    hysteresis: [3, 9, 12],
                },
            )),
            SessionError::Message,
        ),
        (
            TrafficMessage::AmrNoise(crate::l3::legacy_traffic::AmrNoiseMessage::Implicit {
                data: crate::l3::legacy_traffic::ActiveAmrMode::Mode2,
            }),
            SessionError::Message,
        ),
        (
            TrafficMessage::ComfortNoise(crate::l3::legacy_traffic::ComfortNoiseParameters {
                lsf_residual_vector: 1,
                energy_quantization_index: 2,
                resc_parameters: 3,
            }),
            SessionError::Message,
        ),
        (ack, SessionError::Order(OrderError::NotAck)),
    ] {
        assert_eq!(session.receive(&handles[0], &message, 80), Err(expected));
        assert_eq!(session.measurement_state(Party::Caller), measurement_state);
    }
    assert!(session.order(Party::Caller).is_none());
    assert_eq!(session.measurement_state(Party::Caller), measurement_state);
    assert_eq!(session.state(), CallState::Connected);
    assert!(session.media_ready());
    session
        .receive(
            &handles[0],
            &mobile_ack(OrderKind::MeasureServingChannel),
            90,
        )
        .unwrap();
    assert_eq!(
        session.measurement_state(Party::Caller),
        Some(OrderState::Acknowledged)
    );
}
#[test]
fn confirmed_bearers_alert_and_connect_ack_transmission_enable_media() {
    let mut session = session();
    let handles = establish(&mut session);
    assert_eq!(
        session.receive(&handles[1], &TrafficMessage::Connect, 30),
        Err(SessionError::State)
    );
    alert(&mut session, &handles);
    assert_eq!(
        session.receive(&handles[0], &TrafficMessage::Connect, 40),
        Err(SessionError::State)
    );
    session
        .receive(&handles[1], &mobile_ack(OrderKind::Alert), 50)
        .unwrap();
    assert_eq!(session.state(), CallState::WaitingAnswer);
    session
        .receive(&handles[1], &TrafficMessage::Connect, 60)
        .unwrap();
    assert_eq!(session.state(), CallState::Connecting);
    assert!(!session.media_ready());
    let ack = session.response(Party::Callee).unwrap().clone();
    assert_eq!(
        ack.message(),
        &TrafficMessage::Ack {
            ack_message_type: 0xc1,
            information: Default::default()
        }
    );
    session
        .receive(&handles[1], &TrafficMessage::Connect, 65)
        .unwrap();
    session.response_transmitted(&ack, 70).unwrap();
    assert!(session.media_ready());
    session
        .receive(&handles[1], &TrafficMessage::Connect, 80)
        .unwrap();
    let repeat = session.response(Party::Callee).unwrap().clone();
    assert_eq!(
        session.response_transmitted(&ack, 80),
        Err(SessionError::StaleResponse)
    );
    session.response_transmitted(&repeat, 90).unwrap();
    assert!(session.media_ready());
}
#[test]
fn answer_before_alert_ack_cancels_unsent_alert_retry_and_waits_for_base_ack() {
    let mut session = session();
    let handles = establish(&mut session);
    alert(&mut session, &handles);
    session.advance(140).unwrap();
    let retry = session.order(Party::Callee).unwrap().clone();
    session
        .receive(&handles[1], &TrafficMessage::Connect, 150)
        .unwrap();
    assert!(session.order(Party::Callee).is_none());
    assert_eq!(
        session.order_transmitted(&handles[1], &retry, 150),
        Err(SessionError::State)
    );
    let ack = session.response(Party::Callee).unwrap().clone();
    session.response_transmitted(&ack, 160).unwrap();
    assert_eq!(session.state(), CallState::Connected);
}
#[test]
fn release_from_either_party_retains_capacity_until_both_actual_stop_callbacks() {
    for origin in [Party::Caller, Party::Callee] {
        let mut session = session();
        let handles = establish(&mut session);
        connect(&mut session, &handles);
        let release = TrafficMessage::Release {
            information: Default::default(),
        };
        session
            .receive(&handles[origin.index()], &release, 100)
            .unwrap();
        assert_eq!(
            session.state(),
            CallState::Releasing(EndReason::MobileRelease(origin))
        );
        assert!(!session.media_ready());
        assert!(session.stopped_handle().is_none());
        let base_ack = session.response(origin).unwrap().clone();
        assert_eq!(
            base_ack.message(),
            &TrafficMessage::Ack {
                ack_message_type: 0xda,
                information: Default::default()
            }
        );
        session
            .receive(&handles[origin.index()], &release, 100)
            .unwrap();
        session.response_transmitted(&base_ack, 110).unwrap();
        let other = if origin == Party::Caller {
            Party::Callee
        } else {
            Party::Caller
        };
        let order = session.order(other).unwrap().clone();
        session
            .order_transmitted(&handles[other.index()], &order, 110)
            .unwrap();
        session
            .receive(
                &handles[other.index()],
                &mobile_ack(OrderKind::Release),
                120,
            )
            .unwrap();
        let stops = [
            session.stop_request(Party::Caller).unwrap().clone(),
            session.stop_request(Party::Callee).unwrap().clone(),
        ];
        session.bearer_stopped(&stops[0], 130).unwrap();
        assert!(session.stopped_handle().is_none());
        session.bearer_stopped(&stops[1], 140).unwrap();
        assert_eq!(
            session.state(),
            CallState::Stopped(EndReason::MobileRelease(origin))
        );
        assert!(session.stopped_handle().is_some());
        assert_eq!(
            session.bearer_stopped(&stops[0], 140),
            Err(SessionError::StaleStop)
        );
    }
}
#[test]
fn setup_timeout_stops_started_assignments_and_never_requires_stop_for_unsent_reservations() {
    let mut unused = session();
    assert_eq!(
        unused.advance(500).unwrap(),
        CallState::Stopped(EndReason::SetupTimeout)
    );
    assert!(unused.stopped_handle().is_some());
    let mut started = session();
    let caller = started.assignment(Party::Caller).unwrap().handle;
    started.assignment_started(&caller, 10).unwrap();
    assert_eq!(
        started.advance(500).unwrap(),
        CallState::Releasing(EndReason::SetupTimeout)
    );
    assert_eq!(started.bearer_state(Party::Callee), BearerState::Stopped);
    assert_eq!(
        started.bearer_confirmed(&caller, 500),
        Err(SessionError::State)
    );
    assert!(started.stopped_handle().is_none());
    let stop = started.stop_request(Party::Caller).unwrap().clone();
    started.bearer_stopped(&stop, 510).unwrap();
    assert!(started.stopped_handle().is_some());
}
#[test]
fn alert_and_answer_timeouts_force_stops_without_treating_radio_commands_as_stop_confirmation() {
    for connect_pending in [false, true] {
        let mut policy = policy();
        policy.alert.max_transmissions = 1;
        let mut session = CallSession::try_new(reservation(), policy, 0).unwrap();
        let handles = establish(&mut session);
        alert(&mut session, &handles);
        let (deadline, reason) = if connect_pending {
            session
                .receive(&handles[1], &mobile_ack(OrderKind::Alert), 50)
                .unwrap();
            session
                .receive(&handles[1], &TrafficMessage::Connect, 60)
                .unwrap();
            (1040, EndReason::AnswerTimeout)
        } else {
            (140, EndReason::AlertTimeout)
        };
        session.advance(deadline).unwrap();
        assert_eq!(session.state(), CallState::Releasing(reason));
        assert!(session.response(Party::Callee).is_none());
        session.advance(deadline + 500).unwrap();
        assert!(session.stopped_handle().is_none());
        for party in [Party::Caller, Party::Callee] {
            let stop = session.stop_request(party).unwrap().clone();
            session.bearer_stopped(&stop, deadline + 510).unwrap();
        }
        assert_eq!(session.state(), CallState::Stopped(reason));
    }
}
#[test]
fn foreign_bearer_stop_and_ack_handles_and_regressing_clocks_cannot_complete_a_call() {
    let mut session = session();
    let handles = establish(&mut session);
    let mut foreign = CallSession::try_new(reservation(), policy(), 0).unwrap();
    let other_handles = establish(&mut foreign);
    assert_eq!(
        session.receive(&other_handles[1], &TrafficMessage::Connect, 30),
        Err(SessionError::StaleBearer)
    );
    assert_eq!(session.advance(29), Err(SessionError::ClockRegression));
    connect(&mut session, &handles);
    connect(&mut foreign, &other_handles);
    foreign
        .receive(&other_handles[1], &TrafficMessage::Connect, 80)
        .unwrap();
    let ack = foreign.response(Party::Callee).unwrap().clone();
    assert_eq!(
        session.response_transmitted(&ack, 80),
        Err(SessionError::StaleResponse)
    );
    session.end(EndReason::Cancelled, 100).unwrap();
    foreign.end(EndReason::Cancelled, 100).unwrap();
    session.advance(600).unwrap();
    foreign.advance(600).unwrap();
    let stop = foreign.stop_request(Party::Caller).unwrap().clone();
    assert_eq!(
        session.bearer_stopped(&stop, 600),
        Err(SessionError::StaleStop)
    );
    assert!(session.stopped_handle().is_none());
}

#[test]
fn radio_failure_stops_the_lost_leg_and_releases_the_surviving_leg() {
    let mut session = session();
    let handles = establish(&mut session);
    alert(&mut session, &handles);
    session
        .receive(&handles[1], &TrafficMessage::Connect, 50)
        .unwrap();
    let ack = session.response(Party::Callee).unwrap().clone();
    session
        .end(EndReason::RadioFailure(Party::Caller), 60)
        .unwrap();
    assert_eq!(
        session.response_transmitted(&ack, 60),
        Err(SessionError::StaleResponse)
    );
    assert!(!session.media_ready());
    assert!(session.stopped_handle().is_none());
    assert!(session.order(Party::Caller).is_none());
    let lost = session.stop_request(Party::Caller).unwrap().clone();
    session.bearer_stopped(&lost, 70).unwrap();
    assert!(session.stop_request(Party::Callee).is_none());
    let release = session.order(Party::Callee).unwrap().clone();
    session
        .order_transmitted(&handles[1], &release, 70)
        .unwrap();
    session
        .receive(&handles[1], &mobile_ack(OrderKind::Release), 80)
        .unwrap();
    let surviving = session.stop_request(Party::Callee).unwrap().clone();
    session.bearer_stopped(&surviving, 90).unwrap();
    assert_eq!(
        session.state(),
        CallState::Stopped(EndReason::RadioFailure(Party::Caller))
    );
}
#[test]
fn simultaneous_mobile_releases_ack_both_without_extending_the_first_release_deadline() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    let release = TrafficMessage::Release {
        information: Default::default(),
    };
    session.receive(&handles[0], &release, 100).unwrap();
    session.receive(&handles[1], &release, 599).unwrap();
    assert!(session.order(Party::Callee).is_none());
    assert!(session.response(Party::Caller).is_some());
    assert!(session.response(Party::Callee).is_some());
    session.advance(600).unwrap();
    for party in [Party::Caller, Party::Callee] {
        assert!(session.response(party).is_none());
        assert!(session.stop_request(party).is_some());
    }
    assert!(session.stopped_handle().is_none());
}
#[test]
fn zero_policy_and_time_overflow_reject_session_construction() {
    let original = policy();
    for invalid in [
        SessionPolicy {
            establishment_timeout_ms: 0,
            ..original
        },
        SessionPolicy {
            answer_timeout_ms: 0,
            ..original
        },
        SessionPolicy {
            release_timeout_ms: 0,
            ..original
        },
        SessionPolicy {
            alert: OrderPolicy {
                max_transmissions: 0,
                ..original.alert
            },
            ..original
        },
        SessionPolicy {
            release: OrderPolicy {
                response_timeout_ms: 0,
                ..original.release
            },
            ..original
        },
        SessionPolicy {
            physical_control: OrderPolicy {
                response_timeout_ms: 0,
                ..original.physical_control
            },
            ..original
        },
        SessionPolicy {
            capabilities: OrderPolicy {
                max_transmissions: 0,
                ..original.capabilities
            },
            ..original
        },
        SessionPolicy {
            status: OrderPolicy {
                overall_timeout_ms: 0,
                ..original.status
            },
            ..original
        },
    ] {
        assert!(matches!(
            CallSession::try_new(reservation(), invalid, 0),
            Err(SessionError::Policy)
        ));
    }
    assert!(matches!(
        CallSession::try_new(reservation(), original, u64::MAX),
        Err(SessionError::TimeOverflow)
    ));
}

#[test]
fn radio_shutdown_closes_both_bearers_and_invalidates_queued_commands() {
    let mut session = CallSession::try_new(reservation(), policy(), 0).unwrap();
    let caller = session.assignment(Party::Caller).unwrap();
    let callee = session.assignment(Party::Callee).unwrap();
    session.assignment_started(&caller.handle, 10).unwrap();
    session.assignment_started(&callee.handle, 10).unwrap();
    session.bearer_confirmed(&caller.handle, 20).unwrap();
    session.bearer_confirmed(&callee.handle, 20).unwrap();
    let order = session.order(Party::Callee).unwrap().clone();
    session.radio_shutdown(30).unwrap();
    assert_eq!(session.state(), CallState::Stopped(EndReason::Cancelled));
    assert!(session.stopped_handle().is_some());
    for party in [Party::Caller, Party::Callee] {
        assert_eq!(session.bearer_state(party), BearerState::Stopped);
        assert!(session.order(party).is_none());
        assert!(session.stop_request(party).is_none());
    }
    assert!(
        session
            .order_transmitted(&callee.handle, &order, 31)
            .is_err()
    );
}

#[test]
fn measurement_orders_start_after_connect_and_timeouts_do_not_end_the_call() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    session.advance(71).unwrap();
    let caller = session.order(Party::Caller).unwrap().clone();
    let callee = session.order(Party::Callee).unwrap().clone();
    let ack = TrafficMessage::MeasurementOrderAck {
        rf_channels: vec![],
    };
    assert!(matches!(
        session.receive(&handles[0], &ack, 72),
        Err(SessionError::Order(OrderError::NotTransmitted))
    ));
    session.order_transmitted(&handles[0], &caller, 80).unwrap();
    session.order_transmitted(&handles[1], &callee, 80).unwrap();
    session.receive(&handles[0], &ack, 90).unwrap();
    assert_eq!(
        session.measurement_state(Party::Caller),
        Some(OrderState::Acknowledged)
    );
    session.advance(180).unwrap();
    assert!(session.order(Party::Caller).is_none());
    assert!(session.order(Party::Callee).is_some());
    session.advance(571).unwrap();
    assert_eq!(
        session.measurement_state(Party::Callee),
        Some(OrderState::TimedOut)
    );
    assert!(session.media_ready());
    session.end(EndReason::Cancelled, 572).unwrap();
    assert!(session.measurement_state(Party::Caller).is_none());
    assert!(matches!(
        session.order(Party::Caller).unwrap().message(),
        TrafficMessage::Release { .. }
    ));
    assert!(matches!(
        session.receive(&handles[0], &ack, 573),
        Err(SessionError::State)
    ));
}

#[test]
fn measurement_start_stop_is_idempotent_and_replacement_tokens_are_distinct() {
    let mut session = session();
    let handles = establish(&mut session);
    assert_eq!(
        session.set_measurements(Party::Caller, true, 31),
        Err(SessionError::State)
    );
    connect(&mut session, &handles);
    session.advance(71).unwrap();
    let first = session.order(Party::Caller).unwrap().clone();
    session.set_measurements(Party::Caller, true, 72).unwrap();
    assert!(first.matches(session.order(Party::Caller).unwrap()));
    session.set_measurements(Party::Caller, false, 73).unwrap();
    let stop = session.order(Party::Caller).unwrap().clone();
    assert!(matches!(stop.message(), TrafficMessage::StopMeasurements));
    session.set_measurements(Party::Caller, true, 74).unwrap();
    let restarted = session.order(Party::Caller).unwrap().clone();
    assert_eq!(first.message(), restarted.message());
    assert!(!first.matches(&restarted));
    assert_eq!(
        session.order_transmitted(&handles[0], &first, 75),
        Err(SessionError::Order(OrderError::StaleTransmission))
    );
    session.set_measurements(Party::Caller, false, 76).unwrap();
    let stop = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &stop, 77).unwrap();
    let ack = TrafficMessage::Ack {
        ack_message_type: TrafficMessage::StopMeasurements.message_type(),
        information: Default::default(),
    };
    session.receive(&handles[0], &ack, 78).unwrap();
    assert_eq!(
        session.measurement_state(Party::Caller),
        Some(OrderState::Acknowledged)
    );
    assert_eq!(
        session.measurement_kind(Party::Caller),
        Some(OrderKind::StopMeasurements)
    );
    assert!(session.media_ready());
    session.advance(2000).unwrap();
    assert!(session.order(Party::Caller).is_none());
}

fn status_report() -> TrafficMessage {
    use crate::l3::legacy_traffic::{StatusFields, TerminalInformation};
    TrafficMessage::Status(StatusFields {
        serial_numbers: Some(vec![0x12345678]),
        call_modes: Some(vec![3]),
        terminals: Some(vec![TerminalInformation {
            protocol_version: ProtocolVersion::Is136RevA,
            manufacturer: 1,
            model: 2,
            firmware: 3,
            access_overload_class: 0,
            local_control: false,
            registration: true,
        }]),
        ..Default::default()
    })
}
#[test]
fn status_acknowledgments_do_not_overwrite_connect_or_enable_media_for_another_leg() {
    let mut session = session();
    let handles = establish(&mut session);
    alert(&mut session, &handles);
    session
        .receive(&handles[1], &TrafficMessage::Connect, 60)
        .unwrap();
    let connect_ack = session.response(Party::Callee).unwrap().clone();
    session.receive(&handles[1], &status_report(), 61).unwrap();
    session.receive(&handles[0], &status_report(), 62).unwrap();
    assert!(connect_ack.matches(session.response(Party::Callee).unwrap()));
    let status_ack = session.response(Party::Caller).unwrap().clone();
    session.response_transmitted(&status_ack, 63).unwrap();
    assert_eq!(session.state(), CallState::Connecting);
    assert!(!session.media_ready());
    session.response_transmitted(&connect_ack, 64).unwrap();
    assert_eq!(session.state(), CallState::Connected);
    let remaining = session.response(Party::Callee).unwrap().clone();
    assert!(matches!(
        remaining.message(),
        TrafficMessage::Ack {
            ack_message_type: 0x97,
            ..
        }
    ));
    session.response_transmitted(&remaining, 65).unwrap();
    assert!(session.response(Party::Callee).is_none());
    assert!(session.media_ready());
}
#[test]
fn status_poll_waits_for_transmission_and_all_requested_fields_and_serializes_measurements() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    session.request_status(Party::Caller, 81).unwrap();
    let request = session.order(Party::Caller).unwrap().clone();
    assert!(matches!(
        request.message(),
        TrafficMessage::StatusRequest(_)
    ));
    session.request_status(Party::Caller, 82).unwrap();
    assert!(request.matches(session.order(Party::Caller).unwrap()));
    session.receive(&handles[0], &status_report(), 83).unwrap();
    assert_eq!(
        session.status_state(Party::Caller),
        Some(OrderState::Prepared)
    );
    session
        .order_transmitted(&handles[0], &request, 84)
        .unwrap();
    session.set_measurements(Party::Caller, false, 85).unwrap();
    assert!(session.order(Party::Caller).is_none());
    session
        .receive(
            &handles[0],
            &TrafficMessage::Status(crate::l3::legacy_traffic::StatusFields {
                serial_numbers: Some(vec![1]),
                ..Default::default()
            }),
            86,
        )
        .unwrap();
    assert!(matches!(
        session.status_state(Party::Caller),
        Some(OrderState::AwaitingAck { .. })
    ));
    session.receive(&handles[0], &status_report(), 87).unwrap();
    assert_eq!(
        session.status_state(Party::Caller),
        Some(OrderState::Acknowledged)
    );
    assert_eq!(
        session.order(Party::Caller).unwrap().message(),
        &TrafficMessage::StopMeasurements
    );
    session.request_status(Party::Caller, 88).unwrap();
    let replacement = session.order(Party::Caller).unwrap().clone();
    assert!(!replacement.matches(&request));
    assert!(
        session
            .order_transmitted(&handles[0], &request, 89)
            .is_err()
    );
    assert_eq!(
        session.status_state(Party::Caller),
        Some(OrderState::Prepared)
    );
    session.end(EndReason::Cancelled, 90).unwrap();
    assert_eq!(
        session.status_state(Party::Caller),
        Some(OrderState::Cancelled)
    );
    assert!(session.receive(&handles[0], &status_report(), 91).is_err());
}

#[test]
fn status_query_has_bounded_retries_and_late_reports_do_not_revive_it() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    session.request_status(Party::Caller, 81).unwrap();
    for sent_at in [82, 183, 284] {
        let request = session.order(Party::Caller).unwrap().clone();
        assert!(matches!(
            request.message(),
            TrafficMessage::StatusRequest(_)
        ));
        session
            .order_transmitted(&handles[0], &request, sent_at)
            .unwrap();
        session.advance(sent_at + 100).unwrap();
    }
    assert_eq!(
        session.status_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    assert!(session.media_ready());
    session.receive(&handles[0], &status_report(), 385).unwrap();
    assert_eq!(
        session.status_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    assert!(session.response(Party::Caller).is_some());
    session.end(EndReason::Cancelled, 386).unwrap();
    assert_eq!(
        session.status_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
}

fn capability_report() -> TrafficMessage {
    use crate::l3::legacy_traffic::{CapabilityResponse, MobileCapabilities};
    let common = crate::l3::legacy_traffic::CapabilityFields {
        protocol_version: ProtocolVersion::Is136RevA,
        voice_privacy_map: 0,
        voice_coder_map: 1,
        encryption_key_map: 0,
        facch_sacch_arq: false,
        soc: 0,
        bsmc: 0,
        sms_map: 3,
        ira: true,
        encryption_algorithms: [None; 8],
    };
    TrafficMessage::CapabilityUpdateResponse(CapabilityResponse::Mobile(MobileCapabilities {
        common,
        supported_bands: 1,
        analog_800_speech: false,
        pv1: None,
        advice: None,
        pv5: None,
        extended_characters: None,
    }))
}

fn station_capabilities() -> crate::l3::legacy_traffic::BaseStationCapabilities {
    let TrafficMessage::CapabilityUpdateResponse(
        crate::l3::legacy_traffic::CapabilityResponse::Mobile(mobile),
    ) = capability_report()
    else {
        unreachable!()
    };
    crate::l3::legacy_traffic::BaseStationCapabilities {
        common: mobile.common,
        pv1: None,
        extended_bands: None,
        extended_characters: None,
    }
}

#[test]
fn capability_responses_are_single_coalesced_replies_and_do_not_complete_connect() {
    let mut session = session();
    let handles = establish(&mut session);
    alert(&mut session, &handles);
    session
        .respond_capabilities(&handles[0], station_capabilities(), 41)
        .unwrap();
    let response = session.response(Party::Caller).unwrap().clone();
    session
        .respond_capabilities(&handles[0], station_capabilities(), 42)
        .unwrap();
    assert!(response.matches(session.response(Party::Caller).unwrap()));
    session
        .receive(&handles[1], &TrafficMessage::Connect, 60)
        .unwrap();
    let connect = session.response(Party::Callee).unwrap().clone();
    session.response_transmitted(&response, 61).unwrap();
    assert_eq!(session.state(), CallState::Connecting);
    assert!(!session.media_ready());
    assert!(session.response(Party::Caller).is_none());
    assert_eq!(
        session.response_transmitted(&response, 62),
        Err(SessionError::StaleResponse)
    );
    session.response_transmitted(&connect, 63).unwrap();
    assert!(session.media_ready());
}

#[test]
fn capability_reply_does_not_replace_a_pending_connect_ack_and_is_cancelled_on_release() {
    let mut session = session();
    let handles = establish(&mut session);
    alert(&mut session, &handles);
    session
        .receive(&handles[1], &TrafficMessage::Connect, 60)
        .unwrap();
    let connect = session.response(Party::Callee).unwrap().clone();
    session
        .respond_capabilities(&handles[1], station_capabilities(), 61)
        .unwrap();
    assert!(connect.matches(session.response(Party::Callee).unwrap()));
    session.response_transmitted(&connect, 62).unwrap();
    let response = session.response(Party::Callee).unwrap().clone();
    assert!(matches!(
        response.message(),
        TrafficMessage::CapabilityUpdateResponse(_)
    ));
    session.end(EndReason::Cancelled, 63).unwrap();
    assert!(session.response(Party::Callee).is_none());
    assert_eq!(
        session.response_transmitted(&response, 64),
        Err(SessionError::StaleResponse)
    );
    assert_eq!(
        session.respond_capabilities(&handles[0], station_capabilities(), 65),
        Err(SessionError::State)
    );
}

#[test]
fn capability_poll_requires_a_transmitted_request_and_serializes_other_orders() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    session.request_capabilities(Party::Caller, 81).unwrap();
    let query = session.order(Party::Caller).unwrap().clone();
    assert_eq!(
        query.message(),
        &TrafficMessage::CapabilityUpdateRequest(ProtocolVersion::Is136RevA)
    );
    session.request_capabilities(Party::Caller, 82).unwrap();
    assert!(query.matches(session.order(Party::Caller).unwrap()));
    session
        .receive(&handles[0], &capability_report(), 83)
        .unwrap();
    assert_eq!(
        session.capability_state(Party::Caller),
        Some(OrderState::Prepared)
    );
    session.order_transmitted(&handles[0], &query, 84).unwrap();
    session.request_status(Party::Caller, 85).unwrap();
    assert!(session.order(Party::Caller).is_none());
    session.advance(183).unwrap();
    assert!(session.order(Party::Caller).is_none());
    session.advance(184).unwrap();
    let retry = session.order(Party::Caller).unwrap();
    assert!(!query.matches(retry));
    assert_eq!(query.message(), retry.message());
    session
        .receive(&handles[0], &capability_report(), 185)
        .unwrap();
    assert_eq!(
        session.capability_state(Party::Caller),
        Some(OrderState::Acknowledged)
    );
    assert!(matches!(
        session.order(Party::Caller).unwrap().message(),
        TrafficMessage::StatusRequest(_)
    ));
    assert!(session.response(Party::Caller).is_none());
    assert!(session.media_ready());
}

#[test]
fn capability_poll_rejects_stale_completions_and_records_timeout_or_cancellation() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    session.request_capabilities(Party::Caller, 80).unwrap();
    let old = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &old, 81).unwrap();
    session
        .receive(&handles[0], &capability_report(), 82)
        .unwrap();
    session.request_capabilities(Party::Caller, 83).unwrap();
    assert_eq!(
        session.order_transmitted(&handles[0], &old, 84),
        Err(SessionError::Order(OrderError::StaleTransmission))
    );
    let query = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &query, 85).unwrap();
    session.advance(583).unwrap();
    assert_eq!(
        session.capability_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    session
        .receive(&handles[0], &capability_report(), 584)
        .unwrap();
    assert_eq!(
        session.capability_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    assert!(session.media_ready());
    session.request_capabilities(Party::Callee, 585).unwrap();
    session.end(EndReason::Cancelled, 586).unwrap();
    assert_eq!(
        session.capability_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    assert_eq!(
        session.capability_state(Party::Callee),
        Some(OrderState::Cancelled)
    );
}

#[test]
fn capability_query_uses_its_response_instead_of_a_generic_or_forward_acknowledgment() {
    let recipient = MobileIdentity::Min34(1);
    let kind = OrderKind::QueryCapabilities(ProtocolVersion::Is136RevA);
    let mut exchange =
        TrafficOrderExchange::try_new(recipient, kind, policy().capabilities, 0).unwrap();
    let transmission = exchange.transmission().unwrap().clone();
    exchange.transmitted(&transmission, 10).unwrap();
    let generic_ack = TrafficMessage::Ack {
        ack_message_type: transmission.message().message_type(),
        information: Default::default(),
    };
    for wrong in [
        generic_ack,
        TrafficMessage::CapabilityUpdateResponse(
            crate::l3::legacy_traffic::CapabilityResponse::BaseStation(station_capabilities()),
        ),
    ] {
        assert_eq!(
            exchange.acknowledge(recipient, &wrong, 11),
            Err(OrderError::NotAck)
        );
    }
    assert_eq!(
        exchange.acknowledge(MobileIdentity::Min34(2), &capability_report(), 12),
        Err(OrderError::Recipient)
    );
    assert_eq!(
        exchange.acknowledge(recipient, &capability_report(), 13),
        Ok(OrderState::Acknowledged)
    );
}

#[test]
fn capability_query_retries_stop_at_the_transmission_limit() {
    let recipient = MobileIdentity::Min34(1);
    let mut exchange = TrafficOrderExchange::try_new(
        recipient,
        OrderKind::QueryCapabilities(ProtocolVersion::Is136RevA),
        policy().capabilities,
        0,
    )
    .unwrap();
    for attempt in 0..3 {
        let transmission = exchange.transmission().unwrap().clone();
        let sent_ms = 10 + attempt * 100;
        exchange.transmitted(&transmission, sent_ms).unwrap();
        assert_eq!(exchange.transmissions(), attempt as u16 + 1);
        assert_eq!(
            exchange.advance(sent_ms + 99).unwrap(),
            OrderState::AwaitingAck {
                deadline_monotonic_ms: sent_ms + 100
            }
        );
        assert_eq!(
            exchange.advance(sent_ms + 100).unwrap(),
            if attempt == 2 {
                OrderState::TimedOut
            } else {
                OrderState::Prepared
            }
        );
    }
    assert!(exchange.transmission().is_none());
}

#[test]
fn capability_request_response_preserves_the_waiting_for_answer_task() {
    let mut session = session();
    let handles = establish(&mut session);
    alert(&mut session, &handles);
    session
        .receive(&handles[1], &mobile_ack(OrderKind::Alert), 50)
        .unwrap();
    assert_eq!(session.state(), CallState::WaitingAnswer);
    session
        .respond_capabilities(&handles[1], station_capabilities(), 51)
        .unwrap();
    let response = session.response(Party::Callee).unwrap().clone();
    session.response_transmitted(&response, 52).unwrap();
    assert_eq!(session.state(), CallState::WaitingAnswer);
    assert!(!session.media_ready());
}

fn physical_request() -> crate::l3::legacy_traffic::PhysicalControlRequest {
    crate::l3::legacy_traffic::PhysicalControlRequest {
        power_level: Some(5),
        time_alignment: Some(crate::l3::designation::TimeAlignment::OffsetUnits(4)),
        dtx_allowed: Some(false),
        delay_compensation: Some(false),
    }
}
fn physical_ack(control: crate::l3::legacy_traffic::PhysicalControlRequest) -> TrafficMessage {
    TrafficMessage::PhysicalControlAck(crate::l3::legacy_traffic::PhysicalControlFields {
        control,
        ..Default::default()
    })
}
#[test]
fn physical_control_waits_for_final_transmission_and_complete_resulting_state() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    let request = physical_request();
    session
        .request_physical_control(Party::Caller, request, 80)
        .unwrap();
    let transmission = session.order(Party::Caller).unwrap().clone();
    assert!(matches!(
        transmission.message(),
        TrafficMessage::PhysicalControl(_)
    ));
    assert_eq!(
        session.receive(&handles[0], &physical_ack(request), 81),
        Err(SessionError::Order(OrderError::NotTransmitted))
    );
    session
        .order_transmitted(&handles[0], &transmission, 82)
        .unwrap();
    let mut partial = request;
    partial.delay_compensation = None;
    assert_eq!(
        session.receive(&handles[0], &physical_ack(partial), 83),
        Err(SessionError::Order(OrderError::ControlFields))
    );
    partial = request;
    partial.time_alignment = Some(crate::l3::designation::TimeAlignment::OffsetUnits(5));
    assert_eq!(
        session.receive(&handles[0], &physical_ack(partial), 84),
        Err(SessionError::Order(OrderError::ControlFields))
    );
    assert_eq!(
        session.physical_control_state(Party::Caller),
        Some(OrderState::AwaitingAck {
            deadline_monotonic_ms: 182
        })
    );
    let mut actual = request;
    actual.power_level = Some(7);
    session
        .receive(&handles[0], &physical_ack(actual), 85)
        .unwrap();
    assert_eq!(
        session.physical_control_state(Party::Caller),
        Some(OrderState::Acknowledged)
    );
    assert!(session.media_ready());
}
#[test]
fn physical_control_cannot_replace_a_pending_exchange_and_new_tokens_reject_old_completions() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    let request = physical_request();
    session
        .request_physical_control(Party::Caller, request, 80)
        .unwrap();
    let transmission = session.order(Party::Caller).unwrap().clone();
    session
        .request_physical_control(Party::Caller, request, 81)
        .unwrap();
    assert!(transmission.matches(session.order(Party::Caller).unwrap()));
    let mut other = request;
    other.power_level = Some(8);
    assert_eq!(
        session.request_physical_control(Party::Caller, other, 82),
        Err(SessionError::State)
    );
    session
        .order_transmitted(&handles[0], &transmission, 83)
        .unwrap();
    session.request_status(Party::Caller, 84).unwrap();
    assert!(session.order(Party::Caller).is_none());
    session
        .receive(&handles[0], &physical_ack(request), 85)
        .unwrap();
    session
        .request_physical_control(Party::Caller, other, 86)
        .unwrap();
    let current = session.order(Party::Caller).unwrap().clone();
    assert!(!transmission.matches(&current));
    assert_eq!(
        session.order_transmitted(&handles[0], &transmission, 87),
        Err(SessionError::Order(OrderError::StaleTransmission))
    );
    session.end(EndReason::Cancelled, 88).unwrap();
    assert_eq!(
        session.physical_control_state(Party::Caller),
        Some(OrderState::Cancelled)
    );
    assert_eq!(session.physical_control_request(Party::Caller), Some(other));
}
#[test]
fn physical_control_has_bounded_retries_and_timeout_keeps_media_running() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    session
        .request_physical_control(Party::Caller, physical_request(), 80)
        .unwrap();
    for attempt in 0..3 {
        let transmission = session.order(Party::Caller).unwrap().clone();
        let sent = 81 + attempt * 100;
        session
            .order_transmitted(&handles[0], &transmission, sent)
            .unwrap();
        session.advance(sent + 100).unwrap();
    }
    assert_eq!(
        session.physical_control_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    assert!(session.media_ready());
    session.radio_shutdown(382).unwrap();
    assert_eq!(
        session.physical_control_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
}
#[test]
fn invalid_or_empty_physical_controls_are_not_queued() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    assert_eq!(
        session.request_physical_control(Party::Caller, Default::default(), 80),
        Err(SessionError::Message)
    );
    let mut request = physical_request();
    request.power_level = Some(11);
    assert!(matches!(
        session.request_physical_control(Party::Caller, request, 81),
        Err(SessionError::Control(_))
    ));
    assert!(session.physical_control_state(Party::Caller).is_none());
}

#[test]
fn initial_alignment_sends_before_confirmation_and_does_not_enable_alert_or_media() {
    let mut session = session();
    let handles = [
        session.assignment(Party::Caller).unwrap().handle,
        session.assignment(Party::Callee).unwrap().handle,
    ];
    assert_eq!(
        session.align_initial_bearer(&handles[0], 4, 1),
        Err(SessionError::State)
    );
    for handle in &handles {
        session.assignment_started(handle, 10).unwrap();
    }
    session.align_initial_bearer(&handles[0], 4, 20).unwrap();
    let order = session.order(Party::Caller).unwrap().clone();
    let TrafficMessage::PhysicalControl(fields) = order.message() else {
        panic!("timing order expected");
    };
    let response = TrafficMessage::PhysicalControlAck(fields.clone());
    session.order_transmitted(&handles[0], &order, 21).unwrap();
    session.receive(&handles[0], &response, 22).unwrap();
    assert_eq!(session.state(), CallState::Establishing);
    assert_eq!(session.bearer_state(Party::Caller), BearerState::Assigning);
    assert!(!session.media_ready());
    session.bearer_confirmed(&handles[0], 23).unwrap();
    session.align_initial_bearer(&handles[1], 6, 24).unwrap();
    let order = session.order(Party::Callee).unwrap().clone();
    let TrafficMessage::PhysicalControl(fields) = order.message() else {
        panic!("timing order expected");
    };
    let response = TrafficMessage::PhysicalControlAck(fields.clone());
    session.order_transmitted(&handles[1], &order, 25).unwrap();
    session.bearer_confirmed(&handles[1], 26).unwrap();
    assert_eq!(session.state(), CallState::Alerting);
    assert!(session.order(Party::Callee).is_none());
    session.receive(&handles[1], &response, 27).unwrap();
    assert!(matches!(
        session.order(Party::Callee).unwrap().message(),
        TrafficMessage::AlertWithInfo { .. }
    ));
}
#[test]
fn unconfirmed_initial_alignment_expires_without_releasing_live_bearer_resources() {
    let mut session = session();
    let handle = session.assignment(Party::Caller).unwrap().handle;
    session.assignment_started(&handle, 10).unwrap();
    session.align_initial_bearer(&handle, 30, 20).unwrap();
    let transmission = session.order(Party::Caller).unwrap().clone();
    session
        .order_transmitted(&handle, &transmission, 21)
        .unwrap();
    session.advance(500).unwrap();
    assert_eq!(
        session.physical_control_state(Party::Caller),
        Some(OrderState::Cancelled)
    );
    assert!(session.order(Party::Caller).is_none());
    assert!(session.stop_request(Party::Caller).is_some());
    assert!(session.stopped_handle().is_none());
}

#[test]
fn timing_range_failure_releases_both_legs_and_retains_resources_until_actual_stop() {
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    let reason = EndReason::TimingOutOfRange(Party::Callee);
    assert_eq!(
        session.end(reason, 80).unwrap(),
        CallState::Releasing(reason)
    );
    assert!(!session.media_ready());
    for (index, party) in [Party::Caller, Party::Callee].into_iter().enumerate() {
        let order = session.order(party).unwrap().clone();
        assert!(matches!(order.message(), TrafficMessage::Release { .. }));
        let now = 81 + index as u64 * 3;
        session
            .order_transmitted(&handles[index], &order, now)
            .unwrap();
        session
            .receive(&handles[index], &mobile_ack(OrderKind::Release), now + 1)
            .unwrap();
        assert!(session.stopped_handle().is_none());
        let stop = session.stop_request(party).unwrap().clone();
        session.bearer_stopped(&stop, now + 2).unwrap();
        if party == Party::Caller {
            assert!(session.stopped_handle().is_none());
        }
    }
    assert_eq!(session.state(), CallState::Stopped(reason));
    assert!(session.stopped_handle().is_some());
}

#[test]
fn dtmf_requests_get_dedicated_responses_without_changing_conversation() {
    use crate::l3::legacy_traffic::{DtmfDigit, DtmfMessage};
    let mut session = session();
    let handles = establish(&mut session);
    let request = TrafficMessage::Dtmf(DtmfMessage::Continuous {
        request_number: 15,
        digit: DtmfDigit::Five,
    });
    assert!(matches!(
        session.receive(&handles[0], &request, 35),
        Err(SessionError::State)
    ));
    connect(&mut session, &handles);
    for (step, request) in [
        request,
        TrafficMessage::Dtmf(DtmfMessage::Burst {
            request_number: 15,
            digits: Some(vec![DtmfDigit::One, DtmfDigit::Hash]),
        }),
        TrafficMessage::Dtmf(DtmfMessage::Continuous {
            request_number: 0,
            digit: DtmfDigit::Null,
        }),
    ]
    .into_iter()
    .enumerate()
    {
        let now = 80 + step as u64 * 4;
        assert_eq!(
            session.receive(&handles[0], &request, now).unwrap(),
            CallState::Connected
        );
        let response = session.response(Party::Caller).unwrap().clone();
        let TrafficMessage::Dtmf(request) = request else {
            unreachable!()
        };
        assert_eq!(
            response.message(),
            &TrafficMessage::Dtmf(request.acknowledgment().unwrap())
        );
        assert_eq!(
            session.response_transmitted(&response, now + 1).unwrap(),
            CallState::Connected
        );
        assert!(session.media_ready());
    }
    assert!(matches!(
        session.receive(
            &handles[0],
            &TrafficMessage::Dtmf(DtmfMessage::ContinuousAck { request_number: 0 }),
            100
        ),
        Err(SessionError::Message)
    ));
    assert!(matches!(
        session.receive(
            &handles[0],
            &TrafficMessage::Dtmf(DtmfMessage::Continuous {
                request_number: 16,
                digit: DtmfDigit::One
            }),
            101
        ),
        Err(SessionError::Message)
    ));
    session.end(EndReason::Cancelled, 102).unwrap();
    assert!(matches!(
        session.receive(
            &handles[0],
            &TrafficMessage::Dtmf(DtmfMessage::Continuous {
                request_number: 0,
                digit: DtmfDigit::Null
            }),
            103
        ),
        Err(SessionError::State)
    ));
}

#[test]
fn flash_receipts_preserve_call_state_deduplicate_requests_and_cancel_pending_notifications() {
    use crate::l3::legacy_traffic::{
        CallInformation, CallingNumber, FlashMessage, ReverseFlashFields,
    };
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    let request = TrafficMessage::Flash(FlashMessage::Reverse {
        request_number: 15,
        request: ReverseFlashFields {
            feature: Some(vec![7]),
            keypad: Some("*2".into()),
            ..Default::default()
        },
    });
    session.receive(&handles[0], &request, 80).unwrap();
    let response = session.response(Party::Caller).unwrap().clone();
    session.receive(&handles[0], &request, 81).unwrap();
    assert!(session.response(Party::Caller).unwrap().matches(&response));
    assert_eq!(session.flash_received_count(Party::Caller), 1);
    session.response_transmitted(&response, 82).unwrap();
    assert!(session.media_ready());
    let fields = CallInformation {
        calling_numbers: vec![CallingNumber::network("1001").unwrap()],
        ..Default::default()
    };
    session
        .request_call_information(Party::Callee, fields.clone(), 90)
        .unwrap();
    let tx = session.flash_transmission(Party::Callee).unwrap().clone();
    let TrafficMessage::Flash(message) = tx.message() else {
        panic!()
    };
    let ack = TrafficMessage::Flash(message.acknowledgment().unwrap());
    assert!(matches!(
        session.receive(&handles[1], &ack, 91),
        Err(SessionError::Flash(FlashError::NotTransmitted))
    ));
    session.flash_transmitted(Party::Callee, &tx, 100).unwrap();
    session.receive(&handles[1], &ack, 101).unwrap();
    assert_eq!(
        session.flash_state(Party::Callee),
        Some(FlashState::Acknowledged)
    );
    assert!(session.media_ready());
    session
        .request_call_information(Party::Callee, fields, 110)
        .unwrap();
    let tx = session.flash_transmission(Party::Callee).unwrap().clone();
    session.end(EndReason::Cancelled, 111).unwrap();
    assert_eq!(
        session.flash_state(Party::Callee),
        Some(FlashState::Cancelled)
    );
    assert!(matches!(
        session.flash_transmitted(Party::Callee, &tx, 112),
        Err(SessionError::Flash(FlashError::Closed))
    ));
    assert!(session.stopped_handle().is_none());
}

#[test]
fn flash_request_is_not_recorded_if_its_acknowledgment_cannot_be_retained() {
    use crate::l3::legacy_traffic::{DtmfDigit, DtmfMessage, FlashMessage, ReverseFlashFields};
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    for request_number in 0..16 {
        session
            .receive(
                &handles[0],
                &TrafficMessage::Dtmf(DtmfMessage::Continuous {
                    request_number,
                    digit: DtmfDigit::One,
                }),
                80 + u64::from(request_number),
            )
            .unwrap();
    }
    let message = TrafficMessage::Flash(FlashMessage::Reverse {
        request_number: 0,
        request: ReverseFlashFields {
            feature: Some(vec![7]),
            ..Default::default()
        },
    });
    assert!(matches!(
        session.receive(&handles[0], &message, 100),
        Err(SessionError::ResponseQueueFull)
    ));
    assert_eq!(session.flash_received_count(Party::Caller), 0);
    assert!(session.flash_received(Party::Caller).is_none());
    assert!(session.media_ready());
}

#[test]
fn flash_timeout_and_late_acknowledgment_leave_the_conversation_running() {
    use crate::l3::legacy_traffic::{CallInformation, CallingNumber};
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    session
        .request_call_information(
            Party::Callee,
            CallInformation {
                calling_numbers: vec![CallingNumber::network("1001").unwrap()],
                ..Default::default()
            },
            100,
        )
        .unwrap();
    let mut last_ack = None;
    for sent in [110, 320, 530] {
        let tx = session.flash_transmission(Party::Callee).unwrap().clone();
        let TrafficMessage::Flash(message) = tx.message() else {
            panic!()
        };
        last_ack = message.acknowledgment();
        session.flash_transmitted(Party::Callee, &tx, sent).unwrap();
        session.advance(sent + 200).unwrap();
    }
    assert_eq!(
        session.flash_state(Party::Callee),
        Some(FlashState::TimedOut)
    );
    assert!(session.media_ready());
    assert!(matches!(
        session.receive(&handles[1], &TrafficMessage::Flash(last_ack.unwrap()), 731),
        Err(SessionError::Flash(FlashError::Closed))
    ));
    assert!(session.media_ready());
}
#[test]
fn service_response_waits_for_request_ack_and_only_its_ack_completes_exchange() {
    use crate::l3::legacy_traffic::{ServiceMessage, ServiceOptions};
    let mut session = session();
    let handles = establish(&mut session);
    let request = TrafficMessage::Service(ServiceMessage::Request {
        request_number: 15,
        service_code: crate::l3::page_response::ServiceCode::AsyncData as u8,
        options: ServiceOptions::default(),
    });
    assert_eq!(
        session.receive(&handles[0], &request, 30),
        Err(SessionError::State)
    );
    connect(&mut session, &handles);
    session.receive(&handles[0], &request, 80).unwrap();
    assert!(session.service_transmission(Party::Caller).is_none());
    let ack = session.response(Party::Caller).unwrap().clone();
    assert_eq!(
        ack.message(),
        &TrafficMessage::Ack {
            ack_message_type: ServiceMessage::REQUEST_TYPE,
            information: Default::default()
        }
    );
    session.response_transmitted(&ack, 90).unwrap();
    let tx = session.service_transmission(Party::Caller).unwrap().clone();
    assert!(
        matches!(tx.message(), TrafficMessage::Service(ServiceMessage::Response { request_number: 0, service_code: 6, causes: Some(causes), .. }) if causes == &[7])
    );
    let response_ack = TrafficMessage::Ack {
        ack_message_type: ServiceMessage::RESPONSE_TYPE,
        information: Default::default(),
    };
    assert_eq!(
        session.receive(&handles[0], &response_ack, 95),
        Err(SessionError::Service(ServiceError::NotTransmitted))
    );
    session
        .service_transmitted(Party::Caller, &tx, 100)
        .unwrap();
    assert!(session.receive(&handles[1], &response_ack, 105).is_err());
    assert_ne!(
        session.service_state(Party::Caller),
        Some(ServiceState::Acknowledged)
    );
    session.receive(&handles[0], &response_ack, 110).unwrap();
    assert_eq!(
        session.service_state(Party::Caller),
        Some(ServiceState::Acknowledged)
    );
    assert!(session.media_ready());
    let baseline = TrafficMessage::Service(ServiceMessage::Request {
        request_number: 0,
        service_code: 1,
        options: ServiceOptions::default(),
    });
    session.receive(&handles[0], &baseline, 120).unwrap();
    let ack = session.response(Party::Caller).unwrap().clone();
    session.response_transmitted(&ack, 130).unwrap();
    let stale = session.service_transmission(Party::Caller).unwrap().clone();
    assert!(matches!(
        stale.message(),
        TrafficMessage::Service(ServiceMessage::Response {
            request_number: 1,
            service_code: 1,
            causes: None,
            ..
        })
    ));
    session.end(EndReason::Cancelled, 140).unwrap();
    assert_eq!(
        session.service_state(Party::Caller),
        Some(ServiceState::Cancelled)
    );
    assert!(session.service_transmission(Party::Caller).is_none());
    assert_eq!(
        session.service_transmitted(Party::Caller, &stale, 150),
        Err(SessionError::Service(ServiceError::Closed))
    );
}
#[test]
fn service_request_queue_failure_is_atomic_and_response_timeout_preserves_voice() {
    use crate::l3::legacy_traffic::{DtmfDigit, DtmfMessage, ServiceMessage, ServiceOptions};
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    for number in 0..MAX_PENDING_RESPONSES {
        session
            .receive(
                &handles[0],
                &TrafficMessage::Dtmf(DtmfMessage::Continuous {
                    request_number: number as u8,
                    digit: DtmfDigit::One,
                }),
                80,
            )
            .unwrap();
    }
    let request = TrafficMessage::Service(ServiceMessage::Request {
        request_number: 2,
        service_code: 1,
        options: ServiceOptions::default(),
    });
    assert_eq!(
        session.receive(&handles[0], &request, 90),
        Err(SessionError::ResponseQueueFull)
    );
    assert_eq!(session.service_count(Party::Caller), 0);
    while let Some(ack) = session.response(Party::Caller).cloned() {
        session.response_transmitted(&ack, 100).unwrap();
    }
    session.receive(&handles[0], &request, 110).unwrap();
    let ack = session.response(Party::Caller).unwrap().clone();
    session.response_transmitted(&ack, 120).unwrap();
    for now in [130, 330, 530] {
        let tx = session.service_transmission(Party::Caller).unwrap().clone();
        session
            .service_transmitted(Party::Caller, &tx, now)
            .unwrap();
        session.advance(now + 200).unwrap();
    }
    assert_eq!(
        session.service_state(Party::Caller),
        Some(ServiceState::TimedOut)
    );
    assert!(session.media_ready());
}

#[test]
fn alert_has_configured_caller_identity_before_any_answer_and_overlong_numbers_are_unavailable() {
    for (number, presentation, characters) in [("1001".into(), 0, "1001"), ("1".repeat(16), 2, "")]
    {
        let mut reserved = reservation();
        reserved.caller.number = number;
        let mut session = CallSession::try_new(reserved, policy(), 0).unwrap();
        let _handles = establish(&mut session);
        assert_eq!(session.state(), CallState::Alerting);
        assert!(!session.media_ready());
        let TrafficMessage::AlertWithInfo { information } =
            session.order(Party::Callee).unwrap().message()
        else {
            panic!()
        };
        assert_eq!(information.calling_numbers.len(), 1);
        let caller = &information.calling_numbers[0];
        assert_eq!(caller.presentation, presentation);
        assert_eq!(caller.characters, characters);
        assert_eq!(caller.screening, 3);
    }
}

#[test]
fn release_reason_and_return_target_survive_retries_until_full_ack_and_actual_stop_callbacks() {
    use crate::l3::{legacy_traffic::Hyperband, relay::Direction};
    let target = ControlChannel {
        hyperband: Hyperband::Cellular800,
        channel: 991,
        dvcc: 42,
    };
    let mut session = session();
    session.set_return_channel(target).unwrap();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    assert_eq!(session.set_return_channel(target), Err(SessionError::State));
    let release = TrafficMessage::Release {
        information: ReleaseInformation {
            fields: ReleaseFields::Reason(ReleaseReason::PowerDown),
            ..Default::default()
        },
    };
    session.receive(&handles[0], &release, 80).unwrap();
    assert_eq!(
        session.release_reason(Party::Caller),
        Some(ReleaseReason::PowerDown)
    );
    let ack = session.response(Party::Caller).unwrap().clone();
    let TrafficMessage::Ack { information, .. } = ack.message() else {
        panic!()
    };
    assert_eq!(information.control_channels, vec![target]);
    assert_eq!(
        crate::link::traffic::encode(ack.message()).unwrap().len(),
        2
    );
    assert_eq!(
        crate::l3::legacy_traffic::decode_prefix_in(
            &ack.message().encode().unwrap(),
            Direction::Forward
        )
        .unwrap()
        .message,
        *ack.message()
    );
    assert!(session.stop_request(Party::Caller).is_none());
    let first = session.order(Party::Callee).unwrap().clone();
    let expected = TrafficMessage::Release {
        information: ReleaseInformation {
            fields: ReleaseFields::ControlChannels(vec![target]),
            ..Default::default()
        },
    };
    assert_eq!(first.message(), &expected);
    session.order_transmitted(&handles[1], &first, 90).unwrap();
    session.advance(190).unwrap();
    let retry = session.order(Party::Callee).unwrap().clone();
    assert_eq!(retry.message(), &expected);
    assert_eq!(
        session.order_transmitted(&handles[1], &first, 191),
        Err(SessionError::Order(OrderError::StaleTransmission))
    );
    session.order_transmitted(&handles[1], &retry, 192).unwrap();
    session.response_transmitted(&ack, 193).unwrap();
    assert!(session.stop_request(Party::Caller).is_some());
    assert!(session.stop_request(Party::Callee).is_none());
    session
        .receive(&handles[1], &mobile_ack(OrderKind::Release), 194)
        .unwrap();
    for party in [Party::Caller, Party::Callee] {
        let stop = session.stop_request(party).unwrap().clone();
        session.bearer_stopped(&stop, 195).unwrap();
    }
    assert_eq!(
        session.state(),
        CallState::Stopped(EndReason::MobileRelease(Party::Caller))
    );
    assert_eq!(
        session.release_reason(Party::Caller),
        Some(ReleaseReason::PowerDown)
    );
}
#[test]
fn reverse_session_rejects_forward_termination_fields_without_changing_call_state() {
    use crate::l3::legacy_traffic::Hyperband;
    let target = ControlChannel {
        hyperband: Hyperband::Pcs1900,
        channel: 1,
        dvcc: 1,
    };
    let mut session = session();
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    let messages = [
        TrafficMessage::Release {
            information: ReleaseInformation {
                fields: ReleaseFields::ControlChannels(vec![target]),
                ..Default::default()
            },
        },
        TrafficMessage::Ack {
            ack_message_type: ReleaseInformation::MESSAGE_TYPE,
            information: AcknowledgmentInformation {
                control_channels: vec![target],
                ..Default::default()
            },
        },
    ];
    for message in messages {
        assert!(matches!(
            session.receive(&handles[0], &message, 80),
            Err(SessionError::Control(_))
        ));
        assert_eq!(session.state(), CallState::Connected);
        assert_eq!(session.release_reason(Party::Caller), None);
    }
}
