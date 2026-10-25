use super::*;
use crate::l3::legacy_traffic::{
    ExtendedCurrentQuality, NeighborGroup, decode_prefix_in, decode_prefix_in_version,
};
fn channels() -> Vec<MeasurementChannel> {
    vec![
        MeasurementChannel {
            hyperband: Hyperband::Cellular800,
            channel: 991,
        },
        MeasurementChannel {
            hyperband: Hyperband::Pcs1900,
            channel: 42,
        },
        MeasurementChannel {
            hyperband: Hyperband::Imt2000,
            channel: 2047,
        },
    ]
}
fn targets(kind: ExtendedMeasurementType) -> ExtendedMeasurementTargets {
    ExtendedMeasurementTargets::try_new(kind, channels()).unwrap()
}
fn bits(bytes: &[u8], length: usize) -> Vec<bool> {
    bytes
        .iter()
        .flat_map(|byte| {
            (0..u8::BITS)
                .rev()
                .map(move |shift| byte & (1 << shift) != 0)
        })
        .take(length)
        .collect()
}
#[test]
fn extended_order_and_ack_match_complete_independent_vectors_and_keep_pv6_zero_bit() {
    for (kind, support, version, direction, bytes, length) in [
        (
            ExtendedMeasurementType::CarrierInterference,
            None,
            ProtocolVersion::Pv2,
            Direction::Forward,
            [0x36, 0x48, 0xc7, 0xbe, 0x82, 0xaf, 0xff, 0x80],
            57,
        ),
        (
            ExtendedMeasurementType::CarrierInterference,
            Some(false),
            ProtocolVersion::Pv6,
            Direction::Forward,
            [0x36, 0x48, 0xc7, 0xbe, 0x82, 0xaf, 0xff, 0x80],
            58,
        ),
        (
            ExtendedMeasurementType::CarrierInterference,
            Some(true),
            ProtocolVersion::Pv6,
            Direction::Forward,
            [0x36, 0x48, 0xc7, 0xbe, 0x82, 0xaf, 0xff, 0xc0],
            58,
        ),
        (
            ExtendedMeasurementType::WordErrorRate,
            None,
            ProtocolVersion::Pv2,
            Direction::Forward,
            [0x36, 0x50, 0xc7, 0xbe, 0x82, 0xaf, 0xff, 0x80],
            57,
        ),
        (
            ExtendedMeasurementType::CarrierInterference,
            None,
            ProtocolVersion::Pv6,
            Direction::Reverse,
            [0x35, 0x08, 0xc7, 0xbe, 0x82, 0xaf, 0xff, 0x80],
            57,
        ),
        (
            ExtendedMeasurementType::WordErrorRate,
            None,
            ProtocolVersion::Pv2,
            Direction::Reverse,
            [0x35, 0x10, 0xc7, 0xbe, 0x82, 0xaf, 0xff, 0x80],
            57,
        ),
    ] {
        let message = if direction == Direction::Forward {
            let order = ExtendedMeasurementOrder {
                targets: targets(kind),
                mobile_ack_support: support,
            };
            assert_eq!(
                order.encode_for_version(version).unwrap(),
                bits(&bytes, length)
            );
            TrafficMessage::ExtendedMeasurementOrder(order)
        } else {
            TrafficMessage::ExtendedMeasurementAck(targets(kind))
        };
        let wire = bits(&bytes, length);
        assert_eq!(message.encode().unwrap(), wire);
        let mut padded = wire.clone();
        padded.extend([true; 48]);
        let decoded = decode_prefix_in_version(&padded, direction, version).unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.consumed_bits, length);
        for end in 0..length {
            assert!(
                decode_prefix_in_version(&wire[..end], direction, version).is_err(),
                "{direction:?}/{version:?}: truncated at {end}"
            );
        }
        let wrong_direction = if direction == Direction::Forward {
            Direction::Reverse
        } else {
            Direction::Forward
        };
        assert!(decode_prefix_in_version(&wire, wrong_direction, version).is_err());
        assert!(decode_prefix_in_version(&wire, direction, ProtocolVersion::Pv1).is_err());
        if direction == Direction::Forward {
            assert!(matches!(
                decode_prefix_in(&padded, direction),
                Err(TrafficMessageError::Field(
                    "extended_order_version_required"
                ))
            ));
        } else {
            assert_eq!(
                decode_prefix_in(&padded, direction).unwrap().message,
                message
            );
        }
    }
}
#[test]
fn extended_order_version_contract_requires_exact_ack_support_presence() {
    for version in [
        ProtocolVersion::Pv1,
        ProtocolVersion::Pv2,
        ProtocolVersion::Pv5,
        ProtocolVersion::Pv6,
        ProtocolVersion::Pv7,
    ] {
        for support in [None, Some(false), Some(true)] {
            let order = ExtendedMeasurementOrder {
                targets: targets(ExtendedMeasurementType::WordErrorRate),
                mobile_ack_support: support,
            };
            let expected = version != ProtocolVersion::Pv1
                && support.is_some()
                    == matches!(version, ProtocolVersion::Pv6 | ProtocolVersion::Pv7);
            assert_eq!(order.encode_for_version(version).is_ok(), expected);
        }
    }
}
#[test]
fn extended_order_counts_reserved_types_and_channel_widths_are_bounded() {
    for count in [0, MAX_EXTENDED_MEASUREMENT_CHANNELS] {
        let values = vec![
            MeasurementChannel {
                hyperband: Hyperband::Cellular700,
                channel: 2047
            };
            count
        ];
        let target = ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::CarrierInterference,
            values,
        )
        .unwrap();
        let ack = TrafficMessage::ExtendedMeasurementAck(target);
        assert_eq!(
            decode_prefix_in_version(
                &ack.encode().unwrap(),
                Direction::Reverse,
                ProtocolVersion::Pv6
            )
            .unwrap()
            .message,
            ack
        );
    }
    assert!(
        ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::WordErrorRate,
            vec![channels()[0]; MAX_EXTENDED_MEASUREMENT_CHANNELS + 1]
        )
        .is_err()
    );
    assert!(
        ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::WordErrorRate,
            vec![MeasurementChannel {
                hyperband: Hyperband::Cellular800,
                channel: 2048
            }]
        )
        .is_err()
    );
    for measurement in [0, 3, 4, 5, 6, 7] {
        let mut wire = vec![];
        put(&mut wire, 0, 2);
        put(&mut wire, u64::from(ACK), 8);
        put(&mut wire, measurement, MEASUREMENT_BITS);
        put(&mut wire, 0, COUNT_BITS);
        assert!(matches!(
            decode_prefix_in(&wire, Direction::Reverse),
            Err(TrafficMessageError::Field("extended_measurement_type"))
        ));
    }
    for count in 23..32 {
        let mut wire = vec![];
        put(&mut wire, 0, 2);
        put(&mut wire, u64::from(ACK), 8);
        put(&mut wire, 1, MEASUREMENT_BITS);
        put(&mut wire, count, COUNT_BITS);
        assert!(matches!(
            decode_prefix_in(&wire, Direction::Reverse),
            Err(TrafficMessageError::Field("extended_measurement_count"))
        ));
    }
}
#[test]
fn report_association_uses_owned_order_and_checks_unused_entries() {
    let mut values = channels();
    let cir = ExtendedMeasurementTargets::try_new(
        ExtendedMeasurementType::CarrierInterference,
        values.clone(),
    )
    .unwrap();
    values[0].channel = 1;
    let report = ExtendedChannelQualityReport::First {
        current: ExtendedCurrentQuality {
            ber_code: 3,
            measurement_code: 12,
            rssi_code: 42,
        },
        neighbor_rssi_codes: [1, 32, 63, 0],
    };
    let resolved = cir.resolve(&report).unwrap();
    assert_eq!(
        resolved.serving.unwrap().metric,
        ExtendedQualityMetric::CarrierInterference(12)
    );
    assert_eq!(
        resolved
            .neighbors
            .iter()
            .map(|value| value.channel)
            .collect::<Vec<_>>(),
        channels()
    );
    assert_eq!(
        resolved
            .neighbors
            .iter()
            .map(|value| value.rssi_code)
            .collect::<Vec<_>>(),
        [1, 32, 63]
    );
    let wer = targets(ExtendedMeasurementType::WordErrorRate);
    assert_eq!(
        wer.resolve(&report).unwrap().serving.unwrap().metric,
        ExtendedQualityMetric::WordErrorRate(12)
    );
    let mut invalid = report;
    if let ExtendedChannelQualityReport::First {
        neighbor_rssi_codes,
        ..
    } = &mut invalid
    {
        neighbor_rssi_codes[3] = 1;
    }
    assert!(cir.resolve(&invalid).is_err());
    for (group, start) in [
        (NeighborGroup::Second, 4),
        (NeighborGroup::Third, 10),
        (NeighborGroup::Fourth, 16),
    ] {
        let target = ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::WordErrorRate,
            (0..MAX_EXTENDED_MEASUREMENT_CHANNELS)
                .map(|channel| MeasurementChannel {
                    hyperband: Hyperband::Pcs1900,
                    channel: channel as u16,
                })
                .collect(),
        )
        .unwrap();
        let report = ExtendedChannelQualityReport::Later {
            group,
            neighbor_rssi_codes: [1, 2, 3, 4, 5, 6],
        };
        let values = target.resolve(&report).unwrap();
        assert!(values.serving.is_none());
        assert_eq!(usize::from(values.neighbors[0].channel.channel), start);
        assert_eq!(values.neighbors.len(), 6);
        assert!(cir.resolve(&report).is_err());
        assert!(
            cir.resolve(&ExtendedChannelQualityReport::Later {
                group,
                neighbor_rssi_codes: [0; 6]
            })
            .unwrap()
            .neighbors
            .is_empty()
        );
    }
}
