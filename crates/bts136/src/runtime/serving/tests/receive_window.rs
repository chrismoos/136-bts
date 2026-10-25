use super::*;
const STALE_CLOCK_SECONDS: u64 = 2;
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn stale_rx_metadata_is_discarded_without_stalling_the_transmit_timeline() {
    let _guard = PACED_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let database = Database(std::path::PathBuf::from(format!(
        "/var/tmp/136bts-stale-rx-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = database.0.clone();
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        tx_observation_offset_ticks: STALE_CLOCK_SECONDS * TICKS,
        epoch: None,
        tx: Vec::new(),
        slot_count: 0,
        stop_slots: STOP_SLOTS,
        underflow_after_slot: None,
        stall_after_slot: None,
        realtime_epoch: None,
        handset_segmented: None,
        handset_sms: None,
        handset_voice: None,
        handset_idle: None,
        handset_arq: None,
        traffic_packets: Vec::new(),
        packets: Vec::new(),
        tx_active: false,
        rx_active: false,
        shutdown: shutdown.clone(),
    }));
    let prepared = PreparedRadio::from_radio(&config, Box::new(Device(state.clone()))).unwrap();
    let (hub, interface) = crate::operations::Hub::new(&config);
    let summary = run_prepared_with_interface(
        &config,
        prepared,
        &shutdown,
        Some(Duration::from_secs(5)),
        Some(interface),
    )
    .unwrap();
    assert!(summary.submitted_slots >= STOP_SLOTS as u64);
    assert_eq!(summary.completed_registrations, 0);
    assert!(summary.stop_acknowledged);
    let snapshot = hub.snapshot();
    assert!(
        snapshot
            .radio
            .iter()
            .any(|metric| metric.name == "RX discontinuities"
                && metric.value.as_deref() == Some("1"))
    );
    let state = state.lock().unwrap();
    assert!(!state.tx_active && !state.rx_active);
    assert!(state.slot_count >= STOP_SLOTS);
}
