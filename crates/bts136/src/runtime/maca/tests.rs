use super::*;
use crate::{
    config::Config,
    core::maca::AccessTrigger,
    l3::{
        designation::AssignedSlots,
        maca::{MacaReport, ReportMeasurements},
    },
    runtime::access::maca::MacaSource,
};
fn content(channel: u16) -> BroadcastContent {
    let config = Config::from_yaml(&format!(
        "{}\nmaca:\n  current_channels: [{channel}]\n",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/two-phones.yaml"
        ))
    ))
    .unwrap();
    BroadcastContent::try_new(
        AssignedSlots::Full14,
        crate::runtime::serving::messages(&config),
        config.maca_plan().unwrap().unwrap().messages().to_vec(),
    )
    .unwrap()
}
fn serving() -> ControlChannel {
    ControlChannel {
        hyperband: Hyperband::Cellular800,
        channel: 991,
        dvcc: 1,
    }
}
fn companion() -> MacaCompanion {
    MacaCompanion {
        source: MacaSource::Broadcast(AccessTrigger::Registration),
        version: Some(ProtocolVersion::Pv2),
        report: MacaReport {
            measurements: ReportMeasurements::Extended(crate::l3::maca::Measurements {
                current: Some(vec![20, 21]),
                ..Default::default()
            }),
            ..Default::default()
        },
    }
}
#[test]
fn actual_broadcast_offer_maps_channel_order_without_handset_band_guesses() {
    let content = content(992);
    let mut runtime = MacaRuntime {
        serving: serving(),
        history: vec![],
    };
    runtime.retain(&content).unwrap();
    let Association::Observed(result) = runtime.associate_with_capabilities(
        &content,
        &companion(),
        Some(ProtocolVersion::Pv2),
        None,
    ) else {
        panic!()
    };
    assert_eq!(
        result
            .readings
            .iter()
            .map(|reading| (reading.channel, reading.rss_code))
            .collect::<Vec<_>>(),
        [(991, Some(20)), (992, Some(21))]
    );
}
#[test]
fn equal_length_historical_lists_cannot_relabel_a_delayed_report() {
    let original = content(992);
    let changed = content(993);
    let mut runtime = MacaRuntime {
        serving: serving(),
        history: vec![],
    };
    runtime.retain(&original).unwrap();
    runtime.retain(&changed).unwrap();
    assert_eq!(
        runtime.associate_with_capabilities(
            &changed,
            &companion(),
            Some(ProtocolVersion::Pv2),
            None
        ),
        Association::Unmapped("Ambiguous historical target lists")
    );
}
#[test]
fn unsolicited_queue_reports_and_incompatible_formats_are_not_measurements() {
    let content = content(992);
    let mut runtime = MacaRuntime {
        serving: serving(),
        history: vec![],
    };
    runtime.retain(&content).unwrap();
    let mut report = companion();
    report.source = MacaSource::QueueNotification;
    assert_eq!(
        runtime.associate_with_capabilities(&content, &report, Some(ProtocolVersion::Pv2), None),
        Association::Unmapped("No matching queued request")
    );
    report.source = MacaSource::Broadcast(AccessTrigger::Registration);
    assert_eq!(
        runtime.associate_with_capabilities(&content, &report, Some(ProtocolVersion::Pv1), None),
        Association::Unmapped("Report does not match an advertised offer")
    );
}

#[test]
fn durable_predecessor_history_is_used_after_restart() {
    let mut store = StateStore::open(":memory:").unwrap();
    let key = CellKey::try_new(crate::phy::channel::Band::Cellular800, 991).unwrap();
    let original = content(992);
    let changed = content(993);
    let flags = crate::link::broadcast::BroadcastSignaling {
        changes: crate::link::broadcast::BroadcastChanges {
            fbcch: false,
            ebcch: false,
        },
        notification: false,
    };
    let old = store
        .prepare_broadcast_revision(key, &original.storage_bytes().unwrap(), flags, 10)
        .unwrap();
    store.confirm_broadcast_revision(&old, 20).unwrap();
    store
        .prepare_broadcast_revision(key, &changed.storage_bytes().unwrap(), flags, 30)
        .unwrap();
    let runtime = MacaRuntime::try_new(&store, key, serving(), &changed).unwrap();
    assert_eq!(
        runtime.associate_with_capabilities(
            &changed,
            &companion(),
            Some(ProtocolVersion::Pv2),
            None
        ),
        Association::Unmapped("Ambiguous historical target lists")
    );
}
