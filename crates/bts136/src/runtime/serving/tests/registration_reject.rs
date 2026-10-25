use super::*;

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn unprovisioned_registration_returns_typed_reject_over_iq_and_logs_default_retry() {
    registration_reply(false);
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn unprovisioned_registration_is_accepted_by_default_and_visible_without_a_subscriber() {
    registration_reply(true);
}

fn registration_reply(allow_unknown: bool) {
    let _guard = PACED_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = Database(std::env::temp_dir().join(format!(
        "136bts-reject-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = db.0.clone();
    config.subscribers.remove(0);
    if !allow_unknown {
        config.registration.allow_unknown = false;
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        tx_observation_offset_ticks: 0,
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
        packets: vec![(ACCESS_BLOCK, packet())],
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
    assert_eq!(summary.completed_registrations, 1);
    assert!(summary.stop_acknowledged);
    let snapshot = hub.snapshot();
    assert!(
        snapshot
            .subscribers
            .iter()
            .all(|subscriber| !subscriber.registered)
    );
    assert!(
        snapshot
            .observed_phones
            .iter()
            .any(|phone| phone.identity == "Min34(1)")
    );
    assert!(
        snapshot
            .air_messages
            .iter()
            .any(|message| message.direction == "Forward"
                && message.name == "Registration response"
                && if allow_unknown {
                    message.decoded.contains("Accept")
                } else {
                    message.decoded.contains("UnknownMsid")
                        && message.decoded.contains("default=2048 SF")
                })
    );
    assert_eq!(
        crate::core::store::StateStore::open(&db.0)
            .unwrap()
            .enabled_subscribers()
            .unwrap()
            .len(),
        1
    );
    let state = state.lock().unwrap();
    assert!(!state.tx_active && !state.rx_active);
    let samples: Vec<_> = state
        .tx
        .iter()
        .map(|sample| Symbol {
            i: f64::from(sample.re),
            q: f64::from(sample.im),
        })
        .collect();
    let controls = decode_forward_control(&samples, RATE as f64, LogicalChannel::Spach, 1).unwrap();
    let mut receiver = SpachReceiver::default();
    let mut responses = 0;
    for burst in controls {
        if let Ok(Some(message)) = receiver.push(&burst.fields.block.information)
            && message.channel == crate::link::spach::Channel::Arch
            && message.identity == MobileIdentity::Min34(1)
        {
            if allow_unknown {
                assert_eq!(
                    crate::l3::registration::RegistrationResponse::decode_prefix(&message.message)
                        .unwrap()
                        .message,
                    RegistrationResponse::Accept
                );
            } else {
                let response = crate::l3::call_control::Response::decode(&message.message).unwrap();
                assert_eq!(
                    response,
                    crate::l3::call_control::Response::registration_reject(
                        crate::l3::registration::RejectCause::UnknownMsid
                    )
                );
                assert_eq!(
                    response.registration_retry(1).unwrap(),
                    crate::l3::registration::RegistrationRetry::Superframes(2048)
                );
            }
            responses += 1;
        }
    }
    assert_eq!(responses, 1);
}
