use super::*;
use crate::{
    l3::legacy_traffic::decode_prefix_in_version,
    link::traffic::{self, TrafficMessageReceiver},
};
fn configuration() -> AmrConfiguration {
    AmrConfiguration {
        countdown: true,
        link: AmrLink::Both,
        initial_mode: ActiveAmrMode::Mode2,
        active_set: AmrActiveSet::try_new(0x33).unwrap(),
        thresholds: [12, 23, 34],
        common_hysteresis: 5,
    }
}
fn vectors() -> Vec<(AmrControlMessage, Vec<bool>)> {
    let entries: [(AmrControlMessage, &[u8], usize); 3] = [
        (
            AmrControlMessage::Configuration(configuration()),
            &[0x34, 0x7a, 0x66, 0x62, 0xf1, 0x28],
            45,
        ),
        (
            AmrControlMessage::Thresholds(AmrThresholds {
                lower: [4, 17, 50],
                hysteresis: [3, 9, 12],
            }),
            &[0x34, 0x84, 0x47, 0x23, 0x9c],
            40,
        ),
        (
            AmrControlMessage::PhaseFreeze {
                phase: AmrPhase::Maintain,
                freeze: AmrFreeze::Freeze,
                mode: ActiveAmrMode::Mode3,
            },
            &[0x2a, 0x66],
            16,
        ),
    ];
    entries
        .into_iter()
        .map(|(message, bytes, length)| (message, bits(bytes, length)))
        .collect()
}
fn bits(bytes: &[u8], length: usize) -> Vec<bool> {
    bytes
        .iter()
        .flat_map(|byte| (0..u8::BITS).rev().map(move |bit| byte & (1 << bit) != 0))
        .take(length)
        .collect()
}
#[test]
fn control_orders_match_independent_vectors_and_reject_every_truncation() {
    for (message, wire) in vectors() {
        assert_eq!(
            message.encode_for_version(ProtocolVersion::Pv5).unwrap(),
            wire
        );
        let decoded =
            decode_prefix_in_version(&wire, Direction::Forward, ProtocolVersion::Pv5).unwrap();
        assert_eq!(decoded.message, TrafficMessage::AmrControl(message));
        assert_eq!(decoded.consumed_bits, wire.len());
        for length in 0..wire.len() {
            assert!(matches!(
                decode_prefix_in_version(&wire[..length], Direction::Forward, ProtocolVersion::Pv5),
                Err(TrafficMessageError::Truncated { .. })
            ));
        }
    }
}
#[test]
fn older_versions_reverse_direction_and_reserved_phase_codes_are_rejected() {
    for (message, wire) in vectors() {
        assert!(decode_prefix_in_version(&wire, Direction::Reverse, ProtocolVersion::Pv5).is_err());
        for version in [
            ProtocolVersion::Is136RevA,
            ProtocolVersion::Pv2,
            ProtocolVersion::Pv4,
        ] {
            assert!(message.encode_for_version(version).is_err());
            assert!(decode_prefix_in_version(&wire, Direction::Forward, version).is_err());
        }
        for version in [
            ProtocolVersion::Pv5,
            ProtocolVersion::Pv6,
            ProtocolVersion::Pv7,
        ] {
            assert!(message.encode_for_version(version).is_ok());
        }
    }
    for bytes in [[0x2a, 0x76], [0x2a, 0x6e]] {
        assert!(
            decode_prefix_in_version(&bits(&bytes, 16), Direction::Forward, ProtocolVersion::Pv5)
                .is_err()
        );
    }
}
#[test]
fn active_sets_are_nonempty_and_bounded_and_initial_mode_is_a_position() {
    for bitmap in u8::MIN..=u8::MAX {
        let set = AmrActiveSet::try_new(bitmap);
        assert_eq!(set.is_ok(), (1..=4).contains(&bitmap.count_ones()));
        if let Ok(set) = set {
            assert_eq!(set.bitmap(), bitmap);
        }
    }
    let mut value = configuration();
    value.active_set = AmrActiveSet::try_new(0x80).unwrap();
    assert!(!value.active_set.contains_position(ActiveAmrMode::Mode2));
    assert!(
        AmrControlMessage::Configuration(value)
            .encode_for_version(ProtocolVersion::Pv5)
            .is_err()
    );
    value.initial_mode = ActiveAmrMode::Mode1;
    value.thresholds = [63, 0, 1];
    value.link = AmrLink::Neither;
    assert!(
        AmrControlMessage::Configuration(value)
            .encode_for_version(ProtocolVersion::Pv5)
            .is_ok()
    );
    const ACTIVE_SET_START_BIT: usize = 15;
    const ACTIVE_SET_BITS: usize = 8;
    let mut wire = vectors().remove(0).1;
    wire[ACTIVE_SET_START_BIT..ACTIVE_SET_START_BIT + ACTIVE_SET_BITS].fill(false);
    assert!(decode_prefix_in_version(&wire, Direction::Forward, ProtocolVersion::Pv5).is_err());
}
#[test]
fn threshold_order_uses_active_pairs_and_preserves_unused_fields() {
    let set = AmrActiveSet::try_new(0x33).unwrap();
    let bad_lower = AmrThresholds {
        lower: [12, 11, 30],
        hysteresis: [0; 3],
    };
    assert!(bad_lower.validate_for_active_set(set).is_err());
    let bad_upper = AmrThresholds {
        lower: [1, 2, 3],
        hysteresis: [15, 0, 0],
    };
    assert!(bad_upper.validate_for_active_set(set).is_err());
    assert!(
        AmrControlMessage::Thresholds(bad_upper)
            .encode_for_version(ProtocolVersion::Pv5)
            .is_ok()
    );
    let two = AmrActiveSet::try_new(0x11).unwrap();
    assert!(bad_lower.validate_for_active_set(two).is_ok());
    assert!(bad_upper.validate_for_active_set(two).is_ok());
    let one = AmrActiveSet::try_new(0x10).unwrap();
    assert!(
        AmrThresholds {
            lower: [0; 3],
            hysteresis: [0; 3]
        }
        .validate_for_active_set(one)
        .is_ok()
    );
    for phase in [
        AmrPhase::DefaultOdd,
        AmrPhase::OppositeEven,
        AmrPhase::Maintain,
    ] {
        for freeze in [AmrFreeze::Unfreeze, AmrFreeze::Freeze, AmrFreeze::Maintain] {
            let message = AmrControlMessage::PhaseFreeze {
                phase,
                freeze,
                mode: ActiveAmrMode::Mode4,
            };
            let wire = message.encode_for_version(ProtocolVersion::Pv5).unwrap();
            assert_eq!(
                decode_prefix_in_version(&wire, Direction::Forward, ProtocolVersion::Pv5)
                    .unwrap()
                    .message,
                TrafficMessage::AmrControl(message)
            );
        }
    }
}
#[test]
fn threshold_and_hysteresis_field_overflow_is_not_truncated() {
    for thresholds in [
        AmrThresholds {
            lower: [0, 64, 0],
            hysteresis: [0; 3],
        },
        AmrThresholds {
            lower: [0; 3],
            hysteresis: [0, 0, 16],
        },
    ] {
        assert!(
            AmrControlMessage::Thresholds(thresholds)
                .encode_for_version(ProtocolVersion::Pv5)
                .is_err()
        );
    }
    for value in [
        AmrConfiguration {
            thresholds: [12, 23, 64],
            ..configuration()
        },
        AmrConfiguration {
            common_hysteresis: 16,
            ..configuration()
        },
        AmrConfiguration {
            thresholds: [23, 12, 34],
            ..configuration()
        },
    ] {
        assert!(
            AmrControlMessage::Configuration(value)
                .encode_for_version(ProtocolVersion::Pv5)
                .is_err()
        );
    }
}
#[test]
fn traffic_assembly_consumes_one_word_without_filler_or_length_confusion() {
    for (message, _) in vectors() {
        let words = traffic::encode(&TrafficMessage::AmrControl(message)).unwrap();
        assert_eq!(words.len(), 1);
        let mut receiver =
            TrafficMessageReceiver::with_version(Direction::Forward, ProtocolVersion::Pv5);
        assert_eq!(
            receiver.push(&words[0]).unwrap(),
            Some(TrafficMessage::AmrControl(message))
        );
    }
}

#[test]
fn all_valid_active_sets_map_positions_in_increasing_bitrate_order() {
    let modes = [
        AmrCodecMode::Mr475,
        AmrCodecMode::Mr515,
        AmrCodecMode::Mr59,
        AmrCodecMode::Mr67,
        AmrCodecMode::Mr74,
        AmrCodecMode::Mr795,
        AmrCodecMode::Mr102,
        AmrCodecMode::Mr122,
    ];
    for bitmap in 0..=u8::MAX {
        let selected: Vec<_> = modes
            .into_iter()
            .enumerate()
            .filter(|(bit, _)| bitmap & (1 << bit) != 0)
            .map(|(_, mode)| mode)
            .collect();
        let Ok(active) = AmrActiveSet::try_new(bitmap) else {
            assert!(selected.is_empty() || selected.len() > 4);
            continue;
        };
        for (index, position) in [
            ActiveAmrMode::Mode1,
            ActiveAmrMode::Mode2,
            ActiveAmrMode::Mode3,
            ActiveAmrMode::Mode4,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(active.codec_mode(position), selected.get(index).copied());
        }
    }
}
