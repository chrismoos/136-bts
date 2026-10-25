use super::*;
fn message(body: SecurityBody) -> SecurityMessage {
    SecurityMessage {
        body,
        options: Default::default(),
    }
}
fn examples() -> Vec<SecurityMessage> {
    vec![
        message(SecurityBody::Authentication {
            count: 21,
            randc: 0xa5,
            authr: 0x2aaaa,
        }),
        message(SecurityBody::BaseStationChallenge { randbs: 0x12345678 }),
        message(SecurityBody::BaseStationConfirmation { authbs: 0x2aaaa }),
        message(SecurityBody::SsdUpdateOrder {
            randssd: 0x123456789abcde,
        }),
        message(SecurityBody::SsdUpdateConfirmation {
            status: SsdUpdateStatus::AuthbsMismatch,
        }),
        message(SecurityBody::UniqueChallengeOrder { randu: 0x123456 }),
        message(SecurityBody::UniqueChallengeConfirmation { authu: 0x2aaaa }),
    ]
}

#[test]
fn independent_security_vectors_preserve_challenge_widths_and_direction() {
    let wires = [
        vec![2, 0x56, 0x96, 0xaa, 0xaa],
        vec![3, 0x12, 0x34, 0x56, 0x78],
        vec![19, 0xaa, 0xaa, 0x80],
        vec![34, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde],
        vec![16, 0x40],
        vec![36, 0x12, 0x34, 0x56],
        vec![18, 0xaa, 0xaa, 0x80],
    ];
    for (message, wire) in examples().into_iter().zip(wires) {
        assert_eq!(message.encode().unwrap(), wire);
        assert_eq!(
            SecurityMessage::decode(&wire, message.body.direction()).unwrap(),
            message
        );
        let opposite = if message.body.direction() == Direction::Forward {
            Direction::Reverse
        } else {
            Direction::Forward
        };
        assert!(SecurityMessage::decode(&wire, opposite).is_err());
    }
}

#[test]
fn security_field_bounds_and_reserved_ssd_outcomes_are_checked() {
    for body in [
        SecurityBody::Authentication {
            count: 64,
            randc: 0,
            authr: 0,
        },
        SecurityBody::Authentication {
            count: 0,
            randc: 0,
            authr: 1 << 18,
        },
        SecurityBody::BaseStationConfirmation { authbs: 1 << 18 },
        SecurityBody::SsdUpdateOrder { randssd: 1 << 56 },
        SecurityBody::UniqueChallengeOrder { randu: 1 << 24 },
        SecurityBody::UniqueChallengeConfirmation { authu: 1 << 18 },
    ] {
        assert!(message(body).encode().is_err());
    }
    assert_eq!(
        SecurityMessage::decode(&[16, 0], Direction::Reverse)
            .unwrap()
            .body,
        SecurityBody::SsdUpdateConfirmation {
            status: SsdUpdateStatus::Successful
        }
    );
    for status in [2, 3] {
        assert!(SecurityMessage::decode(&[16, status << 6], Direction::Reverse).is_err());
    }
    for body in [
        SecurityBody::Authentication {
            count: 63,
            randc: 255,
            authr: (1 << 18) - 1,
        },
        SecurityBody::BaseStationChallenge { randbs: u32::MAX },
        SecurityBody::BaseStationConfirmation {
            authbs: (1 << 18) - 1,
        },
        SecurityBody::SsdUpdateOrder {
            randssd: (1 << 56) - 1,
        },
        SecurityBody::UniqueChallengeOrder {
            randu: (1 << 24) - 1,
        },
        SecurityBody::UniqueChallengeConfirmation {
            authu: (1 << 18) - 1,
        },
    ] {
        let message = message(body);
        assert_eq!(
            SecurityMessage::decode(&message.encode().unwrap(), message.body.direction()).unwrap(),
            message
        );
    }
}

#[test]
fn all_challenges_and_confirmations_support_non_aligned_subaddress_and_unknown_tails() {
    for mut message in examples().into_iter().skip(1) {
        message.options.subaddress = Some(Subaddress {
            kind: 2,
            odd: true,
            address: vec![255; 20],
        });
        let wire = message.encode().unwrap();
        assert_eq!(
            SecurityMessage::decode(&wire, message.body.direction()).unwrap(),
            message
        );
        message.options.ignored_bits = vec![
            true, false, false, false, false, false, false, false, false, false, false, false,
        ];
        message.options.unparsed_bits = vec![false, true, false, false, true, false, true, false];
        let wire = message.encode().unwrap();
        let decoded = SecurityMessage::decode(&wire, message.body.direction()).unwrap();
        assert_eq!(decoded.encode().unwrap(), wire);
        assert_eq!(decoded.options.subaddress, message.options.subaddress);
    }
}

#[test]
fn authentication_has_exact_fixed_length_and_no_optional_elements() {
    let mut message = examples().remove(0);
    let mut wire = message.encode().unwrap();
    wire.push(0);
    assert!(SecurityMessage::decode(&wire, Direction::Reverse).is_err());
    message.options.subaddress = Some(Subaddress {
        kind: 0,
        odd: false,
        address: vec![],
    });
    assert!(message.encode().is_err());
    for sample in examples() {
        let wire = sample.encode().unwrap();
        for length in 0..wire.len() {
            assert!(SecurityMessage::decode(&wire[..length], sample.body.direction()).is_err());
        }
    }
    assert!(SecurityMessage::decode(&[0x42, 0, 0, 0, 0], Direction::Reverse).is_err());
    assert!(SecurityMessage::decode(&[0; 256], Direction::Reverse).is_err());
}

#[test]
fn unexpected_security_companions_preserve_registration_and_owner() {
    use crate::{
        core::registry::{LocalRegistry, ProvisionedSubscriber},
        l3::{
            broadcast::ProtocolVersion,
            registration::{AccessMessage, RegistrationType},
        },
        link::rach::{MessageSet, MobileIdentity},
    };
    let identity = MobileIdentity::Min34(1);
    let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
        identity,
        number: "1001".into(),
    }])
    .unwrap();
    let registration = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 15,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    registry
        .register(
            &MessageSet {
                identity,
                messages: vec![registration.clone()],
            },
            0,
        )
        .unwrap();
    let snapshot = registry.subscribers()[0].clone();
    let owner = registry.registration_handle(identity).unwrap();
    for message in examples()
        .into_iter()
        .filter(|message| message.body.direction() == Direction::Reverse)
    {
        let wire = message.encode().unwrap();
        for messages in [vec![wire.clone()], vec![registration.clone(), wire]] {
            let set = MessageSet { identity, messages };
            assert!(crate::runtime::access::classify(&set).is_err());
            assert!(registry.register(&set, 1).is_err());
            assert_eq!(registry.subscribers()[0], snapshot);
            assert!(registry.registration_current(&owner));
        }
    }
}

#[test]
fn security_optional_fields_reject_duplicates_reserved_codes_and_truncation() {
    let mut message = message(SecurityBody::UniqueChallengeOrder { randu: 1 });
    for tail in [
        vec![true; 4],
        vec![false; 8],
        vec![false, true, true, true, false, false, false, false],
        vec![false, false, false, true],
    ] {
        message.options.ignored_bits = tail;
        let wire = message.encode().unwrap();
        assert!(SecurityMessage::decode(&wire, Direction::Forward).is_err());
    }
    let address = Subaddress {
        kind: 0,
        odd: false,
        address: vec![],
    };
    message.options.subaddress = Some(address.clone());
    message.options.ignored_bits.clear();
    write_subaddress(&mut message.options.ignored_bits, 1, &address).unwrap();
    let wire = message.encode().unwrap();
    assert!(SecurityMessage::decode(&wire, Direction::Forward).is_err());
}
