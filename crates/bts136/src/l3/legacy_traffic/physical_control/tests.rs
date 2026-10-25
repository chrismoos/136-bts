use super::*;
use crate::link::traffic::{self, TrafficMessageReceiver};

fn literal(value: &str) -> Vec<bool> {
    value
        .chars()
        .filter(|value| !value.is_whitespace())
        .map(|value| match value {
            '0' => false,
            '1' => true,
            _ => panic!("nonbinary vector"),
        })
        .collect()
}

fn request() -> PhysicalControlRequest {
    PhysicalControlRequest {
        power_level: Some(7),
        time_alignment: Some(TimeAlignment::Maintain),
        dtx_allowed: Some(false),
        delay_compensation: Some(false),
    }
}

#[test]
fn physical_control_and_resulting_ack_match_fixed_wire_fields() {
    let control = request();
    let mut actual = control;
    actual.time_alignment = Some(TimeAlignment::OffsetUnits(0));
    let cases = [
        (
            Direction::Forward,
            TrafficMessage::PhysicalControl(PhysicalControlFields {
                control,
                ..Default::default()
            }),
            literal(
                "00 11110010 000111 0010 000001 0111 0011 000001 11111 0100 000001 0 0101 000001 0 00000",
            ),
        ),
        (
            Direction::Reverse,
            TrafficMessage::PhysicalControlAck(PhysicalControlFields {
                control: actual,
                ..Default::default()
            }),
            literal(
                "00 10010001 000111 0010 000001 0111 0011 000001 00000 0100 000001 0 0101 000001 0 00000",
            ),
        ),
    ];
    for (direction, message, expected) in cases {
        assert_eq!(message.encode().unwrap(), expected);
        assert_eq!(
            decode_prefix_in(&expected, direction).unwrap().message,
            message
        );
        for end in 0..expected.len() {
            assert!(matches!(
                decode_prefix_in(&expected[..end], direction),
                Err(TrafficMessageError::Truncated { .. })
            ));
        }
        let mut receiver = TrafficMessageReceiver::new(direction);
        let decoded: Vec<_> = traffic::encode(&message)
            .unwrap()
            .iter()
            .filter_map(|word| receiver.push(word).unwrap())
            .collect();
        assert_eq!(decoded, [message]);
    }
}

#[test]
fn later_control_extensions_match_fixed_counted_vectors() {
    let message = TrafficMessage::PhysicalControl(PhysicalControlFields {
        dtx_mode: Some(37),
        downlink_power_control: Some(7),
        fast_power_control: Some(2),
        ..Default::default()
    });
    let expected = literal(
        "00 11110010 000111 0110 000001 100101 0111 0001 000001 111 0111 0010 000001 10 0000000",
    );
    assert_eq!(message.encode().unwrap(), expected);
    assert_eq!(
        decode_prefix_in(&expected, Direction::Forward)
            .unwrap()
            .message,
        message
    );
}

#[test]
fn ack_requires_all_requested_fields_and_an_absolute_result() {
    let request = request();
    let mut actual = PhysicalControlFields {
        control: request,
        ..Default::default()
    };
    assert!(!request.answered_by(&actual));
    assert!(
        TrafficMessage::PhysicalControlAck(actual.clone())
            .encode()
            .is_err()
    );
    actual.control.time_alignment = Some(TimeAlignment::OffsetUnits(30));
    actual.control.power_level = Some(10);
    assert!(request.answered_by(&actual));
    actual.control.delay_compensation = None;
    assert!(!request.answered_by(&actual));
    actual.control.delay_compensation = Some(true);
    assert!(!request.answered_by(&actual));
    let request = PhysicalControlRequest {
        time_alignment: Some(TimeAlignment::OffsetUnits(4)),
        ..Default::default()
    };
    assert!(!request.answered_by(&actual));
    actual.control.time_alignment = Some(TimeAlignment::OffsetUnits(4));
    assert!(request.answered_by(&actual));
}

#[test]
fn invalid_control_fields_and_direction_are_rejected() {
    for control in [
        PhysicalControlRequest {
            power_level: Some(11),
            ..Default::default()
        },
        PhysicalControlRequest {
            time_alignment: Some(TimeAlignment::OffsetUnits(31)),
            ..Default::default()
        },
    ] {
        assert!(
            TrafficMessage::PhysicalControl(PhysicalControlFields {
                control,
                ..Default::default()
            })
            .encode()
            .is_err()
        );
    }
    let conflict = PhysicalControlFields {
        control: request(),
        dtx_mode: Some(0),
        ..Default::default()
    };
    assert!(TrafficMessage::PhysicalControl(conflict).encode().is_err());
    let bits = TrafficMessage::PhysicalControl(PhysicalControlFields {
        control: request(),
        ..Default::default()
    })
    .encode()
    .unwrap();
    assert_eq!(
        decode_prefix_in(&bits, Direction::Reverse),
        Err(TrafficMessageError::Field("physical_control_direction"))
    );
    let mut count = bits.clone();
    count[20..26].fill(false);
    assert_eq!(
        decode_prefix_in(&count, Direction::Forward),
        Err(TrafficMessageError::Field("physical_control_count"))
    );
    let incomplete = literal("00 11110010 000001 0010 0000");
    assert_eq!(
        decode_prefix_in(&incomplete, Direction::Forward),
        Err(TrafficMessageError::Field("physical_control_length"))
    );
}

#[test]
fn unknown_control_tail_is_preserved_without_becoming_a_known_field() {
    let fields = PhysicalControlFields {
        unparsed_bits: literal("1000 0101 0010 1010"),
        ..Default::default()
    };
    let message = TrafficMessage::PhysicalControl(fields);
    let bits = message.encode().unwrap();
    assert_eq!(
        decode_prefix_in(&bits, Direction::Forward).unwrap().message,
        message
    );
}

#[test]
fn partial_control_reports_retain_other_confirmed_settings_and_replace_dtx_format() {
    let mut confirmed = PhysicalControlFields {
        control: PhysicalControlRequest {
            power_level: Some(7),
            time_alignment: Some(TimeAlignment::OffsetUnits(30)),
            dtx_allowed: Some(false),
            ..Default::default()
        },
        ..Default::default()
    };
    confirmed.update(&PhysicalControlFields {
        control: PhysicalControlRequest {
            power_level: Some(6),
            ..Default::default()
        },
        ..Default::default()
    });
    assert_eq!(confirmed.control.power_level, Some(6));
    assert_eq!(
        confirmed.control.time_alignment,
        Some(TimeAlignment::OffsetUnits(30))
    );
    assert_eq!(confirmed.control.dtx_allowed, Some(false));
    confirmed.update(&PhysicalControlFields {
        dtx_mode: Some(0),
        ..Default::default()
    });
    assert_eq!(confirmed.control.dtx_allowed, None);
    assert_eq!(confirmed.dtx_mode, Some(0));
    confirmed.update(&PhysicalControlFields {
        control: PhysicalControlRequest {
            dtx_allowed: Some(false),
            ..Default::default()
        },
        ..Default::default()
    });
    assert_eq!(confirmed.dtx_mode, None);
}
