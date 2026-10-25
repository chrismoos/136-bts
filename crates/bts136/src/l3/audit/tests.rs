use super::*;
#[test]
fn audit_fixed_vectors_keep_flags_and_pfc_before_optional_fields() {
    for (force, debug, bytes) in [
        (false, false, [0x12, 0]),
        (true, false, [0x12, 0x80]),
        (false, true, [0x12, 0x40]),
        (true, true, [0x12, 0xc0]),
    ] {
        let message = AuditOrder {
            force_registration: force,
            debug_display_allowed: debug,
            ..Default::default()
        };
        assert_eq!(message.encode().unwrap(), bytes);
        assert_eq!(AuditOrder::decode(&bytes).unwrap(), message);
        assert!(AuditOrder::decode(&bytes[..1]).is_err());
    }
    for pfc in 0..8 {
        let message = AuditConfirmation {
            pfc_minus_one: pfc,
            ..Default::default()
        };
        let bytes = [1, pfc << 5];
        assert_eq!(message.encode().unwrap(), bytes);
        assert_eq!(AuditConfirmation::decode(&bytes).unwrap(), message);
        assert!(AuditConfirmation::decode(&bytes[..1]).is_err());
    }
}
#[test]
fn audit_options_roundtrip_unknown_tail_and_display_bounds() {
    let mut message = AuditConfirmation {
        pfc_minus_one: 7,
        selected_system: Some(0),
        group: Some(UserGroupId {
            kind: 3,
            value: (1 << 50) - 1,
        }),
        subaddress: Some(Subaddress {
            kind: 2,
            odd: true,
            address: vec![0x21; 20],
        }),
        display: Some("x".repeat(82)),
        multilingual_display: Some(MultilingualDisplay {
            encoding: BMP,
            bytes: vec![0x20; 82],
        }),
        ignored_bits: Vec::new(),
        unparsed_bits: vec![],
    };
    let bytes = message.encode().unwrap();
    assert_eq!(AuditConfirmation::decode(&bytes).unwrap(), message);
    message.unparsed_bits = vec![true; 8];
    let bytes = message.encode().unwrap();
    assert_eq!(
        AuditConfirmation::decode(&bytes).unwrap().encode().unwrap(),
        bytes
    );
    message.display = Some("é".into());
    assert!(message.encode().is_err());
    message.display = Some("x".repeat(83));
    assert!(message.encode().is_err());
    assert!(
        AuditConfirmation {
            multilingual_display: Some(MultilingualDisplay {
                encoding: 1,
                bytes: vec![]
            }),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
    assert!(
        AuditConfirmation {
            multilingual_display: Some(MultilingualDisplay {
                encoding: BMP,
                bytes: vec![0]
            }),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
}

#[test]
fn audit_declared_display_length_requires_complete_payload() {
    let message = AuditConfirmation {
        display: Some("x".repeat(82)),
        ..Default::default()
    };
    let bytes = message.encode().unwrap();
    for end in 4..bytes.len() {
        assert!(AuditConfirmation::decode(&bytes[..end]).is_err());
    }
}
#[test]
fn audit_rejects_duplicate_known_fields_and_invalid_field_bounds() {
    let message = AuditConfirmation {
        selected_system: Some(0),
        unparsed_bits: vec![false, false, false, true, false, false, false, true],
        ..Default::default()
    };
    assert!(AuditConfirmation::decode(&message.encode().unwrap()).is_err());
    for value in [
        AuditConfirmation {
            pfc_minus_one: 8,
            ..Default::default()
        },
        AuditConfirmation {
            selected_system: Some(16),
            ..Default::default()
        },
        AuditConfirmation {
            group: Some(UserGroupId { kind: 4, value: 0 }),
            ..Default::default()
        },
        AuditConfirmation {
            group: Some(UserGroupId {
                kind: 0,
                value: 1 << 20,
            }),
            ..Default::default()
        },
    ] {
        assert!(value.encode().is_err());
    }
}

#[test]
fn multilingual_bmp_display_rejects_surrogate_units_including_supplementary_pairs() {
    const DATA_START_BITS: usize = 31;
    for bytes in [vec![0xd8, 0], vec![0xdc, 0], vec![0xd8, 0, 0xdc, 0]] {
        let invalid = AuditConfirmation {
            multilingual_display: Some(MultilingualDisplay {
                encoding: 4,
                bytes: bytes.clone(),
            }),
            ..Default::default()
        };
        assert!(invalid.encode().is_err());
        let valid = AuditConfirmation {
            multilingual_display: Some(MultilingualDisplay {
                encoding: 4,
                bytes: vec![0; bytes.len()],
            }),
            ..Default::default()
        };
        let mut wire_bits = control_fields::bits(&valid.encode().unwrap()).unwrap();
        for (octet, byte) in bytes.iter().enumerate() {
            for bit in 0..8 {
                wire_bits[DATA_START_BITS + octet * 8 + bit] = byte & (1 << (7 - bit)) != 0;
            }
        }
        assert!(AuditConfirmation::decode(&pack(wire_bits).unwrap()).is_err());
    }
}
