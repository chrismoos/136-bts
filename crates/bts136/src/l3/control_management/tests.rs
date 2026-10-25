use super::*;

#[test]
fn independent_retry_and_parameter_update_vectors_match_non_aligned_fields() {
    let retry = ControlManagement::DirectedRetry(DirectedRetry {
        last_try: true,
        rcf_auth: Some(RcfAuth {
            read_control_filler: true,
            authentication: false,
        }),
        dtx: Some(AnalogDtx::Attenuation8Db),
        channels: vec![RetryChannel {
            hyperband: Hyperband::Pcs1900,
            channel: 1365,
        }],
        ..Default::default()
    });
    let wire = [0x16, 0x8c, 0x51, 0xb5, 0x54];
    assert_eq!(retry.encode().unwrap(), wire);
    assert_eq!(ControlManagement::decode(&wire).unwrap(), retry);
    let update = ControlManagement::ParameterUpdate(ParameterUpdate {
        request_number: 10,
        subaddress: Some(Subaddress {
            kind: 2,
            odd: true,
            address: vec![0xab],
        }),
        ..Default::default()
    });
    let wire = [0x19, 0xa1, 0x02, 0x14, 0xab];
    assert_eq!(update.encode().unwrap(), wire);
    assert_eq!(ControlManagement::decode(&wire).unwrap(), update);
    assert_eq!(
        ControlManagement::decode(&[0x16, 0x18, 0xf7, 0xc0]).unwrap(),
        ControlManagement::DirectedRetry(DirectedRetry {
            channels: vec![RetryChannel {
                hyperband: Hyperband::Cellular800,
                channel: 991
            }],
            ..Default::default()
        })
    );
    assert_eq!(
        ControlManagement::decode(&[0x16, 0x00]).unwrap(),
        ControlManagement::DirectedRetry(Default::default())
    );
    assert_eq!(
        ControlManagement::decode(&[0x19, 0xf0]).unwrap(),
        ControlManagement::ParameterUpdate(ParameterUpdate {
            request_number: 15,
            ..Default::default()
        })
    );
}

#[test]
fn retry_channel_counts_all_hyperbands_and_request_number_boundaries_are_checked() {
    for count in 0..=6 {
        let retry = ControlManagement::DirectedRetry(DirectedRetry {
            channels: (0..count)
                .map(|index| RetryChannel {
                    hyperband: Hyperband::try_from((index % 4) as u8).unwrap(),
                    channel: if index % 2 == 0 { 0 } else { 2047 },
                })
                .collect(),
            ..Default::default()
        });
        assert_eq!(
            ControlManagement::decode(&retry.encode().unwrap()).unwrap(),
            retry
        );
    }
    let retry = DirectedRetry {
        channels: vec![
            RetryChannel {
                hyperband: Hyperband::Cellular800,
                channel: 1
            };
            7
        ],
        ..Default::default()
    };
    assert!(ControlManagement::DirectedRetry(retry).encode().is_err());
    let retry = DirectedRetry {
        channels: vec![RetryChannel {
            hyperband: Hyperband::Cellular800,
            channel: 2048,
        }],
        ..Default::default()
    };
    assert!(ControlManagement::DirectedRetry(retry).encode().is_err());
    for request_number in 0..=15 {
        let update = ControlManagement::ParameterUpdate(ParameterUpdate {
            request_number,
            ..Default::default()
        });
        assert_eq!(
            ControlManagement::decode(&update.encode().unwrap()).unwrap(),
            update
        );
    }
    assert!(
        ControlManagement::ParameterUpdate(ParameterUpdate {
            request_number: 16,
            ..Default::default()
        })
        .encode()
        .is_err()
    );
}

#[test]
fn analog_flags_and_reserved_dtx_are_distinct_from_digital_retry() {
    let mut retry = DirectedRetry::default();
    retry.validate_access(RetryAccess::Digital).unwrap();
    assert!(retry.validate_access(RetryAccess::Analog).is_err());
    retry.rcf_auth = Some(RcfAuth {
        read_control_filler: true,
        authentication: true,
    });
    assert!(retry.validate_access(RetryAccess::Analog).is_err());
    for dtx in [
        AnalogDtx::Unsupported,
        AnalogDtx::Attenuation8Db,
        AnalogDtx::UnlimitedAttenuation,
    ] {
        retry.dtx = Some(dtx);
        retry.validate_access(RetryAccess::Analog).unwrap();
        assert!(retry.validate_access(RetryAccess::Digital).is_err());
        let message = ControlManagement::DirectedRetry(retry.clone());
        assert_eq!(
            ControlManagement::decode(&message.encode().unwrap()).unwrap(),
            message
        );
    }
    assert!(AnalogDtx::try_from(1).is_err());
    assert!(AnalogDtx::try_from(4).is_err());
}

#[test]
fn malformed_duplicate_out_of_order_and_seventh_retry_channel_are_rejected() {
    for wire in [
        &[0x16][..],
        &[0x19],
        &[0x56, 0],
        &[0x18, 0],
        &[0x16, 0x18],
        &[0x16, 0x08, 0x10],
        &[0x19, 0x00, 0x00],
    ] {
        assert!(ControlManagement::decode(wire).is_err(), "{wire:02x?}");
    }
    let mut bits = Vec::new();
    put(&mut bits, 22, 8).unwrap();
    put(&mut bits, 0, 1).unwrap();
    for _ in 0..7 {
        put(&mut bits, 3, 4).unwrap();
        put(&mut bits, 1, 13).unwrap();
    }
    assert!(ControlManagement::decode(&pack(bits).unwrap()).is_err());
    for fields in [
        vec![(1, 4), (0, 2), (1, 4), (0, 2)],
        vec![(2, 4), (0, 2), (1, 4), (0, 2)],
        vec![(2, 4), (1, 2)],
    ] {
        let mut bits = vec![];
        put(&mut bits, 22, 8).unwrap();
        put(&mut bits, 0, 1).unwrap();
        for (value, width) in fields {
            put(&mut bits, value, width).unwrap();
        }
        assert!(ControlManagement::decode(&pack(bits).unwrap()).is_err());
    }
}

#[test]
fn optional_subaddresses_and_unknown_parameter_rules_are_preserved() {
    for message in [
        ControlManagement::DirectedRetry(DirectedRetry {
            subaddress: Some(Subaddress {
                kind: 2,
                odd: false,
                address: vec![255; 20],
            }),
            ..Default::default()
        }),
        ControlManagement::ParameterUpdate(ParameterUpdate {
            subaddress: Some(Subaddress {
                kind: 0,
                odd: false,
                address: vec![],
            }),
            ..Default::default()
        }),
    ] {
        assert_eq!(
            ControlManagement::decode(&message.encode().unwrap()).unwrap(),
            message
        );
    }
    let mut message = ParameterUpdate {
        request_number: 3,
        ..Default::default()
    };
    message.ignored_bits = vec![true, false, false, false];
    message.ignored_bits.extend([false; 8]);
    let wire = ControlManagement::ParameterUpdate(message.clone())
        .encode()
        .unwrap();
    assert_eq!(
        ControlManagement::decode(&wire).unwrap(),
        ControlManagement::ParameterUpdate(message)
    );
    let message = ControlManagement::ParameterUpdate(ParameterUpdate {
        unparsed_bits: vec![false, true, false, false, true, false, true, false],
        ..Default::default()
    });
    let wire = message.encode().unwrap();
    assert_eq!(
        ControlManagement::decode(&wire).unwrap().encode().unwrap(),
        wire
    );
}

#[test]
fn message_and_subaddress_lengths_are_bounded() {
    assert!(ControlManagement::decode(&[0; 256]).is_err());
    assert!(
        ControlManagement::ParameterUpdate(ParameterUpdate {
            ignored_bits: vec![true; 2040],
            ..Default::default()
        })
        .encode()
        .is_err()
    );
    assert!(
        ControlManagement::DirectedRetry(DirectedRetry {
            subaddress: Some(Subaddress {
                kind: 2,
                odd: false,
                address: vec![0; 21]
            }),
            ..Default::default()
        })
        .encode()
        .is_err()
    );
}
