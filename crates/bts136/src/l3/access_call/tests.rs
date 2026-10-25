use super::*;
use crate::l3::{address::AddressValue, capability_report::VoiceMode};
#[test]
fn call_sets_require_one_primary_followed_by_at_most_one_supported_serial() {
    use crate::link::rach::{MessageSet, MobileIdentity};

    let serial = SerialNumber {
        esn: 1,
        esnx: None,
        s_esn: None,
        ignored_bits: vec![],
        unparsed_bits: vec![],
    };
    for (kind, primary) in [
        (
            Kind::Origination,
            origin(CallOptions::default()).encode().unwrap(),
        ),
        (
            Kind::PageResponse,
            page(CallOptions::default()).encode().unwrap(),
        ),
    ] {
        let validate = |messages| {
            validate_companions(
                &MessageSet {
                    identity: MobileIdentity::Min34(1),
                    messages,
                },
                kind,
                ProtocolVersion::Is136RevA,
            )
        };
        let encoded_serial = serial.encode().unwrap();
        assert_eq!(validate(vec![primary.clone()]), Ok(()));
        assert_eq!(
            validate(vec![primary.clone(), encoded_serial.clone()]),
            Ok(())
        );
        for messages in [
            vec![],
            vec![encoded_serial.clone(), primary.clone()],
            vec![primary.clone(), primary.clone()],
            vec![primary.clone(), encoded_serial.clone(), primary.clone()],
            vec![primary.clone(), encoded_serial.clone(), encoded_serial],
            vec![primary.clone(), vec![]],
            vec![primary.clone(), vec![SERIAL]],
            vec![primary.clone(), vec![ORIGINATION_TYPE, PAGE_RESPONSE_TYPE]],
        ] {
            assert!(
                validate(messages.clone()).is_err(),
                "{kind:?}: {messages:?}"
            );
        }
        for (esnx, s_esn, required_version) in [
            (Some(1), None, ProtocolVersion::Pv1),
            (None, Some(1), ProtocolVersion::Pv5),
        ] {
            let extended = SerialNumber {
                esnx,
                s_esn,
                ..serial.clone()
            }
            .encode()
            .unwrap();
            assert!(validate(vec![primary.clone(), extended.clone()]).is_err());
            let set = MessageSet {
                identity: MobileIdentity::Min34(1),
                messages: vec![primary.clone(), extended],
            };
            assert_eq!(validate_companions(&set, kind, required_version), Ok(()));
        }
    }
}

fn origin(options: CallOptions) -> OriginationRequest {
    OriginationRequest {
        message: Origination {
            protocol_version: ProtocolVersion::Is136RevA,
            emergency_call: false,
            called_party: AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("1002".into()),
            },
            last_try: false,
            scm: 15,
            service_code: ServiceCode::DigitalSpeech,
        },
        options,
    }
}
fn page(options: CallOptions) -> PageResponseRequest {
    PageResponseRequest {
        message: PageResponse {
            protocol_version: ProtocolVersion::Is136RevA,
            last_try: false,
            scm: 15,
            service_code: ServiceCode::DigitalPreferred,
        },
        options,
    }
}
fn choices() -> CallOptions {
    CallOptions {
        voices: vec![
            LegacyVoice {
                coder: 2,
                privacy: 0,
            },
            LegacyVoice {
                coder: 1,
                privacy: 0,
            },
        ],
        encryption: Some(EncryptionMode {
            algorithm: 0,
            domain: 1,
            key: 1,
        }),
        bandwidth: Some(Bandwidth::FullPreferred),
        ..Default::default()
    }
}
#[test]
fn repeated_preferences_and_known_options_match_non_aligned_independent_vectors() {
    // TIA-136-123-E §§5.4.7/8 put optional fields after 55 and 22 mandatory bits for these messages.
    let orig = [
        0x07, 0x40, 0x18, 0x18, 0xd5, 0x11, 0xe2, 0x28, 0x09, 0x06, 0x09, 0x44,
    ];
    let response = [0x08, 0x43, 0xcc, 0x50, 0x12, 0x0c, 0x12, 0x88];
    assert_eq!(origin(choices()).encode().unwrap(), orig);
    assert_eq!(
        OriginationRequest::decode(&orig).unwrap(),
        origin(choices())
    );
    assert_eq!(page(choices()).encode().unwrap(), response);
    assert_eq!(
        PageResponseRequest::decode(&response).unwrap(),
        page(choices())
    );
    assert!(
        OriginationRequest::decode(&orig)
            .unwrap()
            .options
            .select_baseline()
            .is_ok()
    );
    assert_eq!(
        OriginationRequest::decode(&orig[..11])
            .unwrap()
            .options
            .bandwidth,
        None
    );
    for end in [8, 9, 10] {
        assert!(OriginationRequest::decode(&orig[..end]).is_err());
    }
}
#[test]
fn full_rate_vselp_is_selected_from_supported_alternatives_without_enabling_other_services() {
    for bandwidth in [
        Bandwidth::FullOnly,
        Bandwidth::FullPreferred,
        Bandwidth::HalfPreferred,
        Bandwidth::DoublePreferred,
        Bandwidth::TriplePreferred,
    ] {
        let mut value = choices();
        value.bandwidth = Some(bandwidth);
        assert!(value.select_baseline().is_ok());
    }
    for bandwidth in [
        Bandwidth::HalfOnly,
        Bandwidth::DoubleOnly,
        Bandwidth::TripleOnly,
    ] {
        let mut value = choices();
        value.bandwidth = Some(bandwidth);
        assert_eq!(value.select_baseline(), Err(AdmissionError::Bandwidth));
    }
    let supported = ExtendedVoice {
        privacy: 0,
        modes: vec![
            VoiceMode {
                coder: 2,
                modulation: 0,
                interleaving: 0,
            },
            VoiceMode {
                coder: 1,
                modulation: 0,
                interleaving: 1,
            },
        ],
    };
    let mut value = CallOptions {
        extended_voices: vec![supported.clone(), supported],
        ..Default::default()
    };
    assert!(value.select_baseline().is_ok());
    for modulation in [1, 2] {
        assert_eq!(
            CallOptions {
                extended_voices: vec![ExtendedVoice {
                    privacy: 0,
                    modes: vec![VoiceMode {
                        coder: 1,
                        modulation,
                        interleaving: 1
                    }]
                }],
                ..Default::default()
            }
            .select_baseline(),
            Err(AdmissionError::Codec)
        );
    }
    for list in &mut value.extended_voices {
        list.privacy = 1;
    }
    assert_eq!(value.select_baseline(), Err(AdmissionError::Encryption));
    for list in &mut value.extended_voices {
        list.privacy = 0;
        list.modes.retain(|mode| mode.coder != 1);
    }
    assert_eq!(value.select_baseline(), Err(AdmissionError::Codec));
}
#[test]
fn table_order_context_and_versions_are_validated_separately_from_parameter_codes() {
    let subaddress = Subaddress {
        kind: 2,
        odd: false,
        address: vec![1, 2],
    };
    let options = CallOptions {
        called_subaddress: Some(subaddress.clone()),
        calling_subaddress: Some(subaddress),
        extended_voices: vec![ExtendedVoice {
            privacy: 0,
            modes: vec![VoiceMode {
                coder: 1,
                modulation: 0,
                interleaving: 1,
            }],
        }],
        selected_system: Some(1),
        ..Default::default()
    };
    let mut request = origin(options);
    assert!(request.encode().is_err());
    request.message.protocol_version = ProtocolVersion::Pv4;
    let bytes = request.encode().unwrap();
    assert_eq!(OriginationRequest::decode(&bytes).unwrap(), request);
    assert_eq!(
        request.options.select_baseline(),
        Err(AdmissionError::Addressing)
    );
    let mut response = page(CallOptions {
        extended_voices: request.options.extended_voices.clone(),
        ..Default::default()
    });
    assert!(response.encode().is_err());
    response.message.protocol_version = ProtocolVersion::Pv3;
    assert_eq!(
        PageResponseRequest::decode(&response.encode().unwrap()).unwrap(),
        response
    );
    response.options.voices.push(LegacyVoice {
        coder: 1,
        privacy: 0,
    });
    assert!(response.encode().is_err());
    let mut data = origin(CallOptions {
        data_mode: Some(0),
        ..Default::default()
    });
    assert!(data.encode().is_err());
    data.message.service_code = ServiceCode::AsyncData;
    assert!(data.encode().is_err());
    data.options.bandwidth = Some(Bandwidth::FullOnly);
    assert_eq!(
        OriginationRequest::decode(&data.encode().unwrap()).unwrap(),
        data
    );
    assert_eq!(data.options.select_baseline(), Err(AdmissionError::Service));
    let mut bandwidth = origin(CallOptions {
        bandwidth: Some(Bandwidth::DoublePreferred),
        ..Default::default()
    });
    assert!(bandwidth.encode().is_err());
    bandwidth.message.protocol_version = ProtocolVersion::Pv1;
    assert!(bandwidth.encode().is_ok());
    assert_eq!(
        OriginationRequest::decode(&vec![0; 256]),
        Err(OptionsError::Length)
    );
}
#[test]
fn explicit_known_unsupported_scope_never_defaults_into_voice() {
    for (options, reason) in [
        (
            CallOptions {
                group: Some(UserGroup { kind: 0, value: 1 }),
                ..Default::default()
            },
            AdmissionError::UserGroup,
        ),
        (
            CallOptions {
                encryption: Some(EncryptionMode {
                    algorithm: 1,
                    domain: 1,
                    key: 1,
                }),
                ..Default::default()
            },
            AdmissionError::Encryption,
        ),
        (
            CallOptions {
                presentation: Some(Presentation {
                    presentation: 1,
                    screening: 0,
                }),
                ..Default::default()
            },
            AdmissionError::Addressing,
        ),
        (
            CallOptions {
                voices: vec![LegacyVoice {
                    coder: 2,
                    privacy: 0,
                }],
                ..Default::default()
            },
            AdmissionError::Codec,
        ),
    ] {
        assert_eq!(options.select_baseline(), Err(reason));
    }
}

#[test]
fn future_ies_follow_comprehension_rules_without_erasing_recognized_restrictions() {
    let options = CallOptions {
        voices: vec![LegacyVoice {
            coder: 1,
            privacy: 0,
        }],
        unparsed_bits: "0111111011111111"
            .bytes()
            .map(|byte| byte == b'1')
            .collect(),
        ..Default::default()
    };
    let value = page(options);
    let decoded = PageResponseRequest::decode(&value.encode().unwrap()).unwrap();
    assert!(decoded.options.select_baseline().is_ok());
    assert!(!decoded.options.unparsed_bits.is_empty());
    let value = page(CallOptions {
        voices: vec![LegacyVoice {
            coder: 1,
            privacy: 1,
        }],
        ..value.options.clone()
    });
    let decoded = PageResponseRequest::decode(&value.encode().unwrap()).unwrap();
    assert_eq!(
        decoded.options.select_baseline(),
        Err(AdmissionError::Encryption)
    );
    let mut bits = control_fields::bits(&page(Default::default()).encode().unwrap()).unwrap();
    bits.truncate(22);
    bits.extend(
        "101000000001101010100100001"
            .bytes()
            .map(|byte| byte == b'1'),
    );
    let decoded = PageResponseRequest::decode(&control_fields::pack(bits).unwrap()).unwrap();
    assert_eq!(decoded.options.bandwidth, Some(Bandwidth::FullOnly));
    assert_eq!(decoded.options.ignored_bits.len(), 20);
    assert!(decoded.options.select_baseline().is_ok());
    let mut bits = control_fields::bits(&page(Default::default()).encode().unwrap()).unwrap();
    bits.truncate(22);
    bits.extend("10100000001000000000".bytes().map(|byte| byte == b'1'));
    assert!(PageResponseRequest::decode(&control_fields::pack(bits).unwrap()).is_err());
}

#[test]
fn direct_async_and_wort_service_codes_match_fixed_access_vectors() {
    for (service, final_octet, page_octet) in [
        (ServiceCode::DirectAsyncData, 0xf0, 0x60),
        (ServiceCode::Wort, 0xf2, 0x64),
    ] {
        let bytes = [0x07, 0x40, 0x18, 0x18, 0xd5, 0x11, final_octet];
        let mut expected = origin(CallOptions::default());
        expected.message.service_code = service;
        assert_eq!(OriginationRequest::decode(&bytes).unwrap(), expected);
        assert_eq!(expected.encode().unwrap(), bytes);
        let bytes = [0x08, 0x4d, page_octet];
        let decoded = PageResponseRequest::decode(&bytes).unwrap();
        assert_eq!(decoded.message.service_code, service);
        assert_eq!(decoded.encode().unwrap(), bytes);
    }
    for code in 10..16 {
        assert!(PageResponseRequest::decode(&[0x08, 0x4d, 0x40 | code << 2]).is_err());
    }
}

#[test]
fn async_and_fax_require_both_data_mode_and_bandwidth() {
    for service in [ServiceCode::AsyncData, ServiceCode::G3Fax] {
        let mut request = origin(CallOptions::default());
        request.message.service_code = service;
        assert!(request.encode().is_err());
        request.options.data_mode = Some(0);
        assert_eq!(
            request.encode(),
            Err(OptionsError::Field("bandwidth_context"))
        );
        request.options.bandwidth = Some(Bandwidth::FullOnly);
        let bytes = request.encode().unwrap();
        assert_eq!(OriginationRequest::decode(&bytes).unwrap(), request);
        let mut response = page(request.options.clone());
        response.message.service_code = service;
        let bytes = response.encode().unwrap();
        assert_eq!(PageResponseRequest::decode(&bytes).unwrap(), response);
        response.options.bandwidth = None;
        assert!(response.encode().is_err());
    }
}

#[test]
fn nonvoice_services_use_null_legacy_voice_coder() {
    for service in [ServiceCode::DirectAsyncData, ServiceCode::Wort] {
        let mut request = origin(CallOptions {
            voices: vec![LegacyVoice {
                coder: 0,
                privacy: 0,
            }],
            data_mode: Some(0),
            ..Default::default()
        });
        request.message.service_code = service;
        let bytes = request.encode().unwrap();
        assert_eq!(OriginationRequest::decode(&bytes).unwrap(), request);
        request.options.voices[0].coder = 1;
        assert_eq!(
            request.encode(),
            Err(OptionsError::Field("voice_mode_context"))
        );
    }
}
