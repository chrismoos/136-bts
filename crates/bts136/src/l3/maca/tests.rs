use super::*;
fn basic() -> MacaReport {
    MacaReport {
        measurements: ReportMeasurements::Basic(Measurements {
            long_term: Some(LongTermMeasurement {
                wer: 2,
                ber: 3,
                rss: 17,
                full: true,
            }),
            current: Some(vec![10, 20]),
            other: vec![OtherMeasurements {
                rss: vec![Some(4), None, Some(31)],
            }],
        }),
        ..Default::default()
    }
}
fn extended() -> MacaReport {
    MacaReport {
        measurements: ReportMeasurements::Extended(Measurements {
            long_term: Some(LongTermMeasurement {
                wer: 15,
                ber: 8,
                rss: 41,
                full: false,
            }),
            current: Some(vec![0, 41]),
            other: vec![OtherMeasurements {
                rss: vec![None, Some(40)],
            }],
        }),
        ..Default::default()
    }
}
fn targets() -> MeasurementTargets {
    MeasurementTargets::try_new(
        ControlChannel {
            hyperband: Hyperband::Cellular800,
            channel: 991,
            dvcc: 1,
        },
        vec![1],
        vec![HyperbandChannels {
            hyperband: Hyperband::Pcs1900,
            channels: vec![2, 3, 4],
        }],
    )
    .unwrap()
}
#[test]
fn independent_maca_vectors_preserve_quality_current_count_and_leftmost_report_map() {
    for (report, wire) in [
        (
            basic(),
            vec![0x06, 0x14, 0xe3, 0x21, 0x55, 0x0c, 0xe9, 0x3e],
        ),
        (
            extended(),
            vec![0x06, 0x4f, 0x8a, 0x4a, 0x20, 0x52, 0xc4, 0xd0],
        ),
    ] {
        assert_eq!(report.encode().unwrap(), wire);
        assert_eq!(MacaReport::decode(&wire).unwrap(), report);
    }
    assert_eq!(MacaReport::decode(&[6]).unwrap(), MacaReport::default());
    assert!(
        MacaReport {
            measurements: ReportMeasurements::Basic(Default::default()),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
}
#[test]
fn requested_list_cardinality_and_sparse_order_are_required_for_association() {
    let values = basic().short_term_readings(&targets()).unwrap();
    assert_eq!(values.len(), 5);
    assert_eq!((values[0].channel, values[0].rss_code), (991, Some(10)));
    assert_eq!((values[1].channel, values[1].rss_code), (1, Some(20)));
    assert_eq!(
        (values[2].hyperband, values[2].channel, values[2].rss_code),
        (Hyperband::Pcs1900, 2, Some(4))
    );
    assert_eq!(values[3].rss_code, None);
    assert_eq!((values[4].channel, values[4].rss_code), (4, Some(31)));
    let wrong = MeasurementTargets::try_new(
        targets().serving(),
        vec![],
        targets().other_channels().to_vec(),
    )
    .unwrap();
    assert!(basic().short_term_readings(&wrong).is_err());
    let wrong = MeasurementTargets::try_new(
        targets().serving(),
        vec![1],
        vec![HyperbandChannels {
            hyperband: Hyperband::Pcs1900,
            channels: vec![2, 3],
        }],
    )
    .unwrap();
    assert!(basic().short_term_readings(&wrong).is_err());
    let wrong = MeasurementTargets::try_new(targets().serving(), vec![1], vec![]).unwrap();
    assert!(basic().short_term_readings(&wrong).is_err());
    let report = MacaReport {
        measurements: ReportMeasurements::Basic(Measurements {
            long_term: Some(LongTermMeasurement {
                wer: 0,
                ber: 0,
                rss: 0,
                full: false,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(report.short_term_readings(&targets()).unwrap().is_empty());
}
#[test]
fn maca_targets_enforce_combined_measurement_limits() {
    assert!(MeasurementTargets::try_new(targets().serving(), vec![991], vec![]).is_err());
    assert!(MeasurementTargets::try_new(targets().serving(), vec![2048], vec![]).is_err());
    assert!(MeasurementTargets::try_new(targets().serving(), vec![1; 16], vec![]).is_err());
    for other in [
        vec![HyperbandChannels {
            hyperband: Hyperband::Cellular800,
            channels: vec![1],
        }],
        vec![HyperbandChannels {
            hyperband: Hyperband::Pcs1900,
            channels: vec![],
        }],
        vec![
            HyperbandChannels {
                hyperband: Hyperband::Pcs1900,
                channels: vec![1]
            };
            2
        ],
    ] {
        assert!(MeasurementTargets::try_new(targets().serving(), vec![], other).is_err());
    }
    assert!(
        MeasurementTargets::try_new(
            targets().serving(),
            vec![1; 14],
            vec![HyperbandChannels {
                hyperband: Hyperband::Pcs1900,
                channels: vec![1]
            }]
        )
        .is_ok()
    );
}
#[test]
fn basic_and_extended_ranges_saturation_and_negotiated_format_are_checked() {
    for (format, max) in [
        (MeasurementFormat::Basic, 31),
        (MeasurementFormat::Extended, 41),
    ] {
        assert_eq!(format.rss_description(0).unwrap(), "≤-113 dBm");
        assert_eq!(format.rss_description(1).unwrap(), "-111 dBm");
        assert!(format.rss_description(max).unwrap().starts_with('≥'));
        assert!(format.rss_description(max + 1).is_err());
        for code in 0..=max {
            let fields = Measurements {
                current: Some(vec![code]),
                ..Default::default()
            };
            let report = MacaReport {
                measurements: if format == MeasurementFormat::Basic {
                    ReportMeasurements::Basic(fields)
                } else {
                    ReportMeasurements::Extended(fields)
                },
                ..Default::default()
            };
            assert_eq!(
                MacaReport::decode(&report.encode().unwrap()).unwrap(),
                report
            );
        }
    }
    for code in 42..=63 {
        let report = MacaReport {
            measurements: ReportMeasurements::Extended(Measurements {
                current: Some(vec![code]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(report.encode().is_err());
    }
    basic()
        .validate_negotiation(ProtocolVersion::Is136RevA, MeasurementFormat::Basic)
        .unwrap();
    assert!(
        basic()
            .validate_negotiation(ProtocolVersion::Pv2, MeasurementFormat::Extended)
            .is_err()
    );
    assert!(
        extended()
            .validate_negotiation(ProtocolVersion::Pv1, MeasurementFormat::Extended)
            .is_err()
    );
    extended()
        .validate_negotiation(ProtocolVersion::Pv2, MeasurementFormat::Extended)
        .unwrap();
}
#[test]
fn report_cardinality_map_length_order_and_mixed_formats_are_bounded() {
    for count in 1..=16 {
        let report = MacaReport {
            measurements: ReportMeasurements::Basic(Measurements {
                current: Some(vec![0; count]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            MacaReport::decode(&report.encode().unwrap()).unwrap(),
            report
        );
    }
    for count in 1..=15 {
        let report = MacaReport {
            measurements: ReportMeasurements::Basic(Measurements {
                other: vec![OtherMeasurements {
                    rss: vec![None; count],
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            MacaReport::decode(&report.encode().unwrap()).unwrap(),
            report
        );
    }
    for fields in [
        Measurements {
            current: Some(vec![]),
            ..Default::default()
        },
        Measurements {
            current: Some(vec![0; 17]),
            ..Default::default()
        },
        Measurements {
            other: vec![OtherMeasurements { rss: vec![] }],
            ..Default::default()
        },
        Measurements {
            other: vec![OtherMeasurements {
                rss: vec![None; 16],
            }],
            ..Default::default()
        },
        Measurements {
            other: vec![OtherMeasurements { rss: vec![None] }; 4],
            ..Default::default()
        },
        Measurements {
            long_term: Some(LongTermMeasurement {
                wer: 8,
                ber: 0,
                rss: 0,
                full: false,
            }),
            ..Default::default()
        },
    ] {
        assert!(
            MacaReport {
                measurements: ReportMeasurements::Basic(fields),
                ..Default::default()
            }
            .encode()
            .is_err()
        );
    }
    for fields in [
        vec![(1, 4), (0, 12), (4, 4), (0, 15)],
        vec![(2, 4), (0, 4), (0, 5), (1, 4), (0, 12)],
        vec![(1, 4), (0, 12), (1, 4), (0, 12)],
        vec![(3, 4), (0, 4)],
        vec![(5, 4), (0, 4), (42, 6)],
    ] {
        let mut bits = vec![];
        put(&mut bits, 6, 8).unwrap();
        for (value, width) in fields {
            put(&mut bits, value, width).unwrap();
        }
        assert!(MacaReport::decode(&pack(bits).unwrap()).is_err());
    }
}
#[test]
fn unknown_fields_are_preserved_and_unsolicited_reports_are_not_access_procedures() {
    let mut report = basic();
    report.ignored_bits = vec![
        true, false, false, false, false, false, false, false, false, false, false, false,
    ];
    report.unparsed_bits = vec![
        false, true, true, true, false, true, false, false, true, false,
    ];
    let wire = report.encode().unwrap();
    assert_eq!(MacaReport::decode(&wire).unwrap().encode().unwrap(), wire);
    assert!(MacaReport::decode(&[0x46]).is_err());
    assert!(MacaReport::decode(&[0; 256]).is_err());
    use crate::link::rach::{MessageSet, MobileIdentity};
    assert!(
        crate::runtime::access::classify(&MessageSet {
            identity: MobileIdentity::Min34(1),
            messages: vec![basic().encode().unwrap()]
        })
        .is_err()
    );
}
