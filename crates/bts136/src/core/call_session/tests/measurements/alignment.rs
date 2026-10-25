use super::*;
use crate::l3::{
    designation::TimeAlignment,
    legacy_traffic::{MAX_ALIGNMENT_UNITS, PhysicalControlFields, PhysicalControlRequest},
};
fn advance(units: u8) -> PhysicalControlRequest {
    PhysicalControlRequest {
        time_alignment: Some(TimeAlignment::OffsetUnits(units)),
        ..Default::default()
    }
}
fn acknowledgment(control: PhysicalControlRequest) -> TrafficMessage {
    TrafficMessage::PhysicalControlAck(PhysicalControlFields {
        control,
        ..Default::default()
    })
}
fn neighbors() -> (CallSession, [BearerHandle; 2], MeasurementHandle) {
    let (mut session, handles) = connected(ProtocolVersion::Pv2);
    let order = request(2);
    let owner = session
        .request_extended_measurements(&handles[0], order.clone(), 100)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 110).unwrap();
    session
        .receive(
            &handles[0],
            &TrafficMessage::ExtendedMeasurementAck(order.targets),
            120,
        )
        .unwrap();
    session
        .record_extended_measurement(&owner, &first(2), 121)
        .unwrap();
    (session, handles, owner)
}
fn control_roundtrip(
    session: &mut CallSession,
    handle: &BearerHandle,
    control: PhysicalControlRequest,
    now: u64,
) {
    session
        .request_physical_control(Party::Caller, control, now)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    assert!(matches!(tx.message(), TrafficMessage::PhysicalControl(_)));
    session.order_transmitted(handle, &tx, now + 10).unwrap();
    session
        .receive(handle, &acknowledgment(control), now + 20)
        .unwrap();
}
#[test]
fn maximum_advance_waits_for_stop_and_permits_serving_only_until_lower_alignment_is_confirmed() {
    let (mut session, handles, owner) = neighbors();
    session
        .request_physical_control(Party::Caller, advance(MAX_ALIGNMENT_UNITS), 130)
        .unwrap();
    assert!(session.extended_measurements(Party::Caller).is_none());
    assert!(
        session
            .record_extended_measurement(&owner, &first(2), 131)
            .is_err()
    );
    assert!(matches!(
        session.request_extended_measurements(&handles[0], request(2), 132),
        Err(SessionError::Control(_))
    ));
    let stop = session.order(Party::Caller).unwrap().clone();
    assert_eq!(stop.message(), &TrafficMessage::StopMeasurements);
    session.order_transmitted(&handles[0], &stop, 140).unwrap();
    assert!(session.order(Party::Caller).is_none());
    assert_eq!(
        session.receive(
            &handles[0],
            &acknowledgment(advance(MAX_ALIGNMENT_UNITS)),
            141
        ),
        Err(SessionError::Order(OrderError::NotTransmitted))
    );
    assert_eq!(
        session
            .confirmed_physical_control(Party::Caller)
            .control
            .time_alignment,
        None
    );
    session
        .receive(&handles[0], &mobile_ack(OrderKind::StopMeasurements), 150)
        .unwrap();
    let control = session.order(Party::Caller).unwrap().clone();
    assert!(matches!(
        control.message(),
        TrafficMessage::PhysicalControl(_)
    ));
    session
        .order_transmitted(&handles[0], &control, 160)
        .unwrap();
    session
        .receive(
            &handles[0],
            &acknowledgment(advance(MAX_ALIGNMENT_UNITS)),
            170,
        )
        .unwrap();
    assert_eq!(
        session
            .confirmed_physical_control(Party::Caller)
            .control
            .time_alignment,
        Some(TimeAlignment::OffsetUnits(MAX_ALIGNMENT_UNITS))
    );
    let serving = request(0);
    session
        .request_extended_measurements(&handles[0], serving.clone(), 180)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 190).unwrap();
    session
        .receive(
            &handles[0],
            &TrafficMessage::ExtendedMeasurementAck(serving.targets),
            200,
        )
        .unwrap();
    control_roundtrip(
        &mut session,
        &handles[0],
        advance(MAX_ALIGNMENT_UNITS - 1),
        210,
    );
    assert!(
        session
            .request_extended_measurements(&handles[0], request(2), 240)
            .is_ok()
    );
    assert!(session.media_ready());
}
#[test]
fn power_only_control_keeps_alignment_and_invalid_or_unconfirmed_ack_cannot_restore_neighbors() {
    let (mut session, handles) = connected(ProtocolVersion::Pv2);
    control_roundtrip(&mut session, &handles[0], advance(MAX_ALIGNMENT_UNITS), 100);
    let power = PhysicalControlRequest {
        power_level: Some(5),
        ..Default::default()
    };
    session
        .request_physical_control(Party::Caller, power, 130)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 140).unwrap();
    let invalid = PhysicalControlRequest {
        time_alignment: Some(TimeAlignment::OffsetUnits(MAX_ALIGNMENT_UNITS + 1)),
        ..power
    };
    assert!(matches!(
        session.receive(&handles[0], &acknowledgment(invalid), 141),
        Err(SessionError::Control(_))
    ));
    session
        .receive(&handles[0], &acknowledgment(power), 150)
        .unwrap();
    assert_eq!(
        session
            .confirmed_physical_control(Party::Caller)
            .control
            .time_alignment,
        Some(TimeAlignment::OffsetUnits(MAX_ALIGNMENT_UNITS))
    );
    assert_eq!(
        session
            .confirmed_physical_control(Party::Caller)
            .control
            .power_level,
        Some(5)
    );
    session
        .request_physical_control(Party::Caller, advance(MAX_ALIGNMENT_UNITS - 1), 160)
        .unwrap();
    assert!(matches!(
        session.request_extended_measurements(&handles[0], request(1), 161),
        Err(SessionError::Control(_))
    ));
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 170).unwrap();
    assert_eq!(
        session.receive(
            &handles[0],
            &acknowledgment(advance(MAX_ALIGNMENT_UNITS - 2)),
            180
        ),
        Err(SessionError::Order(OrderError::ControlFields))
    );
    assert!(matches!(
        session.request_extended_measurements(&handles[0], request(1), 181),
        Err(SessionError::Control(_))
    ));
    session
        .receive(
            &handles[0],
            &acknowledgment(advance(MAX_ALIGNMENT_UNITS - 1)),
            190,
        )
        .unwrap();
    assert!(
        session
            .request_extended_measurements(&handles[0], request(1), 200)
            .is_ok()
    );
    assert_eq!(
        session
            .confirmed_physical_control(Party::Caller)
            .control
            .power_level,
        Some(5)
    );
}
#[test]
fn transmitted_measurement_stop_releases_maximum_control_after_response_budget() {
    let (mut session, handles, _) = neighbors();
    session
        .request_physical_control(Party::Caller, advance(MAX_ALIGNMENT_UNITS), 130)
        .unwrap();
    for (transmit, expire) in [(140, 240), (250, 350), (360, 460)] {
        let stop = session.order(Party::Caller).unwrap().clone();
        assert_eq!(stop.message(), &TrafficMessage::StopMeasurements);
        session
            .order_transmitted(&handles[0], &stop, transmit)
            .unwrap();
        session.advance(expire).unwrap();
    }
    assert_eq!(
        session.measurement_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    let control = session.order(Party::Caller).unwrap().clone();
    session
        .order_transmitted(&handles[0], &control, 470)
        .unwrap();
    session
        .receive(
            &handles[0],
            &acknowledgment(advance(MAX_ALIGNMENT_UNITS)),
            480,
        )
        .unwrap();
    assert!(session.physical_control_transmitted(Party::Caller));
    assert!(matches!(
        session.request_extended_measurements(&handles[0], request(1), 640),
        Err(SessionError::Control(_))
    ));
    let maintain = PhysicalControlRequest {
        time_alignment: Some(TimeAlignment::Maintain),
        ..Default::default()
    };
    session
        .request_physical_control(Party::Caller, maintain, 650)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 660).unwrap();
    session
        .receive(&handles[0], &acknowledgment(advance(0)), 670)
        .unwrap();
    assert!(
        session
            .request_extended_measurements(&handles[0], request(1), 680)
            .is_ok()
    );
    assert!(session.media_ready());
}
#[test]
fn maintain_response_that_reveals_maximum_alignment_stops_existing_neighbors() {
    let (mut session, handles, owner) = neighbors();
    let maintain = PhysicalControlRequest {
        time_alignment: Some(TimeAlignment::Maintain),
        ..Default::default()
    };
    session
        .request_physical_control(Party::Caller, maintain, 130)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 140).unwrap();
    session
        .receive(
            &handles[0],
            &acknowledgment(advance(MAX_ALIGNMENT_UNITS)),
            150,
        )
        .unwrap();
    assert!(session.extended_measurements(Party::Caller).is_none());
    assert!(
        session
            .record_extended_measurement(&owner, &first(2), 151)
            .is_err()
    );
    assert_eq!(
        session.order(Party::Caller).unwrap().message(),
        &TrafficMessage::StopMeasurements
    );
    for (transmit, expire) in [(160, 260), (270, 370), (380, 480)] {
        let stop = session.order(Party::Caller).unwrap().clone();
        session
            .order_transmitted(&handles[0], &stop, transmit)
            .unwrap();
        session.advance(expire).unwrap();
    }
    assert_eq!(
        session.measurement_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    session
        .receive(
            &handles[0],
            &acknowledgment(advance(MAX_ALIGNMENT_UNITS)),
            490,
        )
        .unwrap();
    assert_eq!(
        session.measurement_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    assert!(session.order(Party::Caller).is_none());
}

#[test]
fn serving_only_replacement_cannot_bypass_the_alignment_transition_barrier() {
    let (mut session, handles, _) = neighbors();
    session
        .request_physical_control(Party::Caller, advance(MAX_ALIGNMENT_UNITS), 130)
        .unwrap();
    session.set_measurements(Party::Caller, true, 131).unwrap();
    let serving = session.order(Party::Caller).unwrap().clone();
    assert_eq!(
        serving.message(),
        &TrafficMessage::MeasurementOrder {
            rf_channels: vec![]
        }
    );
    session
        .order_transmitted(&handles[0], &serving, 140)
        .unwrap();
    assert!(session.order(Party::Caller).is_none());
    assert!(!session.physical_control_transmitted(Party::Caller));
    session
        .receive(
            &handles[0],
            &mobile_ack(OrderKind::MeasureServingChannel),
            150,
        )
        .unwrap();
    assert!(matches!(
        session.order(Party::Caller).unwrap().message(),
        TrafficMessage::PhysicalControl(_)
    ));
}
#[test]
fn required_measurement_stop_does_not_preempt_an_already_transmitted_status_query() {
    let (mut session, handles, _) = neighbors();
    session.request_status(Party::Caller, 125).unwrap();
    let status = session.order(Party::Caller).unwrap().clone();
    assert!(matches!(status.message(), TrafficMessage::StatusRequest(_)));
    session
        .order_transmitted(&handles[0], &status, 126)
        .unwrap();
    session
        .request_physical_control(Party::Caller, advance(MAX_ALIGNMENT_UNITS), 130)
        .unwrap();
    assert!(session.order(Party::Caller).is_none());
    session.receive(&handles[0], &status_report(), 140).unwrap();
    assert_eq!(
        session.order(Party::Caller).unwrap().message(),
        &TrafficMessage::StopMeasurements
    );
    assert!(!session.physical_control_transmitted(Party::Caller));
}

#[test]
fn untransmitted_measurement_stop_cannot_release_maximum_control() {
    let (mut session, handles, _) = neighbors();
    session.policy.measurements.overall_timeout_ms = 50;
    session
        .request_physical_control(Party::Caller, advance(MAX_ALIGNMENT_UNITS), 130)
        .unwrap();
    session.advance(180).unwrap();
    assert_eq!(
        session.measurement_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    assert!(session.order(Party::Caller).is_none());
    assert!(!session.physical_control_transmitted(Party::Caller));
    session.advance(630).unwrap();
    assert_eq!(
        session.physical_control_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    assert!(
        session
            .request_extended_measurements(&handles[0], request(1), 640)
            .is_err()
    );
    assert!(session.media_ready());
}
