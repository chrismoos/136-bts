use super::*;
use crate::l3::{
    address::{AddressInfo, AddressValue},
    registration::{EncryptionMode, GroupRequest, MultilingualNumber},
};
fn registry() -> LocalRegistry {
    LocalRegistry::try_new(vec![ProvisionedSubscriber {
        identity: MobileIdentity::Min34(1),
        number: "1001".into(),
    }])
    .unwrap()
}
fn set(options: RegistrationOptions) -> MessageSet {
    MessageSet {
        identity: MobileIdentity::Min34(1),
        messages: vec![
            RegistrationRequest {
                registration_type: RegistrationType::PowerUp,
                scm: 21,
                protocol_version: ProtocolVersion::Pv1,
                options,
            }
            .encode()
            .unwrap(),
        ],
    }
}
#[test]
fn registration_options_assign_only_baseline_pfc_and_provisioned_directory() {
    let mut registry = registry();
    let options = RegistrationOptions {
        pfc: Some(7),
        sid_report: Some(42),
        number_request: Some(1),
        encryption: Some(EncryptionMode {
            algorithm: 0,
            domain: 1,
            key: 1,
        }),
        ..Default::default()
    };
    let reply = registry.register(&set(options.clone()), 10).unwrap();
    assert_eq!(reply.options.pfc, Some(0));
    assert_eq!(
        reply.options.directory_address.unwrap().address,
        AddressValue::Tbcd("1001".into())
    );
    assert_eq!(
        registry.subscribers()[0].registration_options.as_ref(),
        Some(&options)
    );
    registry
        .replace_provisioning(vec![ProvisionedSubscriber {
            identity: MobileIdentity::Min34(1),
            number: "2001".into(),
        }])
        .unwrap();
    assert_eq!(
        registry.subscribers()[0].registration_options.as_ref(),
        Some(&options)
    );
    let reply = registry.register(&set(options), 20).unwrap();
    assert_eq!(
        reply.options.directory_address.unwrap().address,
        AddressValue::Tbcd("2001".into())
    );
}
#[test]
fn unsupported_registration_services_reject_without_mutating_existing_state() {
    let mut registry = registry();
    registry.register(&set(Default::default()), 10).unwrap();
    let previous = registry.subscribers()[0].clone();
    for (options, cause) in [
        (
            RegistrationOptions {
                encryption: Some(EncryptionMode {
                    algorithm: 1,
                    domain: 1,
                    key: 1,
                }),
                ..Default::default()
            },
            RejectCause::EncryptionModeTemporarilyDenied,
        ),
        (
            RegistrationOptions {
                group: Some(GroupRequest::Unspecified),
                ..Default::default()
            },
            RejectCause::UserGroupBarred,
        ),
        (
            RegistrationOptions {
                selected_system: Some(1),
                ..Default::default()
            },
            RejectCause::AddressingNotSupported,
        ),
        (
            RegistrationOptions {
                multilingual_number: Some(MultilingualNumber {
                    number_type: 0,
                    numbering_plan: 1,
                    encoding: 2,
                    bytes: vec![65],
                }),
                ..Default::default()
            },
            RejectCause::AddressingNotSupported,
        ),
        (
            RegistrationOptions {
                c_number: Some(AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: AddressValue::Tbcd("9999".into()),
                }),
                ..Default::default()
            },
            RejectCause::AddressingNotSupported,
        ),
    ] {
        let reply = registry.register(&set(options), 20).unwrap();
        assert_eq!(reply.message, RegistrationResponse::Reject { cause });
        assert_eq!(registry.subscribers()[0], previous);
    }
    let mut malformed = set(Default::default());
    malformed.messages[0] = vec![0x0c, 0x1a, 0xa8, 0x80];
    assert!(registry.register(&malformed, 20).is_err());
    assert_eq!(registry.subscribers()[0], previous);
}
#[test]
fn unencodable_directory_rejects_before_registration_and_unknown_tail_is_preserved() {
    let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
        identity: MobileIdentity::Min34(1),
        number: "1".repeat(510),
    }])
    .unwrap();
    let reply = registry
        .register(
            &set(RegistrationOptions {
                number_request: Some(1),
                ..Default::default()
            }),
            10,
        )
        .unwrap();
    assert_eq!(
        reply.message,
        RegistrationResponse::Reject {
            cause: RejectCause::AddressingNotSupported
        }
    );
    assert!(registry.subscribers()[0].registration.is_none());
    let options = RegistrationOptions {
        unparsed_bits: vec![true; 11],
        ..Default::default()
    };
    registry.register(&set(options.clone()), 20).unwrap();
    assert_eq!(
        registry.subscribers()[0].registration_options.as_ref(),
        Some(&options)
    );
    let mut powerdown = set(Default::default());
    powerdown.messages[0] = AccessMessage::Registration {
        registration_type: RegistrationType::PowerDown,
        scm: 21,
        protocol_version: ProtocolVersion::Pv1,
    }
    .encode()
    .unwrap();
    registry.register(&powerdown, 30).unwrap();
    assert!(registry.subscribers()[0].registration_options.is_none());
}
#[test]
fn serial_reports_preserve_equipment_fields_without_replacing_esn_or_provisioning() {
    let mut registry = registry();
    let serial = registration::SerialNumber {
        esn: 0x12345678,
        esnx: Some(0),
        s_esn: Some(0x10203040),
        ignored_bits: Vec::new(),
        unparsed_bits: vec![],
    };
    let mut request = set(Default::default());
    request.messages[0] = RegistrationRequest {
        registration_type: RegistrationType::PowerUp,
        scm: 21,
        protocol_version: ProtocolVersion::Pv5,
        options: Default::default(),
    }
    .encode()
    .unwrap();
    request.messages.push(serial.encode().unwrap());
    registry.register(&request, 10).unwrap();
    let subscriber = &registry.subscribers()[0];
    assert_eq!(subscriber.registration.unwrap().esn, Some(serial.esn));
    assert_eq!(subscriber.reported_serial.as_ref(), Some(&serial));
    assert_eq!(subscriber.provisioning.number, "1001");
    registry
        .replace_provisioning(vec![ProvisionedSubscriber {
            identity: MobileIdentity::Min34(1),
            number: "2001".into(),
        }])
        .unwrap();
    registry.register(&set(Default::default()), 20).unwrap();
    assert_eq!(
        registry.subscribers()[0].reported_serial.as_ref(),
        Some(&serial)
    );
    request.messages[1] = AccessMessage::SerialNumber { esn: 42 }.encode().unwrap();
    registry.register(&request, 30).unwrap();
    let report = registry.subscribers()[0].reported_serial.as_ref().unwrap();
    assert_eq!(report.esn, 42);
    assert_eq!(report.esnx, None);
    assert_eq!(report.s_esn, None);
    request.messages[0] = AccessMessage::Registration {
        registration_type: RegistrationType::PowerDown,
        scm: 21,
        protocol_version: ProtocolVersion::Pv5,
    }
    .encode()
    .unwrap();
    registry.register(&request, 40).unwrap();
    assert!(registry.subscribers()[0].reported_serial.is_none());
}
#[test]
fn invalid_serial_companions_leave_entire_registration_unchanged() {
    let mut registry = registry();
    registry.register(&set(Default::default()), 10).unwrap();
    let previous = registry.subscribers()[0].clone();
    let mut request = set(Default::default());
    request.messages.push(
        registration::SerialNumber {
            esn: 42,
            esnx: None,
            s_esn: Some(1),
            ignored_bits: Vec::new(),
            unparsed_bits: vec![],
        }
        .encode()
        .unwrap(),
    );
    assert!(matches!(
        registry.register(&request, 20),
        Err(RegistryError::Options(_))
    ));
    assert_eq!(registry.subscribers()[0], previous);
    request.messages[1] = vec![13, 0, 0, 0, 42, 0x10];
    assert!(registry.register(&request, 20).is_err());
    assert_eq!(registry.subscribers()[0], previous);
    request.messages[1] = AccessMessage::SerialNumber { esn: 42 }.encode().unwrap();
    request.messages.push(request.messages[1].clone());
    assert_eq!(
        registry.register(&request, 20),
        Err(RegistryError::DuplicateMessage)
    );
    assert_eq!(registry.subscribers()[0], previous);
}

#[test]
fn noncritical_registration_fields_are_retained_and_malformed_lengths_are_atomic() {
    let mut registry = registry();
    let ignored = vec![
        true, false, false, false, false, false, false, false, false, false, false, false,
    ];
    let options = RegistrationOptions {
        pfc: Some(7),
        ignored_bits: ignored.clone(),
        ..Default::default()
    };
    let reply = registry.register(&set(options.clone()), 10).unwrap();
    assert_eq!(reply.options.pfc, Some(0));
    assert_eq!(
        registry.subscribers()[0].registration_options.as_ref(),
        Some(&options)
    );
    let before = registry.subscribers()[0].registration;
    let malformed = RegistrationOptions {
        unparsed_bits: vec![
            true, false, false, false, true, true, true, true, true, true, true, true,
        ],
        ..Default::default()
    };
    assert!(registry.register(&set(malformed), 20).is_err());
    assert_eq!(registry.subscribers()[0].registration, before);
    assert_eq!(
        registry.subscribers()[0].registration_options.as_ref(),
        Some(&options)
    );
}

#[test]
fn open_registration_omits_unassigned_directory_and_enforces_supported_options() {
    let mut registry = LocalRegistry::try_new(Vec::new()).unwrap();
    registry.set_allow_unknown(true);
    let reply = registry
        .register(
            &set(RegistrationOptions {
                number_request: Some(DIRECTORY_NUMBER_REQUESTED),
                pfc: Some(7),
                ..Default::default()
            }),
            1,
        )
        .unwrap();
    assert_eq!(reply.message, RegistrationResponse::Accept);
    assert_eq!(reply.options.directory_address, None);
    assert_eq!(reply.options.pfc, Some(BASELINE_PFC_MINUS_ONE));
    assert!(reply.encode().is_ok());
    assert!(registry.subscribers().is_empty());
    let prior = registry
        .unprovisioned_phone(MobileIdentity::Min34(1))
        .unwrap()
        .clone();
    let reply = registry
        .register(
            &set(RegistrationOptions {
                encryption: Some(EncryptionMode {
                    algorithm: 1,
                    domain: 1,
                    key: 1,
                }),
                ..Default::default()
            }),
            2,
        )
        .unwrap();
    assert_eq!(
        reply.message,
        RegistrationResponse::Reject {
            cause: RejectCause::EncryptionModeTemporarilyDenied
        }
    );
    assert_eq!(
        registry.unprovisioned_phone(MobileIdentity::Min34(1)),
        Some(&prior)
    );
    registry
        .register(
            &super::tests::request(MobileIdentity::Min34(1), RegistrationType::PowerDown),
            3,
        )
        .unwrap();
    assert!(
        registry
            .unprovisioned_phone(MobileIdentity::Min34(1))
            .unwrap()
            .registration
            .is_none()
    );
}

#[test]
fn open_registration_keeps_a_bounded_unnumbered_history() {
    let mut registry = LocalRegistry::try_new(Vec::new()).unwrap();
    registry.set_allow_unknown(true);
    for value in 1..=MAX_UNPROVISIONED_REGISTRATIONS + 1 {
        assert_eq!(
            registry
                .register(
                    &super::tests::request(
                        MobileIdentity::Min34(value as u64),
                        RegistrationType::PowerUp
                    ),
                    value as u64
                )
                .unwrap()
                .message,
            RegistrationResponse::Accept
        );
    }
    assert_eq!(
        registry.unprovisioned.len(),
        MAX_UNPROVISIONED_REGISTRATIONS
    );
    assert!(
        registry
            .unprovisioned_phone(MobileIdentity::Min34(1))
            .is_none()
    );
    assert!(registry.subscribers().is_empty());
    assert!(registry.registration_owners.is_empty());
}
