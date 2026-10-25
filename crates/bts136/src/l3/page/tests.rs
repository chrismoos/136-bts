use super::*;
use crate::l3::address::AddressValue;
use crate::l3::user_alert::Pitch;
fn caller() -> AddressInfo {
    AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd("120*#".into()),
    }
}
#[test]
fn mandatory_signal_presentation_number_and_display_match_independent_control_vectors() {
    let cases = [
        (Page::default(), vec![0x18, 0x10]),
        (
            Page {
                signal: Some(Signal::default()),
                ..Default::default()
            },
            vec![0x18, 0x12, 0x01, 0xf0],
        ),
        (
            Page {
                presentation: Some(Presentation {
                    presentation: 0,
                    screening: 3,
                }),
                calling_number: Some(caller()),
                ..Default::default()
            },
            vec![0x18, 0x13, 0x38, 0x04, 0x03, 0x12, 0xab, 0xc0],
        ),
        (
            Page {
                display: Some("A".into()),
                ..Default::default()
            },
            vec![0x18, 0x1a, 0x01, 0x41],
        ),
    ];
    for (page, wire) in cases {
        assert_eq!(page.encode().unwrap(), wire);
        assert_eq!(Page::decode(&wire).unwrap(), page);
    }
    assert_eq!(
        Page::default().encode().unwrap(),
        crate::l3::call_control::digital_page()
    );
    for service in 0..16 {
        let wire = [0x18, service << 4];
        assert_eq!(Page::decode(&wire).is_ok(), service <= 9);
    }
    assert!(Page::decode(&[0x58, 0x10]).is_err());
    assert!(Page::decode(&[0x18]).is_err());
}
#[test]
fn all_optional_fields_preserve_unaligned_layout_versions_and_subaddress_bounds() {
    let page = Page {
        called_subaddress: Some(Subaddress {
            kind: 0,
            odd: false,
            address: vec![0x12, 0x34],
        }),
        signal: Some(Signal {
            pitch: Pitch::High,
            cadence: 12,
            duration_code: 3,
        }),
        presentation: Some(Presentation {
            presentation: 1,
            screening: 2,
        }),
        calling_number: Some(caller()),
        calling_subaddress: Some(Subaddress {
            kind: 2,
            odd: true,
            address: vec![0xff; 20],
        }),
        display: Some("Incoming call".into()),
        called_party: Some(AddressInfo {
            number_type: 0,
            numbering_plan: 14,
            address: AddressValue::Ira(b"a@b".to_vec()),
        }),
        multilingual_display: Some(MultilingualDisplay {
            encoding: 4,
            bytes: vec![0x03, 0xa9],
        }),
        ..Default::default()
    };
    let wire = page.encode().unwrap();
    assert_eq!(Page::decode(&wire).unwrap(), page);
    assert!(page.validate_version(ProtocolVersion::Is136RevA).is_err());
    page.validate_version(ProtocolVersion::Pv1).unwrap();
    for length in 0..wire.len() {
        if let Ok(decoded) = Page::decode(&wire[..length]) {
            assert!(decoded.multilingual_display.is_none());
        }
    }
    let mut invalid = page.clone();
    invalid.calling_subaddress.as_mut().unwrap().address.push(0);
    assert!(invalid.encode().is_err());
    invalid = page.clone();
    invalid.multilingual_display.as_mut().unwrap().bytes = vec![0xd8, 0];
    assert!(invalid.encode().is_err());
    invalid = page;
    invalid.multilingual_display.as_mut().unwrap().bytes = vec![0, 1, 2];
    assert!(invalid.encode().is_err());
}
#[test]
fn signal_presentation_and_display_ranges_reject_reserved_or_overflow_values() {
    for pitch in 0..4 {
        for cadence in 0..64 {
            for duration in 0..16 {
                let wire = [0x18, 0x12, (pitch << 6) | cadence, duration << 4];
                assert_eq!(Page::decode(&wire).is_ok(), pitch < 3 && cadence <= 12);
            }
        }
    }
    for value in 0..16 {
        assert_eq!(
            Page::decode(&[0x18, 0x13, value << 4]).is_ok(),
            value >> 2 < 3
        );
    }
    for display in ["A".repeat(83), "é".into()] {
        assert!(
            Page {
                display: Some(display),
                ..Default::default()
            }
            .encode()
            .is_err()
        );
    }
    for (cadence, duration_code) in [(13, 0), (0, 16)] {
        assert!(
            Page {
                signal: Some(Signal {
                    pitch: Pitch::Medium,
                    cadence,
                    duration_code
                }),
                ..Default::default()
            }
            .encode()
            .is_err()
        );
    }
}
#[test]
fn duplicate_order_truncation_unknown_critical_and_noncritical_fields_are_distinct() {
    for wire in [
        vec![0x18, 0x13, 0x33, 0x30],
        vec![0x18, 0x13, 0x32, 0x01, 0xf0],
        vec![0x18, 0x18, 0x02, 0x03],
        vec![0x18, 0x1a, 0x02, 0x41],
        vec![0x18, 0x1f, 0],
        vec![0x18, 0x10, 0],
        vec![0x18, 0x1d, 0x02, 0xff],
    ] {
        assert!(Page::decode(&wire).is_err(), "{wire:02X?}");
    }
    let wire = [0x18, 0x1d, 0x01, 0xff, 0xa0, 0x14, 0x10];
    let decoded = Page::decode(&wire).unwrap();
    assert_eq!(decoded.display.as_deref(), Some("A"));
    assert_eq!(decoded.ignored_bits.len(), 20);
    let wire = [0x18, 0x14, 0xab, 0xcd];
    let decoded = Page::decode(&wire).unwrap();
    assert_eq!(decoded.unparsed_bits.len(), 20);
    assert_eq!(decoded.encode().unwrap(), wire);
}
#[test]
fn maximum_control_message_length_and_e_revision_address_plans_are_enforced() {
    let mut page = Page {
        calling_number: Some(AddressInfo {
            number_type: 0,
            numbering_plan: 1,
            address: AddressValue::Ira(vec![b'1'; 251]),
        }),
        ..Default::default()
    };
    assert_eq!(page.encode().unwrap().len(), 255);
    assert_eq!(Page::decode(&page.encode().unwrap()).unwrap(), page);
    page.calling_number.as_mut().unwrap().address = AddressValue::Ira(vec![b'1'; 252]);
    assert!(page.encode().is_err());
    page.calling_number = Some(AddressInfo {
        number_type: 0,
        numbering_plan: 12,
        address: AddressValue::DpcSsn(vec![1]),
    });
    assert!(page.encode().is_err());
    assert!(Page::decode(&[0x18, 0x18, 0x02, 0x18, 1]).is_err());
    page.calling_number = Some(AddressInfo {
        number_type: 0,
        numbering_plan: 13,
        address: AddressValue::BinaryInternet("192.0.2.1".parse().unwrap()),
    });
    let wire = [0x18, 0x18, 5, 0x1a, 192, 0, 2, 1];
    assert_eq!(page.encode().unwrap(), wire);
    assert_eq!(Page::decode(&wire).unwrap(), page);
}

#[test]
fn alphanumeric_internet_plan_uses_ira_and_never_tbcd() {
    let address = AddressInfo {
        number_type: 0,
        numbering_plan: 14,
        address: AddressValue::Tbcd("123".into()),
    };
    assert!(
        Page {
            calling_number: Some(address),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
    assert!(Page::decode(&[0x18, 0x18, 0x02, 0x1d, 0x12]).is_err());
}
