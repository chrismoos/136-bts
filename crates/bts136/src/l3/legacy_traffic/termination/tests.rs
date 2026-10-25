use super::*;
use crate::l3::legacy_traffic::{self, TrafficMessage};
fn channel() -> ControlChannel {
    ControlChannel {
        hyperband: Hyperband::Cellular800,
        channel: 991,
        dvcc: 42,
    }
}
fn wire_bits(bytes: &[u8]) -> Vec<bool> {
    bytes
        .iter()
        .flat_map(|byte| {
            (0..u8::BITS)
                .rev()
                .map(move |shift| byte & (1 << shift) != 0)
        })
        .collect()
}
#[test]
fn release_and_acknowledgment_match_independent_complete_wire_vectors() {
    let mut ack = AcknowledgmentInformation {
        control_channels: vec![channel()],
        ..Default::default()
    };
    let cases = vec![
        (
            TrafficMessage::Release {
                information: Default::default(),
            },
            vec![0x36, 0x80],
            Direction::Reverse,
        ),
        (
            TrafficMessage::Release {
                information: ReleaseInformation {
                    fields: ReleaseFields::Reason(ReleaseReason::PowerDown),
                    ..Default::default()
                },
            },
            vec![0x36, 0x82, 0x10, 0x7c],
            Direction::Reverse,
        ),
        (
            TrafficMessage::Release {
                information: ReleaseInformation {
                    fields: ReleaseFields::ControlChannels(vec![channel()]),
                    ..Default::default()
                },
            },
            vec![0x36, 0x84, 0x10, 0x47, 0xbe, 0x54],
            Direction::Forward,
        ),
        (
            TrafficMessage::Ack {
                ack_message_type: 0xda,
                information: ack.clone(),
            },
            vec![0x15, 0x36, 0x84, 0x20, 0x47, 0xbe, 0x54],
            Direction::Forward,
        ),
    ];
    for (message, wire, direction) in cases {
        assert_eq!(message.encode().unwrap(), wire_bits(&wire));
        assert_eq!(
            legacy_traffic::decode_prefix_in(&wire_bits(&wire), direction)
                .unwrap()
                .message,
            message
        );
        for length in 0..wire.len() {
            assert!(
                legacy_traffic::decode_prefix_in(&wire_bits(&wire[..length]), direction).is_err()
            );
        }
    }
    ack.last_decoded_parameter = Some(7);
    let message = TrafficMessage::Ack {
        ack_message_type: 0xda,
        information: ack,
    };
    let wire = [0x15, 0x36, 0x86, 0x10, 0x5c, 0x81, 0x1e, 0xf9, 0x50];
    assert_eq!(message.encode().unwrap(), wire_bits(&wire));
    assert_eq!(
        legacy_traffic::decode_prefix_in(&wire_bits(&wire), Direction::Forward)
            .unwrap()
            .message,
        message
    );
}
#[test]
fn reverse_release_reasons_and_single_value_counts_reject_all_reserved_values() {
    for reason in 0..16 {
        let wire = [0x36, 0x82, 0x10, 0x40 | (reason << 2)];
        assert_eq!(
            legacy_traffic::decode_prefix_in(&wire_bits(&wire), Direction::Reverse).is_ok(),
            reason == 0 || reason == 15
        );
    }
    for count in 0..64 {
        let bytes = [0x10 | (count >> 2), (count & 3) << 6];
        assert_eq!(
            decode_release(&bytes, Direction::Reverse).is_ok(),
            count == 1
        );
        assert_eq!(decode_ack(&bytes, Direction::Reverse).is_ok(), count == 1);
    }
    for parameter in 0..16 {
        let bytes = [0x10, 0x40 | (parameter << 2)];
        assert_eq!(
            decode_ack(&bytes, Direction::Reverse)
                .unwrap()
                .last_decoded_parameter,
            Some(parameter)
        );
    }
    let ack = AcknowledgmentInformation {
        last_decoded_parameter: Some(16),
        ..Default::default()
    };
    assert!(ack.encode_octets().is_err());
}
#[test]
fn one_to_three_control_channels_preserve_every_hyperband_and_maximum_field_widths() {
    for band in 0..4 {
        for count in 1..=3 {
            let channels = vec![
                ControlChannel {
                    hyperband: Hyperband::try_from(band).unwrap(),
                    channel: 2047,
                    dvcc: 255
                };
                count
            ];
            let information = ReleaseInformation {
                fields: ReleaseFields::ControlChannels(channels.clone()),
                ..Default::default()
            };
            let bytes = information.encode_octets().unwrap();
            assert_eq!(
                decode_release(&bytes, Direction::Forward).unwrap(),
                information
            );
            information.validate_in(Direction::Forward).unwrap();
            assert!(information.validate_in(Direction::Reverse).is_err());
            let ack = AcknowledgmentInformation {
                control_channels: channels,
                last_decoded_parameter: Some(15),
                ..Default::default()
            };
            assert_eq!(
                decode_ack(&ack.encode_octets().unwrap(), Direction::Forward).unwrap(),
                ack
            );
            assert!(ack.validate_in(Direction::Reverse).is_err());
        }
    }
    for count in [0, 4] {
        assert!(
            ReleaseInformation {
                fields: ReleaseFields::ControlChannels(vec![channel(); count]),
                ..Default::default()
            }
            .encode_octets()
            .is_err()
        );
    }
    assert!(Hyperband::try_from(4).is_err());
    let mut bad_channel = channel();
    bad_channel.channel = 2048;
    assert!(bad_channel.validate().is_err());
    let reason = ReleaseInformation {
        fields: ReleaseFields::Reason(ReleaseReason::Normal),
        ..Default::default()
    };
    assert!(reason.validate_in(Direction::Forward).is_err());
}
#[test]
fn duplicate_order_malformed_completed_elements_and_nonzero_padding_are_rejected() {
    for bytes in [vec![0x10], vec![0x10, 0x40, 0x41, 0], vec![0x10, 0x41]] {
        assert!(
            decode_release(&bytes, Direction::Reverse).is_err(),
            "{bytes:02X?}"
        );
    }
    let mut body = Vec::new();
    write_channels(&mut body, ACK_CONTROL_CHANNELS, &[channel()]).unwrap();
    nibble(&mut body, FIRST_PARAMETER, 7).unwrap();
    assert!(decode_ack(&octets(body).unwrap(), Direction::Forward).is_err());
    assert_eq!(
        legacy_traffic::decode_prefix_in(&wire_bits(&[0x36, 0x81, 0x10]), Direction::Reverse),
        Err(TrafficMessageError::Field("release_element_length"))
    );
    assert_eq!(
        legacy_traffic::decode_prefix_in(&wire_bits(&[0x15, 0x36, 0x81, 0x10]), Direction::Forward),
        Err(TrafficMessageError::Field("ack_element_length"))
    );
    assert!(decode_release(&[0x10, 0x47, 0xbe, 0x55], Direction::Forward).is_err());
    for count in [0, 4, 63] {
        let bytes = [0x10 | (count >> 2), (count & 3) << 6];
        assert!(decode_release(&bytes, Direction::Forward).is_err());
    }
}
#[test]
fn unknown_optional_suffixes_are_retained_without_inventing_direction_specific_fields() {
    let bytes = [0xa5; 63];
    for direction in [Direction::Forward, Direction::Reverse] {
        let release = decode_release(&bytes, direction).unwrap();
        assert_eq!(release.fields, ReleaseFields::None);
        assert_eq!(release.encode_octets().unwrap(), bytes);
        let ack = decode_ack(&bytes, direction).unwrap();
        assert!(ack.control_channels.is_empty());
        assert_eq!(ack.encode_octets().unwrap(), bytes);
    }
    assert_eq!(
        octets(vec![true; 64 * 8]),
        Err(TrafficMessageError::OptionalLength)
    );
}
