use super::*;
fn update(value: QueueUpdate) -> QueueMessage {
    QueueMessage {
        body: QueueBody::Update(value),
        subaddress: None,
        ignored_bits: vec![],
        unparsed_bits: vec![],
    }
}
fn serving() -> ControlChannel {
    ControlChannel {
        hyperband: Hyperband::Cellular800,
        channel: 991,
        dvcc: 1,
    }
}
#[test]
fn fixed_queue_vectors_keep_positions_non_aligned_maca_lists_and_display_counts() {
    for (body, wire) in [
        (QueueBody::Disconnect, vec![19]),
        (QueueBody::DisconnectAck, vec![43]),
        (QueueBody::Update(Default::default()), vec![44]),
    ] {
        let message = QueueMessage {
            body,
            subaddress: None,
            ignored_bits: vec![],
            unparsed_bits: vec![],
        };
        assert_eq!(message.encode().unwrap(), wire);
        assert_eq!(
            QueueMessage::decode(&wire, message.body.direction()).unwrap(),
            message
        );
        let opposite = if message.body.direction() == Direction::Forward {
            Direction::Reverse
        } else {
            Direction::Forward
        };
        assert!(QueueMessage::decode(&wire, opposite).is_err());
    }
    let message = update(QueueUpdate {
        position: Some(5),
        current_channels: Some(vec![991]),
        other_channels: vec![HyperbandChannels {
            hyperband: Hyperband::Pcs1900,
            channels: vec![1000],
        }],
        multilingual_display: Some(MultilingualDisplay {
            encoding: 4,
            bytes: vec![0x03, 0xa9],
        }),
        ..Default::default()
    });
    let wire = [
        0x2c, 0x15, 0x30, 0x7b, 0xe8, 0x83, 0xe8, 0x90, 0x32, 0x00, 0x3a, 0x90,
    ];
    assert_eq!(message.encode().unwrap(), wire);
    assert_eq!(
        QueueMessage::decode(&wire, Direction::Forward).unwrap(),
        message
    );
    assert!(QueueMessage::decode(&[42], Direction::Forward).is_err());
    assert_eq!(
        QueueMessage::decode(&[44, 0x80, 0], Direction::Forward).unwrap(),
        update(QueueUpdate {
            display: Some(String::new()),
            ..Default::default()
        })
    );
}

#[test]
fn maca_serving_context_distinguishes_current_frequency_and_other_hyperbands() {
    let mut value = QueueUpdate {
        current_channels: Some(vec![1]),
        other_channels: vec![HyperbandChannels {
            hyperband: Hyperband::Pcs1900,
            channels: vec![991],
        }],
        ..Default::default()
    };
    value.validate_serving_channel(serving()).unwrap();
    value.current_channels = Some(vec![991]);
    assert!(value.validate_serving_channel(serving()).is_err());
    value.current_channels = None;
    value.other_channels[0].hyperband = Hyperband::Cellular800;
    assert!(value.validate_serving_channel(serving()).is_err());
    value.other_channels = vec![
        HyperbandChannels {
            hyperband: Hyperband::Pcs1900,
            channels: vec![1],
        },
        HyperbandChannels {
            hyperband: Hyperband::Cellular700,
            channels: vec![1],
        },
        HyperbandChannels {
            hyperband: Hyperband::Imt2000,
            channels: vec![1],
        },
    ];
    value.validate_serving_channel(serving()).unwrap();
    let message = update(value);
    assert_eq!(
        QueueMessage::decode(&message.encode().unwrap(), Direction::Forward).unwrap(),
        message
    );
}

#[test]
fn queue_positions_maca_counts_widths_and_hyperband_uniqueness_are_bounded() {
    for position in 1..=15 {
        let message = update(QueueUpdate {
            position: Some(position),
            ..Default::default()
        });
        assert_eq!(
            QueueMessage::decode(&message.encode().unwrap(), Direction::Forward).unwrap(),
            message
        );
    }
    for position in [0, 16, 255] {
        assert!(
            update(QueueUpdate {
                position: Some(position),
                ..Default::default()
            })
            .encode()
            .is_err()
        );
    }
    for count in 1..=15 {
        let message = update(QueueUpdate {
            current_channels: Some(vec![2047; count]),
            ..Default::default()
        });
        assert_eq!(
            QueueMessage::decode(&message.encode().unwrap(), Direction::Forward).unwrap(),
            message
        );
    }
    for channels in [vec![], vec![1; 16], vec![2048]] {
        assert!(
            update(QueueUpdate {
                current_channels: Some(channels),
                ..Default::default()
            })
            .encode()
            .is_err()
        );
    }
    let list = HyperbandChannels {
        hyperband: Hyperband::Pcs1900,
        channels: vec![1],
    };
    assert!(
        update(QueueUpdate {
            other_channels: vec![list.clone(), list.clone()],
            ..Default::default()
        })
        .encode()
        .is_err()
    );
    assert!(
        update(QueueUpdate {
            current_channels: Some(vec![1; 15]),
            other_channels: vec![list],
            ..Default::default()
        })
        .encode()
        .is_err()
    );
}

#[test]
fn optional_fields_versions_and_unknown_tails_preserve_complete_queue_messages() {
    for mut message in [
        update(QueueUpdate {
            position: Some(15),
            display: Some("Waiting".into()),
            multilingual_display: Some(MultilingualDisplay {
                encoding: 3,
                bytes: vec![0xc4],
            }),
            ..Default::default()
        }),
        QueueMessage {
            body: QueueBody::Disconnect,
            subaddress: None,
            ignored_bits: vec![],
            unparsed_bits: vec![],
        },
        QueueMessage {
            body: QueueBody::DisconnectAck,
            subaddress: None,
            ignored_bits: vec![],
            unparsed_bits: vec![],
        },
    ] {
        message.subaddress = Some(Subaddress {
            kind: 2,
            odd: false,
            address: vec![255; 20],
        });
        let wire = message.encode().unwrap();
        assert_eq!(
            QueueMessage::decode(&wire, message.body.direction()).unwrap(),
            message
        );
        assert!(
            message
                .validate_version(ProtocolVersion::Is136Rev0)
                .is_err()
        );
        message.validate_version(ProtocolVersion::Pv1).unwrap();
        if matches!(message.body, QueueBody::Update(_)) {
            assert!(
                message
                    .validate_version(ProtocolVersion::Is136RevA)
                    .is_err()
            );
        } else {
            message
                .validate_version(ProtocolVersion::Is136RevA)
                .unwrap();
        }
        message.ignored_bits = vec![
            true, false, true, false, false, false, false, false, false, false, false, false,
        ];
        message.unparsed_bits = vec![false, true, true, false, true, false, true, false];
        let wire = message.encode().unwrap();
        let decoded = QueueMessage::decode(&wire, message.body.direction()).unwrap();
        assert_eq!(decoded.encode().unwrap(), wire);
        assert_eq!(decoded.subaddress, message.subaddress);
    }
}

#[test]
fn malformed_queue_fields_and_repeated_hyperbands_cannot_enter_the_codec() {
    for bytes in [
        &[44, 0x10][..],
        &[44, 0x30],
        &[44, 0x3f],
        &[44, 0x11, 0x11],
        &[44, 0x80, 0x14],
        &[44, 0x90, 0],
        &[0x6c],
    ] {
        assert!(
            QueueMessage::decode(bytes, Direction::Forward).is_err(),
            "{bytes:02x?}"
        );
    }
    let mut bits = vec![];
    put(&mut bits, 44, 8).unwrap();
    for _ in 0..2 {
        put(&mut bits, 4, 4).unwrap();
        put(&mut bits, 1, 2).unwrap();
        put(&mut bits, 0, 4).unwrap();
        put(&mut bits, 1, 11).unwrap();
    }
    assert!(QueueMessage::decode(&pack(bits).unwrap(), Direction::Forward).is_err());
    let mut message = QueueMessage {
        body: QueueBody::DisconnectAck,
        subaddress: Some(Subaddress {
            kind: 0,
            odd: false,
            address: vec![],
        }),
        ignored_bits: vec![],
        unparsed_bits: vec![],
    };
    write_subaddress(
        &mut message.ignored_bits,
        1,
        message.subaddress.as_ref().unwrap(),
    )
    .unwrap();
    assert!(QueueMessage::decode(&message.encode().unwrap(), Direction::Forward).is_err());
    assert!(QueueMessage::decode(&[0; 256], Direction::Forward).is_err());
    assert!(
        update(QueueUpdate {
            display: Some("A".repeat(83)),
            ..Default::default()
        })
        .encode()
        .is_err()
    );
}

#[test]
fn queue_disconnect_does_not_refresh_registration_or_dispatch_as_a_call_release() {
    use crate::{
        core::registry::{LocalRegistry, ProvisionedSubscriber},
        l3::registration::{AccessMessage, RegistrationType},
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
    for messages in [vec![vec![19]], vec![registration, vec![19]]] {
        let set = MessageSet { identity, messages };
        assert!(crate::runtime::access::classify(&set).is_err());
        assert!(registry.register(&set, 1).is_err());
        assert_eq!(registry.subscribers()[0], snapshot);
        assert!(registry.registration_current(&owner));
    }
}
