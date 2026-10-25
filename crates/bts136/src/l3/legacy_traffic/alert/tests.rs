use super::*;
use crate::l3::{
    legacy_traffic::{self, TrafficMessage},
    relay::Direction,
};
#[test]
fn empty_and_signal_alerts_match_fixed_wire_vectors_without_a_request_number() {
    for (information, wire) in [
        (AlertInformation::default(), vec![0x0c, 0x40]),
        (
            AlertInformation {
                signals: Some(vec![1]),
                ..Default::default()
            },
            vec![0x0c, 0x43, 0x10, 0x40, 0x40],
        ),
    ] {
        let message = TrafficMessage::AlertWithInfo { information };
        let bits = message.encode().unwrap();
        let expected: Vec<_> = wire
            .iter()
            .flat_map(|byte| (0..8).rev().map(move |shift| byte & (1 << shift) != 0))
            .collect();
        assert_eq!(bits, expected);
        assert_eq!(
            legacy_traffic::decode_prefix_in(&expected, Direction::Forward)
                .unwrap()
                .message,
            message
        );
        assert!(legacy_traffic::decode_prefix_in(&expected, Direction::Reverse).is_err());
    }
}

#[test]
fn calling_number_and_name_match_independent_non_aligned_optional_vectors_and_ignore_received_reserved_bits()
 {
    let information = AlertInformation {
        calling_numbers: vec![CallingNumber::network("123").unwrap()],
        calling_name: Some(CallingName::network("AB").unwrap()),
        ..Default::default()
    };
    let message = TrafficMessage::AlertWithInfo {
        information: information.clone(),
    };
    let wire = [
        0x0c, 0x4b, 0x41, 0x40, 0x80, 0xcc, 0x4c, 0x8c, 0xd4, 0x30, 0x34, 0x14, 0x20,
    ];
    let expected: Vec<_> = wire
        .iter()
        .flat_map(|byte| (0..8).rev().map(move |shift| byte & (1 << shift) != 0))
        .collect();
    assert_eq!(message.encode().unwrap(), expected);
    assert_eq!(
        legacy_traffic::decode_prefix_in(&expected, Direction::Forward)
            .unwrap()
            .message,
        message
    );
    let mut reserved = expected;
    reserved[33..38].fill(true);
    reserved[76..80].fill(true);
    assert_eq!(
        legacy_traffic::decode_prefix_in(&reserved, Direction::Forward)
            .unwrap()
            .message,
        message
    );
}
#[test]
fn optional_counts_order_length_and_ira_bounds_are_strict_while_extensions_are_retained() {
    let information = AlertInformation {
        signals: Some(vec![140]),
        calling_numbers: vec![CallingNumber::network(&"1".repeat(15)).unwrap(); 2],
        calling_name: Some(CallingName::network(&"X".repeat(20)).unwrap()),
        unparsed_bits: vec![true, false, true, false],
    };
    let message = TrafficMessage::AlertWithInfo { information };
    let wire = message.encode().unwrap();
    let decoded = legacy_traffic::decode_prefix_in(&wire, Direction::Forward).unwrap();
    assert_eq!(decoded.message.encode().unwrap(), wire);
    let invalid = [
        AlertInformation {
            calling_numbers: vec![CallingNumber::network("1").unwrap(); 3],
            ..Default::default()
        },
        AlertInformation {
            signals: Some(vec![1, 1]),
            ..Default::default()
        },
        AlertInformation {
            calling_name: Some(CallingName {
                presentation: 0,
                screening: 3,
                characters: "é".into(),
            }),
            ..Default::default()
        },
        AlertInformation {
            calling_name: Some(CallingName::network(&"X".repeat(60)).unwrap()),
            calling_numbers: vec![CallingNumber::network("123456789012345").unwrap()],
            ..Default::default()
        },
    ];
    for fields in invalid {
        assert!(fields.validate().is_err());
    }
    for bytes in [
        vec![0x40, 0x40],
        vec![0x40],
        vec![0x50],
        vec![0x10, 0x80],
        vec![0x10, 0x40, 0x44, 0x10, 0x10],
    ] {
        assert!(decode(&bytes).is_err(), "{bytes:?}");
    }
}
#[test]
fn traffic_alert_and_flash_reject_all_reserved_signal_codes() {
    for value in 0..=u8::MAX {
        let valid = value >> 6 < 3 && value & 63 <= 12;
        let alert = AlertInformation {
            signals: Some(vec![value]),
            ..Default::default()
        };
        assert_eq!(alert.validate().is_ok(), valid);
        let flash = crate::l3::legacy_traffic::CallInformation {
            signals: Some(vec![value]),
            ..Default::default()
        };
        assert_eq!(flash.validate().is_ok(), valid);
        let wire = [0x0c, 0x43, 0x10, 0x40 | (value >> 2), (value & 3) << 6];
        let bits: Vec<_> = wire
            .iter()
            .flat_map(|byte| (0..8).rev().map(move |shift| byte & (1 << shift) != 0))
            .collect();
        assert_eq!(
            legacy_traffic::decode_prefix_in(&bits, Direction::Forward).is_ok(),
            valid
        );
    }
}
