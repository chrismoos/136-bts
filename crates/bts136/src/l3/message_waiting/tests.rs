use super::*;
#[test]
fn mandatory_counts_match_fixed_vectors_and_confirmations_are_not_notifications() {
    for (count, wire) in [
        (0, [0x17, 0, 0]),
        (1, [0x17, 0, 4]),
        (62, [0x17, 0, 0xf8]),
        (63, [0x17, 0, 0xfc]),
    ] {
        let message = MessageWaiting {
            entries: vec![Waiting { kind: 0, count }],
            ..Default::default()
        };
        assert_eq!(message.encode().unwrap(), wire);
        assert_eq!(MessageWaiting::decode(&wire).unwrap(), message);
    }
    let confirmation = crate::l3::notification::SpachConfirmation {
        confirmed_type: MESSAGE_TYPE,
        ..Default::default()
    };
    assert_eq!(confirmation.encode().unwrap(), [0x0f, 0x5c]);
    assert_eq!(
        crate::l3::notification::SpachConfirmation::decode(&[0x0f, 0x5c]).unwrap(),
        confirmation
    );
    assert!(crate::l3::notification::sms_confirmation(&[0x0f, 0x5c]).is_err());
}
#[test]
fn every_assigned_kind_is_unique_and_reserved_kinds_or_counts_fail() {
    let entries = (0..=7)
        .filter(|kind| *kind != 3)
        .map(|kind| Waiting { kind, count: 63 })
        .collect();
    let message = MessageWaiting {
        entries,
        ..Default::default()
    };
    assert_eq!(
        MessageWaiting::decode(&message.encode().unwrap()).unwrap(),
        message
    );
    for entries in [
        vec![],
        vec![Waiting { kind: 0, count: 64 }],
        vec![Waiting { kind: 3, count: 0 }],
        vec![Waiting { kind: 8, count: 0 }],
        vec![Waiting { kind: 1, count: 2 }; 2],
    ] {
        assert!(
            MessageWaiting {
                entries,
                ..Default::default()
            }
            .encode()
            .is_err()
        );
    }
    for wire in [
        vec![],
        vec![0x17],
        vec![0x57, 0, 0],
        vec![0x17, 0x03, 0],
        vec![0x17, 0, 1],
    ] {
        assert!(MessageWaiting::decode(&wire).is_err(), "{wire:?}");
    }
}
#[test]
fn nonaligned_optional_fields_and_maximum_displays_retain_wire_values() {
    for entries in [
        vec![Waiting { kind: 1, count: 63 }],
        vec![
            Waiting { kind: 0, count: 0 },
            Waiting { kind: 2, count: 62 },
        ],
    ] {
        let message = MessageWaiting {
            entries,
            subaddress: Some(Subaddress {
                kind: 2,
                odd: false,
                address: vec![0x21; 20],
            }),
            display: Some("D".repeat(82)),
            multilingual_display: Some(MultilingualDisplay {
                encoding: 4,
                bytes: vec![0; 82],
            }),
            ..Default::default()
        };
        let wire = message.encode().unwrap();
        assert_eq!(MessageWaiting::decode(&wire).unwrap(), message);
        assert!(
            message
                .validate_version(ProtocolVersion::Is136RevA)
                .is_err()
        );
        assert!(message.validate_version(ProtocolVersion::Pv1).is_ok());
        assert!(MessageWaiting::decode(&wire[..wire.len() - 1]).is_err());
    }
    let confirmation = crate::l3::notification::SpachConfirmation {
        confirmed_type: 23,
        subaddress: Some(Subaddress {
            kind: 2,
            odd: true,
            address: vec![0x21],
        }),
        ..Default::default()
    };
    assert_eq!(
        crate::l3::notification::SpachConfirmation::decode(&confirmation.encode().unwrap())
            .unwrap(),
        confirmation
    );
    for wire in [vec![0x0f], vec![0x4f, 0x5c], vec![0x0f, 0x5d]] {
        assert!(crate::l3::notification::SpachConfirmation::decode(&wire).is_err());
    }
}
