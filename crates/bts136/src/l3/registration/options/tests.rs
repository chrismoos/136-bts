use super::*;
fn request(options: RegistrationOptions) -> RegistrationRequest {
    RegistrationRequest {
        registration_type: RegistrationType::PowerUp,
        scm: 21,
        protocol_version: ProtocolVersion::Pv1,
        options,
    }
}
#[test]
fn optional_fields_start_at_bit_twenty_one_and_preserve_zero_values() {
    let value = request(RegistrationOptions {
        pfc: Some(0),
        sid_report: Some(42),
        number_request: Some(1),
        ..Default::default()
    });
    // TIA-136-123-E §§5.4.12,6.166,6.220,6.21: 21 fixed bits then 0010/000,0111/0001/15-bit SID,0111/0010/0001.
    let wire = [0x0c, 0x1a, 0xa9, 0x07, 0x10, 0x05, 0x4e, 0x42];
    assert_eq!(value.encode().unwrap(), wire);
    assert_eq!(RegistrationRequest::decode(&wire).unwrap(), value);
    assert_eq!(
        request(Default::default()).encode().unwrap(),
        [0x0c, 0x1a, 0xa8]
    );
}
#[test]
fn accept_directory_and_assignment_match_fixed_bits() {
    let options = AcceptOptions {
        pfc: Some(0),
        directory_address: Some(AddressInfo {
            number_type: 0,
            numbering_plan: 1,
            address: address::AddressValue::Tbcd("1001".into()),
        }),
        ..Default::default()
    };
    // TIA-136-123-E §5.3.14: 00011101,0001/000,1001/00000011/00000011/00011010/10100001.
    let wire = [0x1d, 0x11, 0x20, 0x60, 0x63, 0x54, 0x20];
    assert_eq!(options.encode().unwrap(), wire);
    assert_eq!(AcceptOptions::decode(&wire).unwrap(), options);
    for pfc in 0..8 {
        let value = AcceptOptions {
            pfc: Some(pfc),
            ..Default::default()
        };
        assert_eq!(
            AcceptOptions::decode(&value.encode().unwrap()).unwrap(),
            value
        );
    }
}
#[test]
fn registration_option_scope_versions_and_malformed_lengths_are_checked() {
    let mut value = request(RegistrationOptions {
        number_request: Some(1),
        ..Default::default()
    });
    value.protocol_version = ProtocolVersion::Is136RevA;
    assert!(value.encode().is_err());
    for options in [
        RegistrationOptions {
            number_request: Some(2),
            ..Default::default()
        },
        RegistrationOptions {
            target_soc: Some(1),
            ..Default::default()
        },
        RegistrationOptions {
            selected_system: Some(1),
            system_value: Some(1),
            ..Default::default()
        },
    ] {
        assert!(request(options).encode().is_err());
    }
    assert_eq!(
        RegistrationRequest::decode(&vec![0; 256]),
        Err(OptionsError::Length)
    );
    assert!(RegistrationRequest::decode(&[0x0c, 0x1a]).is_err());
    assert!(AcceptOptions::decode(&[0x1d, 0x9f, 0xf0]).is_err());
    assert!(AcceptOptions::decode(&[0x1d, 0x10, 0x20]).is_err());
    let tail = vec![true; 11];
    let value = request(RegistrationOptions {
        unparsed_bits: tail.clone(),
        ..Default::default()
    });
    assert_eq!(
        RegistrationRequest::decode(&value.encode().unwrap())
            .unwrap()
            .options
            .unparsed_bits,
        tail
    );
}
#[test]
fn all_structured_registration_fields_roundtrip_without_enabling_services() {
    for group in [
        GroupRequest::Unspecified,
        GroupRequest::Deallocate,
        GroupRequest::Allocated {
            kind: 0,
            value: (1 << 20) - 1,
        },
        GroupRequest::Allocated {
            kind: 1,
            value: (1 << 24) - 1,
        },
        GroupRequest::Allocated {
            kind: 2,
            value: (1 << 34) - 1,
        },
        GroupRequest::Allocated {
            kind: 3,
            value: (1 << 50) - 1,
        },
    ] {
        let value = request(RegistrationOptions {
            group: Some(group),
            encryption: Some(EncryptionMode {
                algorithm: 1,
                domain: 1,
                key: 1,
            }),
            selected_system: Some(1),
            target_soc: Some(23),
            subaddress: Some(Subaddress {
                kind: 2,
                odd: true,
                address: vec![1, 2],
            }),
            multilingual_number: Some(MultilingualNumber {
                number_type: 0,
                numbering_plan: 1,
                encoding: 2,
                bytes: vec![65, 66],
            }),
            ..Default::default()
        });
        assert_eq!(
            RegistrationRequest::decode(&value.encode().unwrap()).unwrap(),
            value
        );
    }
}
