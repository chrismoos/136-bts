use super::*;
#[test]
fn absent_standard_and_explicit_signals_match_fixed_control_vectors() {
    assert_eq!(UserAlert::default().encode().unwrap(), [0x25]);
    assert_eq!(UserAlert::decode(&[0x25]).unwrap(), UserAlert::default());
    for (signal, wire) in [
        (Signal::default(), [0x25, 0x10, 0x1f]),
        (
            Signal {
                pitch: Pitch::Low,
                cadence: 12,
                duration_code: 14,
            },
            [0x25, 0x18, 0xce],
        ),
        (
            Signal {
                pitch: Pitch::Medium,
                cadence: 0,
                duration_code: 0,
            },
            [0x25, 0x10, 0],
        ),
    ] {
        let alert = UserAlert {
            signal: Some(signal),
            ..Default::default()
        };
        assert_eq!(alert.encode().unwrap(), wire);
        assert_eq!(UserAlert::decode(&wire).unwrap(), alert);
    }
    assert_eq!(
        crate::l3::notification::SpachConfirmation {
            confirmed_type: MESSAGE_TYPE,
            ..Default::default()
        }
        .encode()
        .unwrap(),
        [0x0f, 0x94]
    );
}
#[test]
fn reserved_signal_codes_duplicates_truncation_and_display_overflow_fail() {
    for pitch in 0..4 {
        for cadence in 0..64 {
            for duration in 0..16 {
                let value = (1u16 << 12) | (pitch << 10) | (cadence << 4) | duration;
                let wire = [0x25, (value >> 8) as u8, value as u8];
                assert_eq!(UserAlert::decode(&wire).is_ok(), pitch < 3 && cadence <= 12);
            }
        }
    }
    for wire in [
        vec![],
        vec![0x65],
        vec![0x25, 0x10],
        vec![0x25, 0x10, 0x1f, 0x10, 0x1f],
        vec![0x25, 0x85, 0x30],
    ] {
        assert!(UserAlert::decode(&wire).is_err(), "{wire:?}");
    }
    assert!(
        UserAlert {
            display: Some("X".repeat(83)),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
    assert!(
        UserAlert {
            display: Some("é".into()),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
}
#[test]
fn optional_address_and_maximum_displays_use_the_alert_layout_and_version_gate() {
    let alert = UserAlert {
        signal: Some(Signal::default()),
        subaddress: Some(Subaddress {
            kind: 2,
            odd: true,
            address: vec![0x21; 20],
        }),
        display: Some("X".repeat(82)),
        multilingual_display: Some(MultilingualDisplay {
            encoding: 4,
            bytes: vec![0; 82],
        }),
        ..Default::default()
    };
    let wire = alert.encode().unwrap();
    assert_eq!(UserAlert::decode(&wire).unwrap(), alert);
    assert!(alert.validate_version(ProtocolVersion::Is136RevA).is_err());
    assert!(alert.validate_version(ProtocolVersion::Pv1).is_ok());
    assert!(UserAlert::decode(&wire[..wire.len() - 1]).is_err());
}
