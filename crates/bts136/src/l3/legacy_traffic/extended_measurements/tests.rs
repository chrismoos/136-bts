use super::*;
use crate::l3::legacy_traffic::{TrafficMessage, decode_prefix_in};
fn vectors() -> Vec<(ExtendedChannelQualityReport, Vec<bool>)> {
    [
        (
            ExtendedChannelQualityReport::First {
                current: ExtendedCurrentQuality {
                    ber_code: 3,
                    measurement_code: 12,
                    rssi_code: 42,
                },
                neighbor_rssi_codes: [0, 1, 32, 63],
            },
            [0x14, 0xcf, 0x2a, 0x00, 0x18, 0x3f],
            48,
        ),
        (
            ExtendedChannelQualityReport::Later {
                group: NeighborGroup::Second,
                neighbor_rssi_codes: [0, 1, 15, 31, 32, 63],
            },
            [0x2a, 0x00, 0x04, 0xf7, 0xe0, 0xfc],
            46,
        ),
        (
            ExtendedChannelQualityReport::Later {
                group: NeighborGroup::Third,
                neighbor_rssi_codes: [0, 1, 15, 31, 32, 63],
            },
            [0x37, 0xc0, 0x04, 0xf7, 0xe0, 0xfc],
            46,
        ),
        (
            ExtendedChannelQualityReport::Later {
                group: NeighborGroup::Fourth,
                neighbor_rssi_codes: [0, 1, 15, 31, 32, 63],
            },
            [0x29, 0x40, 0x04, 0xf7, 0xe0, 0xfc],
            46,
        ),
    ]
    .into_iter()
    .map(|(message, bytes, length)| {
        let bits = bytes
            .into_iter()
            .flat_map(|byte| (0..8).rev().map(move |shift| byte & (1 << shift) != 0))
            .take(length)
            .collect();
        (message, bits)
    })
    .collect()
}
#[test]
fn four_report_layouts_match_independent_fixed_vectors_and_exclude_word_filler() {
    for (report, bits) in vectors() {
        let message = TrafficMessage::ExtendedChannelQuality(report);
        assert_eq!(message.encode().unwrap(), bits);
        let mut padded = bits.clone();
        padded.extend([true; 48]);
        let decoded = decode_prefix_in(&padded, Direction::Reverse).unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.consumed_bits, bits.len());
        assert!(matches!(
            decode_prefix_in(&bits, Direction::Forward),
            Err(TrafficMessageError::Field("extended_quality_direction"))
        ));
        for end in 0..bits.len() {
            assert!(
                decode_prefix_in(&bits[..end], Direction::Reverse).is_err(),
                "truncation at {end}"
            );
        }
    }
}
#[test]
fn extended_reports_require_pv2_and_keep_ambiguous_current_metric_unresolved() {
    for (report, wire) in vectors() {
        assert!(report.validate_version(ProtocolVersion::Pv1).is_err());
        assert!(report.validate_version(ProtocolVersion::Pv2).is_ok());
        assert!(
            crate::l3::legacy_traffic::decode_prefix_in_version(
                &wire,
                Direction::Reverse,
                ProtocolVersion::Pv1
            )
            .is_err()
        );
        assert_eq!(
            crate::l3::legacy_traffic::decode_prefix_in_version(
                &wire,
                Direction::Reverse,
                ProtocolVersion::Pv2
            )
            .unwrap()
            .message,
            TrafficMessage::ExtendedChannelQuality(report)
        );
    }
    let current = ExtendedCurrentQuality {
        ber_code: 3,
        measurement_code: 12,
        rssi_code: 42,
    };
    assert_eq!(
        current.metric(ExtendedMeasurementType::CarrierInterference),
        ExtendedQualityMetric::CarrierInterference(12)
    );
    assert_eq!(
        current.metric(ExtendedMeasurementType::WordErrorRate),
        ExtendedQualityMetric::WordErrorRate(12)
    );
    for ((report, _), first) in vectors().into_iter().zip([0, 4, 10, 16]) {
        assert_eq!(report.neighbors().0, first);
    }
}
#[test]
fn extended_quality_fields_reject_overflow_without_clamping() {
    for current in [
        ExtendedCurrentQuality {
            ber_code: 16,
            measurement_code: 0,
            rssi_code: 0,
        },
        ExtendedCurrentQuality {
            ber_code: 0,
            measurement_code: 16,
            rssi_code: 0,
        },
        ExtendedCurrentQuality {
            ber_code: 0,
            measurement_code: 0,
            rssi_code: 64,
        },
    ] {
        assert!(
            TrafficMessage::ExtendedChannelQuality(ExtendedChannelQualityReport::First {
                current,
                neighbor_rssi_codes: [0; 4]
            })
            .encode()
            .is_err()
        );
    }
    for (mut report, _) in vectors() {
        match &mut report {
            ExtendedChannelQualityReport::First {
                current,
                neighbor_rssi_codes,
            } => {
                *current = ExtendedCurrentQuality {
                    ber_code: 15,
                    measurement_code: 15,
                    rssi_code: 63,
                };
                *neighbor_rssi_codes = [63; 4];
            }
            ExtendedChannelQualityReport::Later {
                neighbor_rssi_codes,
                ..
            } => *neighbor_rssi_codes = [63; 6],
        }
        assert!(
            TrafficMessage::ExtendedChannelQuality(report.clone())
                .encode()
                .is_ok()
        );
        match &mut report {
            ExtendedChannelQualityReport::First {
                neighbor_rssi_codes,
                ..
            } => neighbor_rssi_codes[0] = 64,
            ExtendedChannelQualityReport::Later {
                neighbor_rssi_codes,
                ..
            } => neighbor_rssi_codes[0] = 64,
        }
        assert!(
            TrafficMessage::ExtendedChannelQuality(report)
                .encode()
                .is_err()
        );
    }
}
