use super::*;
use crate::l3::capability_report::{ReportOptions, SupportedServices};
fn parts() -> [Vec<u8>; 3] {
    [
        AccessMessage::Registration {
            registration_type: RegistrationType::PowerUp,
            scm: 15,
            protocol_version: ProtocolVersion::Is136RevA,
        }
        .encode()
        .unwrap(),
        AccessMessage::SerialNumber { esn: 42 }.encode().unwrap(),
        CapabilityReport {
            protocol_version: ProtocolVersion::Pv1,
            scm: 15,
            software_vintage: 2,
            firmware_vintage: 3,
            model: 4,
            manufacturer: 32,
            max_supported_pfc: 5,
            supported_bands: 3,
            services: SupportedServices {
                ira: true,
                ..Default::default()
            },
            options: ReportOptions::default(),
        }
        .encode()
        .unwrap(),
    ]
}
fn fixture() -> LocalRegistry {
    let mut registry = LocalRegistry::try_new(vec![
        ProvisionedSubscriber {
            identity: MobileIdentity::Min34(1),
            number: "1001".into(),
        },
        ProvisionedSubscriber {
            identity: MobileIdentity::Min34(2),
            number: "1002".into(),
        },
    ])
    .unwrap();
    for id in [1, 2] {
        registry
            .register(
                &MessageSet {
                    identity: MobileIdentity::Min34(id),
                    messages: vec![parts()[0].clone()],
                },
                0,
            )
            .unwrap();
    }
    registry
}
#[test]
fn registration_primary_must_precede_every_companion_in_dispatch_and_registry() {
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let mut registry = fixture();
        let before = registry.subscribers().to_vec();
        let owner = registry
            .registration_handle(MobileIdentity::Min34(1))
            .unwrap();
        let other = registry
            .registration_handle(MobileIdentity::Min34(2))
            .unwrap();
        let pieces = parts();
        let set = MessageSet {
            identity: MobileIdentity::Min34(1),
            messages: order.iter().map(|index| pieces[*index].clone()).collect(),
        };
        let classified = crate::runtime::access::classify(&set);
        let result = registry.register(&set, 1);
        if order[0] == 0 {
            assert!(matches!(
                classified,
                Ok(crate::runtime::access::Procedure::Registration)
            ));
            assert_eq!(result.unwrap().message, RegistrationResponse::Accept);
            assert!(!registry.registration_current(&owner));
            assert_eq!(
                registry.subscribers()[0].registration.unwrap().esn,
                Some(42)
            );
            assert!(registry.subscribers()[0].capabilities.is_some());
        } else {
            assert!(classified.is_err(), "{order:?}");
            assert_eq!(result, Err(RegistryError::MissingRegistration), "{order:?}");
            assert_eq!(registry.subscribers(), before);
            assert!(registry.registration_current(&owner));
        }
        assert!(registry.registration_current(&other));
        assert_eq!(registry.subscribers()[1], before[1]);
    }
}
#[test]
fn a_late_registration_cannot_turn_a_serial_number_set_into_a_primary_access() {
    for kind in [
        RegistrationType::PowerUp,
        RegistrationType::Periodic,
        RegistrationType::PowerDown,
        RegistrationType::Deregistration,
    ] {
        let primary = AccessMessage::Registration {
            registration_type: kind,
            scm: 15,
            protocol_version: ProtocolVersion::Is136RevA,
        }
        .encode()
        .unwrap();
        let set = MessageSet {
            identity: MobileIdentity::Min34(1),
            messages: vec![parts()[1].clone(), primary],
        };
        let mut registry = fixture();
        let before = registry.subscribers().to_vec();
        let owner = registry.registration_handle(set.identity).unwrap();
        assert!(crate::runtime::access::classify(&set).is_err());
        assert_eq!(
            registry.register(&set, 2),
            Err(RegistryError::MissingRegistration)
        );
        assert_eq!(registry.subscribers(), before);
        assert!(registry.registration_current(&owner));
    }
}
#[test]
fn duplicate_or_malformed_companions_remain_atomic_after_a_valid_primary() {
    let pieces = parts();
    let malformed_cap = vec![capability_report::MESSAGE_TYPE];
    for messages in [
        vec![pieces[0].clone(), pieces[1].clone(), pieces[1].clone()],
        vec![pieces[0].clone(), pieces[2].clone(), pieces[2].clone()],
        vec![pieces[0].clone(), pieces[1].clone(), malformed_cap],
        vec![pieces[0].clone(), pieces[1].clone(), pieces[0].clone()],
    ] {
        let mut registry = fixture();
        let before = registry.subscribers().to_vec();
        let owner = registry
            .registration_handle(MobileIdentity::Min34(1))
            .unwrap();
        let set = MessageSet {
            identity: MobileIdentity::Min34(1),
            messages,
        };
        assert!(crate::runtime::access::classify(&set).is_err());
        assert!(registry.register(&set, 3).is_err());
        assert_eq!(registry.subscribers(), before);
        assert!(registry.registration_current(&owner));
    }
}
