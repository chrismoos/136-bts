use super::*;
use crate::l3::address::{AddressInfo, AddressValue};

#[test]
fn fixed_test_registration_vectors_use_lsb_first_system_positions_and_nibble_lists() {
    let request = Request {
        selected_systems: 0x8001,
        number_request: Some(true),
        ..Default::default()
    };
    assert_eq!(request.encode().unwrap(), [0x11, 0x80, 0x01, 0x21]);
    assert_eq!(Request::decode(&[0x11, 0x80, 0x01, 0x21]).unwrap(), request);
    let response = Response {
        accepted_systems: 1,
        names: Some(NameList::try_new(None, &[b"A".to_vec()]).unwrap()),
        ..Default::default()
    };
    let wire = [0x23, 0, 1, 0x90, 0x21, 0x41, 0];
    assert_eq!(response.encode().unwrap(), wire);
    assert_eq!(Response::decode(&wire).unwrap(), response);
    let response = Response {
        accepted_systems: 0x8000,
        system_id: Some("A".into()),
        ..Default::default()
    };
    let wire = [0x23, 0x80, 0, 0x80, 0x14, 0x10];
    assert_eq!(response.encode().unwrap(), wire);
    assert_eq!(Response::decode(&wire).unwrap(), response);
}

#[test]
fn every_name_count_preserves_wire_bytes_and_resolves_filler_with_broadcast_context() {
    for count in 0..=16 {
        for encoding in [None, Some(3), Some(4), Some(18)] {
            let name = if encoding == Some(4) {
                vec![0, 65]
            } else {
                b"A".to_vec()
            };
            let mut names = vec![name; count];
            if let Some(last) = names.last_mut() {
                last.clear();
            }
            let list = NameList::try_new(encoding, &names).unwrap();
            assert_eq!(list.resolve(count).unwrap(), names);
            let mut response = Response {
                accepted_systems: u16::MAX,
                ..Default::default()
            };
            if encoding.is_some() {
                response.multilingual_names = Some(list);
            } else {
                response.names = Some(list);
            }
            assert_eq!(
                Response::decode(&response.encode().unwrap()).unwrap(),
                response
            );
        }
    }
    let list = NameList::try_new(None, &[b"A".to_vec()]).unwrap();
    assert_eq!(list.resolve(1).unwrap(), vec![b"A".to_vec()]);
    assert_eq!(list.resolve(2).unwrap(), vec![b"A".to_vec(), vec![]]);
    assert!(list.resolve(0).is_err());
    assert!(list.resolve(17).is_err());
}

#[test]
fn response_names_cannot_display_unaccepted_systems_or_reserved_encodings() {
    let names = NameList::try_new(None, &[b"A".to_vec()]).unwrap();
    assert!(
        Response {
            names: Some(names),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
    assert!(Response::decode(&[0x23, 0, 0, 0x90, 0x21, 0x41, 0]).is_err());
    for encoding in [Some(0), Some(1), Some(19), Some(31)] {
        assert!(NameList::try_new(encoding, &[]).is_err());
    }
    assert!(NameList::try_new(None, &[vec![0x80]]).is_err());
    assert!(NameList::try_new(Some(4), &[vec![0]]).is_err());
    assert!(NameList::try_new(Some(4), &[vec![0xd8, 0]]).is_err());
    assert!(NameList::try_new(None, &[vec![65; 16]]).is_err());
    assert!(NameList::try_new(Some(4), &[vec![0; 32]]).is_err());
    assert!(NameList::try_new(None, &vec![vec![]; 17]).is_err());
    let mut names = vec![b"A".to_vec(); 16];
    names[0].clear();
    let response = Response {
        accepted_systems: 0xfffe,
        names: Some(NameList::try_new(None, &names).unwrap()),
        ..Default::default()
    };
    assert_eq!(
        Response::decode(&response.encode().unwrap()).unwrap(),
        response
    );
}

#[test]
fn all_response_fields_version_and_directory_obligations_are_checked() {
    let address = Subaddress {
        kind: 2,
        odd: false,
        address: vec![0x12],
    };
    let request = Request {
        selected_systems: 5,
        subaddress: Some(address.clone()),
        number_request: Some(true),
        ..Default::default()
    };
    assert!(
        request
            .validate_version(ProtocolVersion::Is136RevA)
            .is_err()
    );
    request.validate_version(ProtocolVersion::Pv1).unwrap();
    assert_eq!(
        Request::decode(&request.encode().unwrap()).unwrap(),
        request
    );
    let mut response = Response {
        accepted_systems: 5,
        subaddress: Some(address),
        system_id: Some("136BTS".into()),
        names: Some(
            NameList::try_new(None, &[b"One".to_vec(), vec![], b"Three".to_vec()]).unwrap(),
        ),
        multilingual_id: Some(MultilingualDisplay {
            encoding: 4,
            bytes: vec![0x03, 0xa9],
        }),
        multilingual_names: Some(
            NameList::try_new(Some(3), &[vec![0xc4], vec![], vec![0xd6]]).unwrap(),
        ),
        ..Default::default()
    };
    assert!(
        response
            .validate_for_request(&request, ProtocolVersion::Pv1)
            .is_err()
    );
    response.directory = Some(AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd("1001".into()),
    });
    response
        .validate_for_request(&request, ProtocolVersion::Pv1)
        .unwrap();
    assert!(
        response
            .validate_version(ProtocolVersion::Is136RevA)
            .is_err()
    );
    assert_eq!(
        Response::decode(&response.encode().unwrap()).unwrap(),
        response
    );
}

#[test]
fn malformed_test_registration_fields_order_lengths_and_unknown_prefixes_are_checked() {
    for bytes in [
        &[0x11, 0][..],
        &[0x11, 0, 0, 0x22],
        &[0x11, 0, 0, 0x21, 0x21],
        &[0x11, 0, 0, 0x10],
        &[0x51, 0, 0],
    ] {
        assert!(Request::decode(bytes).is_err(), "{bytes:02x?}");
    }
    for bytes in [
        &[0x23, 0][..],
        &[0x23, 0, 1, 0x80, 0x14],
        &[0x23, 0, 1, 0xb0, 0],
        &[0x23, 0, 1, 0x90, 0x2f, 0xff, 0xff],
        &[0x23, 0, 1, 0x80, 0, 0x80, 0],
        &[0x63, 0, 0],
    ] {
        assert!(Response::decode(bytes).is_err(), "{bytes:02x?}");
    }
    let request = Request {
        ignored_bits: vec![
            true, false, false, false, false, false, false, false, false, false, false, false,
        ],
        unparsed_bits: vec![false, true, false, false, true, false, true, false],
        ..Default::default()
    };
    let wire = request.encode().unwrap();
    let decoded = Request::decode(&wire).unwrap();
    assert_eq!(decoded.encode().unwrap(), wire);
    let response = Response {
        unparsed_bits: vec![false, true, false, false, true, false, true, false],
        ..Default::default()
    };
    let wire = response.encode().unwrap();
    assert_eq!(Response::decode(&wire).unwrap().encode().unwrap(), wire);
}

#[test]
fn test_registration_never_creates_or_replaces_a_normal_registration() {
    use crate::{
        core::registry::{LocalRegistry, ProvisionedSubscriber},
        link::rach::{MessageSet, MobileIdentity},
    };
    let identity = MobileIdentity::Min34(1);
    let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
        identity,
        number: "1001".into(),
    }])
    .unwrap();
    let set = MessageSet {
        identity,
        messages: vec![
            Request {
                selected_systems: 1,
                ..Default::default()
            }
            .encode()
            .unwrap(),
        ],
    };
    assert!(crate::runtime::access::classify(&set).is_err());
    assert!(registry.register(&set, 0).is_err());
    assert!(registry.subscribers()[0].registration.is_none());
    let registration = crate::l3::registration::AccessMessage::Registration {
        registration_type: crate::l3::registration::RegistrationType::PowerUp,
        scm: 15,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    registry
        .register(
            &MessageSet {
                identity,
                messages: vec![registration],
            },
            0,
        )
        .unwrap();
    let old = registry.subscribers()[0].clone();
    let owner = registry.registration_handle(identity).unwrap();
    assert!(registry.register(&set, 1).is_err());
    assert_eq!(registry.subscribers()[0], old);
    assert!(registry.registration_current(&owner));
}

#[test]
fn multilingual_system_id_uses_count_including_encoding_octet_and_message_bounds_hold() {
    let response = Response {
        accepted_systems: 1,
        multilingual_id: Some(MultilingualDisplay {
            encoding: 4,
            bytes: vec![0x03, 0xa9],
        }),
        ..Default::default()
    };
    let wire = [0x23, 0, 1, 0xb0, 0x32, 0x00, 0x3a, 0x90];
    assert_eq!(response.encode().unwrap(), wire);
    assert_eq!(Response::decode(&wire).unwrap(), response);
    assert!(Request::decode(&[0; 256]).is_err());
    assert!(Response::decode(&[0; 256]).is_err());
    assert!(
        Request {
            ignored_bits: vec![true; 2040],
            ..Default::default()
        }
        .encode()
        .is_err()
    );
    assert!(
        Response {
            system_id: Some("A".repeat(16)),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
    assert!(
        Response {
            multilingual_id: Some(MultilingualDisplay {
                encoding: 4,
                bytes: vec![0; 32]
            }),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
}
