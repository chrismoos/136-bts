use super::*;
use crate::l3::maca::{LongTermMeasurement, Measurements, OtherMeasurements, ReportMeasurements};
fn serving() -> ControlChannel {
    ControlChannel {
        hyperband: Hyperband::Cellular800,
        channel: 991,
        dvcc: 1,
    }
}
fn triggers() -> ReportTriggers {
    ReportTriggers {
        registration: true,
        origination: true,
        page_response: true,
        audit: true,
    }
}
fn plan(current: Vec<u16>, other: Vec<HyperbandChannels>) -> MacaPlan {
    MacaPlan::try_new(
        serving(),
        ProtocolVersion::Pv2,
        MacaStatus::Both,
        triggers(),
        current,
        other,
    )
    .unwrap()
}
fn report(format: MeasurementFormat, current: Vec<u8>, other: Vec<Vec<Option<u8>>>) -> MacaReport {
    let fields = Measurements {
        current: Some(current),
        other: other
            .into_iter()
            .map(|rss| OtherMeasurements { rss })
            .collect(),
        ..Default::default()
    };
    MacaReport {
        measurements: match format {
            MeasurementFormat::Basic => ReportMeasurements::Basic(fields),
            MeasurementFormat::Extended => ReportMeasurements::Extended(fields),
        },
        ..Default::default()
    }
}
fn offer(plan: &MacaPlan, version: ProtocolVersion) -> MacaOffer {
    plan.offer(AccessTrigger::Registration, version, |_, _| true)
        .unwrap()
        .unwrap()
}
#[test]
fn disabled_triggers_and_non_dcch_versions_cannot_create_an_offer() {
    let plan = MacaPlan::try_new(
        serving(),
        ProtocolVersion::Pv2,
        MacaStatus::Both,
        ReportTriggers {
            registration: true,
            ..Default::default()
        },
        vec![],
        vec![],
    )
    .unwrap();
    for trigger in [
        AccessTrigger::Origination,
        AccessTrigger::PageResponse,
        AccessTrigger::Audit,
    ] {
        assert!(
            plan.offer(trigger, ProtocolVersion::Pv2, |_, _| true)
                .unwrap()
                .is_none()
        );
    }
    assert!(
        plan.offer(
            AccessTrigger::Registration,
            ProtocolVersion::Eia553OrIs54A,
            |_, _| true
        )
        .is_err()
    );
    assert!(
        MacaPlan::try_new(
            serving(),
            ProtocolVersion::Pv2,
            MacaStatus::Disabled,
            triggers(),
            vec![],
            vec![]
        )
        .is_err()
    );
}
#[test]
fn older_handsets_use_basic_reports_and_reject_extended_format() {
    let plan = plan(vec![992], vec![]);
    let old = offer(&plan, ProtocolVersion::Pv1);
    assert_eq!(old.format(), MeasurementFormat::Basic);
    assert_eq!(
        old.short_term_readings(&report(MeasurementFormat::Basic, vec![20, 21], vec![]))
            .unwrap()
            .len(),
        2
    );
    assert!(
        old.short_term_readings(&report(MeasurementFormat::Extended, vec![20, 21], vec![]))
            .is_err()
    );
    let current = offer(&plan, ProtocolVersion::Pv2);
    assert_eq!(current.format(), MeasurementFormat::Extended);
    assert!(
        current
            .short_term_readings(&report(MeasurementFormat::Basic, vec![20, 21], vec![]))
            .is_err()
    );
}
#[test]
fn multi_hyperband_selection_depends_on_a_supported_listed_channel() {
    let plan = plan(
        vec![992],
        vec![HyperbandChannels {
            hyperband: Hyperband::Pcs1900,
            channels: vec![100, 101],
        }],
    );
    let basic = plan
        .offer(
            AccessTrigger::Registration,
            ProtocolVersion::Pv2,
            |_, channel| channel == 102,
        )
        .unwrap()
        .unwrap();
    assert!(basic.targets().other_channels().is_empty());
    let multi = plan
        .offer(
            AccessTrigger::Registration,
            ProtocolVersion::Pv2,
            |band, channel| band == Hyperband::Pcs1900 && channel == 101,
        )
        .unwrap()
        .unwrap();
    assert_eq!(multi.targets().current_channels(), &[992]);
    assert_eq!(multi.targets().other_channels()[0].channels, vec![100, 101]);
    assert!(
        multi
            .short_term_readings(&report(
                MeasurementFormat::Extended,
                vec![10, 11],
                vec![vec![Some(12)]]
            ))
            .is_err()
    );
}
#[test]
fn full_fifteen_target_context_preserves_all_bands_and_clear_map_bits() {
    let plan = plan(
        vec![992, 993, 994, 995],
        vec![
            HyperbandChannels {
                hyperband: Hyperband::Pcs1900,
                channels: vec![100, 101, 102, 103, 104],
            },
            HyperbandChannels {
                hyperband: Hyperband::Cellular700,
                channels: vec![200, 201, 202],
            },
            HyperbandChannels {
                hyperband: Hyperband::Imt2000,
                channels: vec![300, 301, 302],
            },
        ],
    );
    let packets = plan.messages();
    assert_eq!(packets.len(), 2);
    let base = MacaBroadcast::decode(&packets[0]).unwrap();
    let multi = MacaBroadcast::decode(&packets[1]).unwrap();
    assert_eq!(multi.unrestricted, Some(true));
    assert!(multi.current_channels.is_none());
    assert_eq!(
        multi
            .resolve_targets(serving(), Some(&base))
            .unwrap()
            .current_channels(),
        &[992, 993, 994, 995]
    );
    let offer = plan
        .offer(
            AccessTrigger::Registration,
            ProtocolVersion::Pv2,
            |band, _| band == Hyperband::Pcs1900,
        )
        .unwrap()
        .unwrap();
    let values = offer
        .short_term_readings(&report(
            MeasurementFormat::Extended,
            vec![10, 11, 12, 13, 14],
            vec![vec![Some(15); 5], vec![None; 3], vec![None; 3]],
        ))
        .unwrap();
    assert_eq!(values.len(), 16);
    assert_eq!(values[0].channel, 991);
    assert_eq!(values[10].hyperband, Hyperband::Cellular700);
    assert_eq!(values[10].rss_code, None);
    assert_eq!(values[15].channel, 302);
}
#[test]
fn offers_keep_their_original_order_when_a_new_plan_is_created() {
    let old_plan = plan(vec![992, 992], vec![]);
    let old = offer(&old_plan, ProtocolVersion::Pv2);
    drop(old_plan);
    let new_plan = plan(vec![993, 994], vec![]);
    let new = offer(&new_plan, ProtocolVersion::Pv2);
    let data = report(MeasurementFormat::Extended, vec![20, 21, 22], vec![]);
    let original = old.short_term_readings(&data).unwrap();
    assert_eq!(
        original
            .iter()
            .map(|reading| reading.channel)
            .collect::<Vec<_>>(),
        vec![991, 992, 992]
    );
    assert_eq!(original[2].rss_code, Some(22));
    assert_eq!(new.short_term_readings(&data).unwrap()[1].channel, 993);
}
#[test]
fn long_and_short_term_modes_reject_unrequested_measurement_families() {
    let long = MacaPlan::try_new(
        serving(),
        ProtocolVersion::Pv2,
        MacaStatus::LongTerm,
        triggers(),
        vec![],
        vec![],
    )
    .unwrap();
    assert!(
        offer(&long, ProtocolVersion::Pv2)
            .short_term_readings(&report(MeasurementFormat::Extended, vec![20], vec![]))
            .is_err()
    );
    let short = MacaPlan::try_new(
        serving(),
        ProtocolVersion::Pv2,
        MacaStatus::ShortTerm,
        triggers(),
        vec![],
        vec![],
    )
    .unwrap();
    let data = MacaReport {
        measurements: ReportMeasurements::Extended(Measurements {
            long_term: Some(LongTermMeasurement {
                wer: 1,
                ber: 1,
                rss: 20,
                full: false,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(
        offer(&short, ProtocolVersion::Pv2)
            .short_term_readings(&data)
            .is_err()
    );
    assert!(
        offer(&long, ProtocolVersion::Pv2)
            .short_term_readings(&data)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn oversized_lists_duplicate_other_bands_and_serving_channel_aliases_reject() {
    assert!(
        MacaPlan::try_new(
            serving(),
            ProtocolVersion::Pv2,
            MacaStatus::Both,
            triggers(),
            vec![991],
            vec![]
        )
        .is_err()
    );
    let list = HyperbandChannels {
        hyperband: Hyperband::Pcs1900,
        channels: vec![100],
    };
    assert!(
        MacaPlan::try_new(
            serving(),
            ProtocolVersion::Pv2,
            MacaStatus::Both,
            triggers(),
            vec![],
            vec![list.clone(), list]
        )
        .is_err()
    );
    assert!(
        MacaPlan::try_new(
            serving(),
            ProtocolVersion::Pv2,
            MacaStatus::Both,
            triggers(),
            (1..=16).collect(),
            vec![]
        )
        .is_err()
    );
}

#[test]
fn a_classic_bmi_keeps_extended_handsets_on_basic_reporting() {
    let plan = MacaPlan::try_new(
        serving(),
        ProtocolVersion::Pv1,
        MacaStatus::Both,
        triggers(),
        vec![992],
        vec![],
    )
    .unwrap();
    assert!(
        MacaBroadcast::decode(&plan.messages()[0])
            .unwrap()
            .enhanced
            .is_none()
    );
    let context = offer(&plan, ProtocolVersion::Pv2);
    assert_eq!(context.format(), MeasurementFormat::Basic);
    assert_eq!(
        context
            .short_term_readings(&report(MeasurementFormat::Basic, vec![20, 21], vec![]))
            .unwrap()
            .len(),
        2
    );
    assert!(
        context
            .short_term_readings(&report(MeasurementFormat::Extended, vec![20, 21], vec![]))
            .is_err()
    );
}

#[test]
fn selected_multi_hyperband_control_overrides_basic_triggers_and_status() {
    let basic = MacaBroadcast {
        status: MacaStatus::Both,
        triggers: ReportTriggers {
            registration: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut multi = MacaBroadcast {
        kind: BroadcastKind::MultiHyperband,
        status: MacaStatus::ShortTerm,
        triggers: ReportTriggers {
            origination: true,
            ..Default::default()
        },
        other_channels: vec![HyperbandChannels {
            hyperband: Hyperband::Pcs1900,
            channels: vec![100],
        }],
        ..Default::default()
    };
    let plan = MacaPlan::from_broadcasts(
        serving(),
        ProtocolVersion::Pv2,
        basic.clone(),
        Some(multi.clone()),
    )
    .unwrap();
    assert!(
        plan.offer(AccessTrigger::Registration, ProtocolVersion::Pv2, |_, _| {
            true
        })
        .unwrap()
        .is_none()
    );
    assert!(
        plan.offer(AccessTrigger::Origination, ProtocolVersion::Pv2, |_, _| {
            true
        })
        .unwrap()
        .is_some()
    );
    assert!(
        plan.offer(AccessTrigger::Registration, ProtocolVersion::Pv2, |_, _| {
            false
        })
        .unwrap()
        .is_some()
    );
    multi.status = MacaStatus::Disabled;
    let plan =
        MacaPlan::from_broadcasts(serving(), ProtocolVersion::Pv2, basic, Some(multi)).unwrap();
    assert!(
        plan.offer(AccessTrigger::Origination, ProtocolVersion::Pv2, |_, _| {
            true
        })
        .unwrap()
        .is_none()
    );
}
