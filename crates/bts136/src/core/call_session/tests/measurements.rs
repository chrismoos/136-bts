use super::*;
mod alignment;
use crate::l3::legacy_traffic::{
    ExtendedChannelQualityReport, ExtendedCurrentQuality, ExtendedMeasurementOrder,
    ExtendedMeasurementTargets, ExtendedMeasurementType, ExtendedQualityMetric, Hyperband,
    MeasurementChannel, NeighborGroup,
};
fn request(count: usize) -> ExtendedMeasurementOrder {
    ExtendedMeasurementOrder {
        targets: ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::WordErrorRate,
            (0..count)
                .map(|index| MeasurementChannel {
                    hyperband: Hyperband::Pcs1900,
                    channel: index as u16,
                })
                .collect(),
        )
        .unwrap(),
        mobile_ack_support: None,
    }
}
fn first(count: usize) -> ExtendedChannelQualityReport {
    ExtendedChannelQualityReport::First {
        current: ExtendedCurrentQuality {
            ber_code: 3,
            measurement_code: 12,
            rssi_code: 42,
        },
        neighbor_rssi_codes: std::array::from_fn(
            |index| if index < count { (index + 1) as u8 } else { 0 },
        ),
    }
}
fn connected(version: ProtocolVersion) -> (CallSession, [BearerHandle; 2]) {
    let mut session = session();
    for party in [Party::Caller, Party::Callee] {
        let assignment = session.assignment(party).unwrap();
        session.bind_protocol(&assignment.handle, version).unwrap();
    }
    let handles = establish(&mut session);
    connect(&mut session, &handles);
    session.advance(71).unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 75).unwrap();
    session
        .receive(
            &handles[0],
            &mobile_ack(OrderKind::MeasureServingChannel),
            80,
        )
        .unwrap();
    (session, handles)
}
#[test]
fn bearer_protocol_is_bound_once_before_assignment_and_controls_extended_admission() {
    let mut session = session();
    let caller = session.assignment(Party::Caller).unwrap().handle;
    assert_eq!(session.protocol_version(Party::Caller), None);
    assert!(matches!(
        session.request_extended_measurements(&caller, request(0), 0),
        Err(SessionError::ProtocolContext)
    ));
    session
        .bind_protocol(&caller, ProtocolVersion::Pv2)
        .unwrap();
    session
        .bind_protocol(&caller, ProtocolVersion::Pv2)
        .unwrap();
    assert_eq!(
        session.bind_protocol(&caller, ProtocolVersion::Pv6),
        Err(SessionError::ProtocolContext)
    );
    session.assignment_started(&caller, 10).unwrap();
    assert_eq!(
        session.bind_protocol(&caller, ProtocolVersion::Pv2),
        Err(SessionError::State)
    );
    assert_eq!(
        session.protocol_version(Party::Caller),
        Some(ProtocolVersion::Pv2)
    );
    let (mut baseline, handles) = connected(ProtocolVersion::Is136RevA);
    let before = baseline.measurement_state(Party::Caller);
    assert!(matches!(
        baseline.request_extended_measurements(&handles[0], request(4), 100),
        Err(SessionError::Order(OrderError::MeasurementFields))
    ));
    assert_eq!(baseline.measurement_state(Party::Caller), before);
    assert!(baseline.extended_measurements(Party::Caller).is_none());
    let (other, foreign) = connected(ProtocolVersion::Pv2);
    assert!(matches!(
        baseline.request_extended_measurements(&foreign[0], request(0), 101),
        Err(SessionError::StaleBearer)
    ));
    assert_eq!(
        other.protocol_version(Party::Caller),
        Some(ProtocolVersion::Pv2)
    );
}
#[test]
fn requests_serialize_and_reports_start_after_final_tx_without_acknowledging_the_order() {
    let (mut session, handles) = connected(ProtocolVersion::Pv2);
    let requested = request(22);
    let owner = session
        .request_extended_measurements(&handles[0], requested.clone(), 100)
        .unwrap();
    assert!(session.active_measurement_handle(Party::Caller).is_none());
    assert!(matches!(
        session.record_extended_measurement(&owner, &first(4), 101),
        Err(SessionError::State)
    ));
    let tx = session.order(Party::Caller).unwrap().clone();
    assert!(matches!(
        session.request_extended_measurements(&handles[0], request(0), 102),
        Err(SessionError::State)
    ));
    session
        .request_extended_measurements(&handles[0], requested.clone(), 103)
        .unwrap();
    assert!(session.order(Party::Caller).unwrap().matches(&tx));
    session.order_transmitted(&handles[0], &tx, 130).unwrap();
    let active = session.active_measurement_handle(Party::Caller).unwrap();
    session
        .record_extended_measurement(&active, &first(4), 150)
        .unwrap();
    let awaiting = session.measurement_state(Party::Caller);
    assert!(matches!(awaiting, Some(OrderState::AwaitingAck { .. })));
    for (group, start) in [
        (NeighborGroup::Second, 4),
        (NeighborGroup::Third, 10),
        (NeighborGroup::Fourth, 16),
    ] {
        session
            .record_extended_measurement(
                &owner,
                &ExtendedChannelQualityReport::Later {
                    group,
                    neighbor_rssi_codes: std::array::from_fn(|offset| (start + offset + 1) as u8),
                },
                160,
            )
            .unwrap();
    }
    let snapshot = session.extended_measurements(Party::Caller).unwrap();
    assert_eq!(
        snapshot.neighbor_rssi_codes,
        (1..=22).map(Some).collect::<Vec<_>>()
    );
    assert_eq!(
        snapshot.serving.unwrap().metric,
        ExtendedQualityMetric::WordErrorRate(12)
    );
    assert_eq!(snapshot.received_monotonic_ms, Some(160));
    assert_eq!(session.measurement_state(Party::Caller), awaiting);
    session
        .receive(
            &handles[0],
            &TrafficMessage::ExtendedMeasurementAck(requested.targets),
            170,
        )
        .unwrap();
    assert_eq!(
        session.measurement_state(Party::Caller),
        Some(OrderState::Acknowledged)
    );
    assert!(session.media_ready());
}
#[test]
fn report_owners_and_samples_do_not_survive_replacement_stop_or_release() {
    let (mut session, handles) = connected(ProtocolVersion::Pv2);
    let original = request(2);
    let old = session
        .request_extended_measurements(&handles[0], original.clone(), 100)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 110).unwrap();
    session
        .receive(
            &handles[0],
            &TrafficMessage::ExtendedMeasurementAck(original.targets),
            120,
        )
        .unwrap();
    session
        .record_extended_measurement(&old, &first(2), 121)
        .unwrap();
    let replacement = request(4);
    let new = session
        .request_extended_measurements(&handles[0], replacement, 130)
        .unwrap();
    assert_ne!(old, new);
    assert_eq!(new, new.clone());
    assert!(session.extended_measurements(Party::Caller).is_none());
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 140).unwrap();
    assert!(matches!(
        session.record_extended_measurement(&old, &first(2), 141),
        Err(SessionError::State)
    ));
    session
        .record_extended_measurement(&new, &first(4), 142)
        .unwrap();
    session
        .request_extended_measurements(&handles[0], request(4), 143)
        .unwrap();
    assert!(
        session
            .extended_measurements(Party::Caller)
            .unwrap()
            .received_monotonic_ms
            == Some(142)
    );
    session
        .record_extended_measurement(&new, &first(4), 144)
        .unwrap();
    session.set_measurements(Party::Caller, false, 150).unwrap();
    assert!(session.extended_measurements(Party::Caller).is_none());
    assert!(session.active_measurement_handle(Party::Caller).is_none());
    assert!(
        session
            .record_extended_measurement(&new, &first(4), 151)
            .is_err()
    );
    assert!(matches!(
        session.order_transmitted(&handles[0], &tx, 152),
        Err(SessionError::Order(OrderError::StaleTransmission))
    ));
    let stop = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &stop, 160).unwrap();
    session
        .receive(&handles[0], &mobile_ack(OrderKind::StopMeasurements), 170)
        .unwrap();
    let latest = session
        .request_extended_measurements(&handles[0], request(0), 180)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 190).unwrap();
    session
        .receive(
            &handles[0],
            &TrafficMessage::Release {
                information: Default::default(),
            },
            200,
        )
        .unwrap();
    assert!(session.extended_measurements(Party::Caller).is_none());
    assert!(
        session
            .record_extended_measurement(&latest, &first(0), 201)
            .is_err()
    );
}
#[test]
fn retries_preserve_measurement_owner_and_samples_until_exact_overall_timeout() {
    let (mut session, handles) = connected(ProtocolVersion::Pv2);
    let requested = request(2);
    let owner = session
        .request_extended_measurements(&handles[0], requested, 100)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 130).unwrap();
    session
        .record_extended_measurement(&owner, &first(2), 150)
        .unwrap();
    let before = session
        .extended_measurements(Party::Caller)
        .unwrap()
        .clone();
    session.advance(230).unwrap();
    let retry = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &retry, 240).unwrap();
    assert_eq!(session.extended_measurements(Party::Caller), Some(&before));
    session
        .record_extended_measurement(&owner, &first(2), 241)
        .unwrap();
    session.advance(340).unwrap();
    let retry = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &retry, 590).unwrap();
    assert!(session.extended_measurements(Party::Caller).is_some());
    session.advance(600).unwrap();
    assert_eq!(
        session.measurement_state(Party::Caller),
        Some(OrderState::TimedOut)
    );
    assert!(session.extended_measurements(Party::Caller).is_none());
    assert!(session.active_measurement_handle(Party::Caller).is_none());
    assert!(
        session
            .record_extended_measurement(&owner, &first(2), 601)
            .is_err()
    );
    assert!(session.media_ready());
}
#[test]
fn malformed_or_unrequested_report_groups_cannot_partially_change_samples() {
    let (mut session, handles) = connected(ProtocolVersion::Pv2);
    let owner = session
        .request_extended_measurements(&handles[0], request(2), 100)
        .unwrap();
    let tx = session.order(Party::Caller).unwrap().clone();
    session.order_transmitted(&handles[0], &tx, 110).unwrap();
    session
        .record_extended_measurement(&owner, &first(2), 120)
        .unwrap();
    let before = session
        .extended_measurements(Party::Caller)
        .unwrap()
        .clone();
    for report in [
        first(4),
        ExtendedChannelQualityReport::Later {
            group: NeighborGroup::Second,
            neighbor_rssi_codes: [0; 6],
        },
        ExtendedChannelQualityReport::First {
            current: ExtendedCurrentQuality {
                ber_code: 16,
                measurement_code: 0,
                rssi_code: 0,
            },
            neighbor_rssi_codes: [0; 4],
        },
    ] {
        assert!(matches!(
            session.record_extended_measurement(&owner, &report, 121),
            Err(SessionError::Control(_))
        ));
        assert_eq!(session.extended_measurements(Party::Caller), Some(&before));
    }
    let (mut other, _) = connected(ProtocolVersion::Pv2);
    assert!(matches!(
        other.record_extended_measurement(&owner, &first(2), 122),
        Err(SessionError::StaleBearer)
    ));
    assert!(other.extended_measurements(Party::Caller).is_none());
}

#[test]
fn traffic_capability_requests_use_the_frozen_assigned_protocol() {
    for version in [
        ProtocolVersion::Is136RevA,
        ProtocolVersion::Pv2,
        ProtocolVersion::Pv6,
    ] {
        let (mut session, handles) = connected(version);
        session.request_capabilities(Party::Caller, 100).unwrap();
        let query = session.order(Party::Caller).unwrap().clone();
        assert_eq!(
            query.message(),
            &TrafficMessage::CapabilityUpdateRequest(version)
        );
        assert_eq!(session.protocol_version(Party::Caller), Some(version));
        assert_eq!(
            session.bind_protocol(&handles[0], ProtocolVersion::Pv7),
            Err(SessionError::State)
        );
    }
}
