use super::*;
use crate::{
    l3::legacy_traffic::decode_prefix_in_version,
    link::traffic::{self, TrafficMessageReceiver},
};
fn parameters(command: ActiveAmrMode) -> AmrNoiseMessage {
    AmrNoiseMessage::Parameters {
        command,
        indication: ActiveAmrMode::Mode3,
        parameters: AmrNoiseParameters {
            lsf_reference_vector: 5,
            lsf_subvectors: [0xa6, 0x123, 0x1ab],
            frame_energy: 0x2d,
        },
    }
}
fn bits(bytes: &[u8], count: usize) -> Vec<bool> {
    bytes
        .iter()
        .flat_map(|byte| (0..u8::BITS).rev().map(move |bit| byte & (1 << bit) != 0))
        .take(count)
        .collect()
}
#[test]
fn all_four_forward_commands_match_fixed_47_bit_vectors() {
    for (command, bytes) in [
        (ActiveAmrMode::Mode1, [0x2a, 0xab, 0x4d, 0x23, 0xd5, 0xda]),
        (ActiveAmrMode::Mode2, [0x2a, 0xeb, 0x4d, 0x23, 0xd5, 0xda]),
        (ActiveAmrMode::Mode3, [0x2b, 0x6b, 0x4d, 0x23, 0xd5, 0xda]),
        (ActiveAmrMode::Mode4, [0x2b, 0xab, 0x4d, 0x23, 0xd5, 0xda]),
    ] {
        let expected = bits(&bytes, 47);
        let message = parameters(command);
        assert_eq!(
            message
                .encode_for_version(ProtocolVersion::Pv5, Direction::Forward)
                .unwrap(),
            expected
        );
        let decoded =
            decode_prefix_in_version(&bits(&bytes, 48), Direction::Forward, ProtocolVersion::Pv5)
                .unwrap();
        assert_eq!(decoded.message, TrafficMessage::AmrNoise(message));
        assert_eq!(decoded.consumed_bits, 47);
        for length in 0..expected.len() {
            assert!(matches!(
                decode_prefix_in_version(
                    &expected[..length],
                    Direction::Forward,
                    ProtocolVersion::Pv5
                ),
                Err(TrafficMessageError::Truncated { .. })
            ));
        }
    }
}
#[test]
fn implicit_noise_preserves_two_bits_in_either_direction_without_guessing_phase() {
    for (mode, suffix) in [
        (ActiveAmrMode::Mode1, 0),
        (ActiveAmrMode::Mode2, 0x10),
        (ActiveAmrMode::Mode3, 0x20),
        (ActiveAmrMode::Mode4, 0x30),
    ] {
        let expected = bits(&[0x34, suffix], 12);
        let message = AmrNoiseMessage::Implicit { data: mode };
        for direction in [Direction::Forward, Direction::Reverse] {
            assert_eq!(
                message
                    .encode_for_version(ProtocolVersion::Pv5, direction)
                    .unwrap(),
                expected
            );
            let decoded =
                decode_prefix_in_version(&expected, direction, ProtocolVersion::Pv5).unwrap();
            assert_eq!(decoded.message, TrafficMessage::AmrNoise(message));
            assert_eq!(decoded.consumed_bits, 12);
            for length in 0..expected.len() {
                assert!(matches!(
                    decode_prefix_in_version(&expected[..length], direction, ProtocolVersion::Pv5),
                    Err(TrafficMessageError::Truncated { .. })
                ));
            }
            assert!(decode_prefix_in_version(&expected, direction, ProtocolVersion::Pv2).is_err());
        }
    }
}
#[test]
fn version_direction_and_field_boundaries_are_enforced() {
    let mut message = parameters(ActiveAmrMode::Mode1);
    assert!(
        message
            .encode_for_version(ProtocolVersion::Pv5, Direction::Reverse)
            .is_err()
    );
    let wire = message
        .encode_for_version(ProtocolVersion::Pv5, Direction::Forward)
        .unwrap();
    assert!(decode_prefix_in_version(&wire, Direction::Reverse, ProtocolVersion::Pv5).is_err());
    for version in [
        ProtocolVersion::Is136RevA,
        ProtocolVersion::Pv1,
        ProtocolVersion::Pv2,
        ProtocolVersion::Pv3,
        ProtocolVersion::Pv4,
    ] {
        assert!(
            message
                .encode_for_version(version, Direction::Forward)
                .is_err()
        );
        assert!(decode_prefix_in_version(&wire, Direction::Forward, version).is_err());
    }
    for version in [
        ProtocolVersion::Pv5,
        ProtocolVersion::Pv6,
        ProtocolVersion::Pv7,
    ] {
        assert!(
            message
                .encode_for_version(version, Direction::Forward)
                .is_ok()
        );
    }
    for (reference, subvectors, energy) in [
        (8, [0, 0, 0], 0),
        (0, [256, 0, 0], 0),
        (0, [0, 512, 0], 0),
        (0, [0, 0, 512], 0),
        (0, [0, 0, 0], 64),
    ] {
        if let AmrNoiseMessage::Parameters { parameters, .. } = &mut message {
            *parameters = AmrNoiseParameters {
                lsf_reference_vector: reference,
                lsf_subvectors: subvectors,
                frame_energy: energy,
            };
        }
        assert!(
            message
                .encode_for_version(ProtocolVersion::Pv5, Direction::Forward)
                .is_err()
        );
    }
    let maximum = AmrNoiseMessage::Parameters {
        command: ActiveAmrMode::Mode4,
        indication: ActiveAmrMode::Mode4,
        parameters: AmrNoiseParameters {
            lsf_reference_vector: 7,
            lsf_subvectors: [255, 511, 511],
            frame_energy: 63,
        },
    };
    let wire = maximum
        .encode_for_version(ProtocolVersion::Pv5, Direction::Forward)
        .unwrap();
    assert_eq!(
        decode_prefix_in_version(&wire, Direction::Forward, ProtocolVersion::Pv5)
            .unwrap()
            .message,
        TrafficMessage::AmrNoise(maximum)
    );
    assert!(ActiveAmrMode::try_from(4).is_err());
}
#[test]
fn single_word_assembly_preserves_filler_boundaries_and_rejects_unsupported_pv2() {
    for message in [
        parameters(ActiveAmrMode::Mode3),
        AmrNoiseMessage::Implicit {
            data: ActiveAmrMode::Mode2,
        },
    ] {
        let words = traffic::encode(&TrafficMessage::AmrNoise(message)).unwrap();
        assert_eq!(words.len(), 1);
        let mut receiver =
            TrafficMessageReceiver::with_version(Direction::Forward, ProtocolVersion::Pv5);
        assert_eq!(
            receiver.push(&words[0]).unwrap(),
            Some(TrafficMessage::AmrNoise(message))
        );
        let mut older =
            TrafficMessageReceiver::with_version(Direction::Forward, ProtocolVersion::Pv2);
        assert!(older.push(&words[0]).is_err());
    }
}
