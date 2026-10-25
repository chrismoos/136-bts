use super::*;
use crate::l3::{
    broadcast::ProtocolVersion,
    legacy_traffic::{
        AcknowledgmentInformation, ControlChannel, ExtendedMeasurementOrder,
        ExtendedMeasurementTargets, ExtendedMeasurementType, Hyperband, MeasurementChannel,
    },
};
const PHONE: MobileIdentity = MobileIdentity::Min34(1);
fn policy() -> OrderPolicy {
    OrderPolicy {
        response_timeout_ms: 100,
        overall_timeout_ms: 500,
        max_transmissions: 3,
    }
}
fn order(support: Option<bool>) -> ExtendedMeasurementOrder {
    ExtendedMeasurementOrder {
        targets: ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::CarrierInterference,
            vec![
                MeasurementChannel {
                    hyperband: Hyperband::Cellular800,
                    channel: 991,
                },
                MeasurementChannel {
                    hyperband: Hyperband::Pcs1900,
                    channel: 42,
                },
            ],
        )
        .unwrap(),
        mobile_ack_support: support,
    }
}
fn exchange(support: Option<bool>) -> TrafficOrderExchange {
    let version = if support.is_some() {
        ProtocolVersion::Pv6
    } else {
        ProtocolVersion::Pv2
    };
    TrafficOrderExchange::try_new_extended_measurements(PHONE, order(support), version, policy(), 0)
        .unwrap()
}
fn typed_ack() -> TrafficMessage {
    TrafficMessage::ExtendedMeasurementAck(order(None).targets)
}
fn generic_ack() -> TrafficMessage {
    TrafficMessage::Ack {
        ack_message_type: TrafficMessage::ExtendedMeasurementOrder(order(Some(true)))
            .message_type(),
        information: Default::default(),
    }
}
#[test]
fn construction_requires_owned_parameters_and_consistent_protocol_version() {
    assert!(matches!(
        TrafficOrderExchange::try_new(PHONE, OrderKind::ExtendedMeasurements, policy(), 0),
        Err(OrderError::MeasurementFields)
    ));
    for (support, version) in [
        (None, ProtocolVersion::Pv1),
        (None, ProtocolVersion::Pv6),
        (Some(false), ProtocolVersion::Pv5),
        (Some(true), ProtocolVersion::Pv2),
    ] {
        assert!(matches!(
            TrafficOrderExchange::try_new_extended_measurements(
                PHONE,
                order(support),
                version,
                policy(),
                0
            ),
            Err(OrderError::MeasurementFields)
        ));
    }
    for identity in [MobileIdentity::Min34(0), MobileIdentity::Min34(1 << 34)] {
        assert!(matches!(
            TrafficOrderExchange::try_new_extended_measurements(
                identity,
                order(None),
                ProtocolVersion::Pv2,
                policy(),
                0
            ),
            Err(OrderError::Identity)
        ));
    }
    assert!(matches!(
        TrafficOrderExchange::try_new_extended_measurements(
            PHONE,
            order(None),
            ProtocolVersion::Pv2,
            OrderPolicy {
                response_timeout_ms: 0,
                ..policy()
            },
            0
        ),
        Err(OrderError::Policy)
    ));
    assert!(matches!(
        TrafficOrderExchange::try_new_extended_measurements(
            PHONE,
            order(None),
            ProtocolVersion::Pv2,
            policy(),
            u64::MAX
        ),
        Err(OrderError::TimeOverflow)
    ));
}
#[test]
fn extended_ack_matches_type_order_band_channel_identity_and_final_air_completion() {
    for support in [None, Some(false), Some(true)] {
        let mut exchange = exchange(support);
        assert_eq!(exchange.kind(), OrderKind::ExtendedMeasurements);
        assert_eq!(
            exchange.acknowledge(PHONE, &typed_ack(), 1),
            Err(OrderError::NotTransmitted)
        );
        let tx = exchange.transmission().unwrap().clone();
        exchange.transmitted(&tx, 10).unwrap();
        let awaiting = exchange.state();
        let mut values = order(None).targets.channels().to_vec();
        values.swap(0, 1);
        let reordered = ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::CarrierInterference,
            values,
        )
        .unwrap();
        let wrong_type = ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::WordErrorRate,
            order(None).targets.channels().to_vec(),
        )
        .unwrap();
        for targets in [
            reordered,
            wrong_type,
            ExtendedMeasurementTargets::try_new(
                ExtendedMeasurementType::CarrierInterference,
                vec![],
            )
            .unwrap(),
        ] {
            assert_eq!(
                exchange.acknowledge(PHONE, &TrafficMessage::ExtendedMeasurementAck(targets), 11),
                Err(OrderError::MeasurementChannels)
            );
            assert_eq!(exchange.state(), awaiting);
        }
        for (band, channel) in [(Hyperband::Pcs1900, 991), (Hyperband::Cellular800, 992)] {
            let mut values = order(None).targets.channels().to_vec();
            values[0] = MeasurementChannel {
                hyperband: band,
                channel,
            };
            let targets = ExtendedMeasurementTargets::try_new(
                ExtendedMeasurementType::CarrierInterference,
                values,
            )
            .unwrap();
            assert_eq!(
                exchange.acknowledge(PHONE, &TrafficMessage::ExtendedMeasurementAck(targets), 11),
                Err(OrderError::MeasurementChannels)
            );
        }
        assert_eq!(
            exchange.acknowledge(MobileIdentity::Min34(2), &typed_ack(), 11),
            Err(OrderError::Recipient)
        );
        assert_eq!(
            exchange.acknowledge(
                PHONE,
                &TrafficMessage::MeasurementOrderAck {
                    rf_channels: vec![]
                },
                11
            ),
            Err(OrderError::NotAck)
        );
        assert_eq!(
            exchange.acknowledge(PHONE, &typed_ack(), 12).unwrap(),
            OrderState::Acknowledged
        );
        assert_eq!(
            exchange.acknowledge(PHONE, &typed_ack(), 13).unwrap(),
            OrderState::Acknowledged
        );
    }
}
#[test]
fn generic_mobile_ack_requires_advertised_support_exact_type_and_valid_reverse_fields() {
    for support in [None, Some(false), Some(true)] {
        let mut exchange = exchange(support);
        let tx = exchange.transmission().unwrap().clone();
        exchange.transmitted(&tx, 10).unwrap();
        if support == Some(true) {
            let wrong = TrafficMessage::Ack {
                ack_message_type: TrafficMessage::StopMeasurements.message_type(),
                information: Default::default(),
            };
            assert!(matches!(
                exchange.acknowledge(PHONE, &wrong, 11),
                Err(OrderError::AckType { .. })
            ));
            let invalid = TrafficMessage::Ack {
                ack_message_type: tx.message().message_type(),
                information: AcknowledgmentInformation {
                    control_channels: vec![ControlChannel {
                        hyperband: Hyperband::Cellular800,
                        channel: 991,
                        dvcc: 1,
                    }],
                    ..Default::default()
                },
            };
            assert_eq!(
                exchange.acknowledge(PHONE, &invalid, 11),
                Err(OrderError::MeasurementFields)
            );
            assert_eq!(
                exchange.acknowledge(PHONE, &generic_ack(), 12).unwrap(),
                OrderState::Acknowledged
            );
        } else {
            let awaiting = exchange.state();
            assert_eq!(
                exchange.acknowledge(PHONE, &generic_ack(), 11),
                Err(OrderError::NotAck)
            );
            assert_eq!(exchange.state(), awaiting);
        }
    }
}
#[test]
fn retries_keep_the_original_snapshot_and_deadline_with_fresh_host_tokens() {
    let mut original = order(Some(false));
    let expected = original.targets.clone();
    let mut exchange = TrafficOrderExchange::try_new_extended_measurements(
        PHONE,
        original.clone(),
        ProtocolVersion::Pv6,
        policy(),
        0,
    )
    .unwrap();
    original.targets =
        ExtendedMeasurementTargets::try_new(ExtendedMeasurementType::WordErrorRate, vec![])
            .unwrap();
    let first = exchange.transmission().unwrap().clone();
    let bytes = first.message().encode().unwrap();
    assert_eq!(exchange.extended_targets(), Some(&expected));
    assert_eq!(
        exchange.transmitted(&first, 10).unwrap(),
        OrderState::AwaitingAck {
            deadline_monotonic_ms: 110
        }
    );
    assert_eq!(
        exchange.advance(109).unwrap(),
        OrderState::AwaitingAck {
            deadline_monotonic_ms: 110
        }
    );
    assert_eq!(exchange.advance(110).unwrap(), OrderState::Prepared);
    let retry = exchange.transmission().unwrap().clone();
    assert_eq!(retry.message().encode().unwrap(), bytes);
    assert!(!retry.matches(&first));
    assert_eq!(
        exchange.transmitted(&first, 111),
        Err(OrderError::StaleTransmission)
    );
    assert_eq!(
        exchange.transmitted(&retry, 490).unwrap(),
        OrderState::AwaitingAck {
            deadline_monotonic_ms: 500
        }
    );
    assert_eq!(exchange.transmissions(), 2);
    assert_eq!(
        exchange.acknowledge(PHONE, &typed_ack(), 500),
        Err(OrderError::Closed)
    );
    assert_eq!(exchange.state(), OrderState::TimedOut);
    assert_eq!(exchange.extended_targets(), Some(&expected));
    assert!(exchange.transmission().is_none());
}
#[test]
fn response_retry_budget_cancel_and_foreign_host_callbacks_cannot_revive_closed_orders() {
    let mut exhausted = TrafficOrderExchange::try_new_extended_measurements(
        PHONE,
        order(None),
        ProtocolVersion::Pv2,
        OrderPolicy {
            max_transmissions: 1,
            ..policy()
        },
        0,
    )
    .unwrap();
    let tx = exhausted.transmission().unwrap().clone();
    exhausted.transmitted(&tx, 10).unwrap();
    assert_eq!(
        exhausted.acknowledge(PHONE, &typed_ack(), 110),
        Err(OrderError::Closed)
    );
    assert_eq!(exhausted.state(), OrderState::TimedOut);
    for transmitted in [false, true] {
        let mut old = exchange(None);
        let old_tx = old.transmission().unwrap().clone();
        if transmitted {
            old.transmitted(&old_tx, 10).unwrap();
        }
        assert_eq!(old.cancel(20).unwrap(), OrderState::Cancelled);
        assert!(old.transmission().is_none());
        assert!(old.acknowledge(PHONE, &typed_ack(), 21).is_err());
        assert_eq!(old.advance(500).unwrap(), OrderState::Cancelled);
        let mut replacement = exchange(None);
        assert_eq!(
            replacement.transmitted(&old_tx, 30),
            Err(OrderError::StaleTransmission)
        );
        assert_eq!(replacement.state(), OrderState::Prepared);
        assert_eq!(replacement.advance(29).unwrap(), OrderState::Prepared);
    }
    let mut regressed = exchange(None);
    regressed.advance(10).unwrap();
    assert_eq!(
        regressed.acknowledge(PHONE, &typed_ack(), 9),
        Err(OrderError::ClockRegression)
    );
}
