use super::*;
fn basic() -> MacaBroadcast {
    MacaBroadcast {
        status: MacaStatus::Both,
        triggers: ReportTriggers {
            registration: true,
            audit: true,
            ..Default::default()
        },
        unrestricted: Some(true),
        current_channels: Some(vec![1, 1000]),
        enhanced: Some(EnhancedControl {
            dpc: true,
            extended: true,
        }),
        ..Default::default()
    }
}
fn multi() -> MacaBroadcast {
    MacaBroadcast {
        kind: BroadcastKind::MultiHyperband,
        status: MacaStatus::ShortTerm,
        triggers: ReportTriggers {
            origination: true,
            page_response: true,
            ..Default::default()
        },
        other_channels: vec![HyperbandChannels {
            hyperband: Hyperband::Pcs1900,
            channels: vec![1000],
        }],
        enhanced: Some(EnhancedControl {
            dpc: false,
            extended: true,
        }),
        ..Default::default()
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
fn independent_broadcast_vectors_keep_mandatory_flags_and_counted_channel_lists() {
    for (message, wire) in [
        (basic(), vec![0x05, 0xe4, 0x64, 0x20, 0x05, 0xf4, 0x1b]),
        (multi(), vec![0x29, 0x98, 0xd0, 0x7d, 0x08, 0x80]),
    ] {
        assert_eq!(message.encode().unwrap(), wire);
        assert_eq!(MacaBroadcast::decode(&wire).unwrap(), message);
    }
    assert_eq!(MacaBroadcast::default().encode().unwrap(), [5, 0]);
    assert_eq!(
        MacaBroadcast::decode(&[5, 0]).unwrap(),
        MacaBroadcast::default()
    );
}
#[test]
fn target_resolution_uses_explicit_override_or_base_fallback_and_retains_an_owned_snapshot() {
    let mut base = basic();
    let mut selected = multi();
    let saved = selected.resolve_targets(serving(), Some(&base)).unwrap();
    assert_eq!(saved.current_channels(), [1, 1000]);
    assert_eq!(saved.other_channels()[0].channels, [1000]);
    base.current_channels = Some(vec![3]);
    assert_eq!(saved.current_channels(), [1, 1000]);
    selected.current_channels = Some(vec![2]);
    let targets = selected.resolve_targets(serving(), Some(&base)).unwrap();
    assert_eq!(targets.current_channels(), [2]);
    selected.current_channels = None;
    assert!(
        selected
            .resolve_targets(serving(), None)
            .unwrap()
            .current_channels()
            .is_empty()
    );
    assert!(
        selected
            .resolve_targets(serving(), Some(&selected))
            .is_err()
    );
}
#[test]
fn fallback_total_limit_and_serving_context_are_checked_after_combining_lists() {
    let mut base = basic();
    base.current_channels = Some(vec![1; 14]);
    let mut selected = multi();
    selected.other_channels[0].channels = vec![2, 3];
    selected.encode().unwrap();
    assert!(selected.resolve_targets(serving(), Some(&base)).is_err());
    selected.current_channels = Some(vec![1]);
    selected.resolve_targets(serving(), Some(&base)).unwrap();
    selected.current_channels = Some(vec![991]);
    assert!(selected.resolve_targets(serving(), None).is_err());
    selected.current_channels = None;
    selected.other_channels[0].hyperband = Hyperband::Cellular800;
    assert!(selected.resolve_targets(serving(), None).is_err());
}
#[test]
fn trigger_status_enhanced_bits_version_and_kind_rules_are_preserved() {
    for status in 0..=3 {
        for triggers in 0..=15 {
            let message = MacaBroadcast {
                status: MacaStatus::try_from(status).unwrap(),
                triggers: ReportTriggers::decode(triggers),
                ..Default::default()
            };
            assert_eq!(
                MacaBroadcast::decode(&message.encode().unwrap()).unwrap(),
                message
            );
        }
    }
    assert!(MacaStatus::try_from(4).is_err());
    assert!(basic().validate_version(ProtocolVersion::Pv1).is_err());
    basic().validate_version(ProtocolVersion::Pv2).unwrap();
    assert_eq!(basic().advertised_format(), MeasurementFormat::Extended);
    assert_eq!(
        MacaBroadcast::default().advertised_format(),
        MeasurementFormat::Basic
    );
    for enhanced in [
        EnhancedControl::default(),
        EnhancedControl {
            dpc: true,
            extended: false,
        },
        EnhancedControl {
            dpc: false,
            extended: true,
        },
        EnhancedControl {
            dpc: true,
            extended: true,
        },
    ] {
        let message = MacaBroadcast {
            enhanced: Some(enhanced),
            ..Default::default()
        };
        assert_eq!(
            MacaBroadcast::decode(&message.encode().unwrap()).unwrap(),
            message
        );
    }
    assert!(
        MacaBroadcast {
            kind: BroadcastKind::MultiHyperband,
            ..Default::default()
        }
        .encode()
        .is_err()
    );
    assert!(
        MacaBroadcast {
            other_channels: multi().other_channels,
            ..Default::default()
        }
        .encode()
        .is_err()
    );
}
#[test]
fn malformed_count_order_reserved_bits_duplicate_bands_and_unknown_tails_are_checked() {
    for fields in [
        vec![(3, 4), (4, 3)],
        vec![(2, 4), (15, 4)],
        vec![(1, 4), (0, 1), (1, 4), (1, 1)],
        vec![(2, 4), (0, 4)],
        vec![(0, 4)],
    ] {
        let mut bits = vec![];
        put(&mut bits, 5, 8).unwrap();
        put(&mut bits, 0, 6).unwrap();
        for (value, width) in fields {
            put(&mut bits, value, width).unwrap();
        }
        let wire = pack(bits).unwrap();
        assert!(MacaBroadcast::decode(&wire).is_err(), "{wire:02x?}");
    }
    let mut selected = multi();
    selected
        .other_channels
        .push(selected.other_channels[0].clone());
    assert!(selected.encode().is_err());
    selected = multi();
    selected.current_channels = Some(vec![]);
    assert!(selected.encode().is_err());
    let mut message = basic();
    message.ignored_bits = vec![
        true, false, false, false, false, false, false, false, false, false, false, false,
    ];
    message.unparsed_bits = vec![false, true, false, false, true, false, true, false];
    let wire = message.encode().unwrap();
    assert_eq!(
        MacaBroadcast::decode(&wire).unwrap().encode().unwrap(),
        wire
    );
    for wire in [vec![], vec![5], vec![0x45, 0], vec![5; 256]] {
        assert!(MacaBroadcast::decode(&wire).is_err());
    }
}
