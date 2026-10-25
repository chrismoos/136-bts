use super::*;
use crate::{
    core::store::{StateStore, broadcast::CellKey},
    l3::broadcast::MandatoryBroadcast,
};

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn changed_startup_broadcast_crosses_iq_and_confirms_before_registration() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let database = Database(std::env::temp_dir().join(format!(
        "136bts-broadcast-recovery-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = database.0.clone();
    config.cell.network_name = Some("136BTS".into());
    let key = CellKey::try_new(config.cell.band.band(), config.cell.channel).unwrap();
    let original_sid = config.cell.sid;
    let mut first_revision = None;
    for restarting in [false, true] {
        if restarting {
            config.cell.sid += 1;
            config.cell.operator = Some(crate::config::OperatorIdentity { soc: 0, bsmc: 0 });
            config.registration.area = Some(42);
            config.registration.period_superframes = Some(188);
            config.cell.neighbors = (2..=25)
                .map(crate::l3::neighbors::tests::neighbor)
                .collect();
        }
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(Backend {
            sample: 0,
            tx_observation_offset_ticks: 0,
            epoch: None,
            tx: Vec::new(),
            slot_count: 0,
            stop_slots: if restarting { 12 } else { 4 } * PHYSICAL_SLOTS as usize,
            underflow_after_slot: None,
            stall_after_slot: None,
            realtime_epoch: None,
            handset_segmented: None,
            handset_sms: None,
            handset_voice: None,
            handset_arq: None,
            handset_idle: None,
            traffic_packets: Vec::new(),
            packets: if restarting {
                vec![(ACCESS_BLOCK, packet()), (200, packet())]
            } else {
                vec![]
            },
            tx_active: false,
            rx_active: false,
            shutdown: shutdown.clone(),
        }));
        let prepared = PreparedRadio::from_radio(&config, Box::new(Device(state.clone()))).unwrap();
        let summary = run_prepared_with_interface(
            &config,
            prepared,
            &shutdown,
            Some(Duration::from_secs(10)),
            None,
        )
        .unwrap();
        assert!(summary.stop_acknowledged);
        assert_eq!(summary.completed_registrations, u64::from(restarting));
        let store = StateStore::open(&database.0).unwrap();
        let recovery = store.broadcast_recovery(key).unwrap();
        assert!(recovery.pending.is_none());
        let confirmed = recovery.confirmed.unwrap();
        let stored =
            crate::scheduler::broadcast::BroadcastContent::from_storage_bytes(confirmed.contents())
                .unwrap();
        let MandatoryBroadcast::SystemIdentity(identity) = &stored.messages()[4] else {
            panic!()
        };
        assert_eq!(identity.sid, config.cell.sid);
        assert_eq!(identity.network_name.as_deref(), Some("136BTS"));
        assert!(stored.time_zone().is_some());
        if restarting {
            assert!(
                store
                    .broadcast_predecessor(&confirmed)
                    .unwrap()
                    .unwrap()
                    .same_revision(first_revision.as_ref().unwrap())
            );
            let state = state.lock().unwrap();
            assert!(!state.tx_active && !state.rx_active);
            let samples: Vec<_> = state
                .tx
                .iter()
                .map(|sample| Symbol {
                    i: sample.re as f64,
                    q: sample.im as f64,
                })
                .collect();
            let bursts =
                decode_forward_control(&samples, RATE as f64, LogicalChannel::Fbcch, 0).unwrap();
            let mut receiver = crate::link::broadcast::FbcchReceiver::default();
            let mut identities = Vec::new();
            for burst in bursts {
                for message in receiver
                    .push(
                        burst
                            .fields
                            .block
                            .information
                            .as_slice()
                            .try_into()
                            .unwrap(),
                    )
                    .unwrap()
                {
                    if let Ok(decoded) = crate::l3::broadcast::decode_prefix(&message)
                        && let MandatoryBroadcast::SystemIdentity(identity) = decoded.message
                        && identities.last() != Some(&identity.sid)
                    {
                        identities.push(identity.sid);
                    }
                }
            }
            assert_eq!(identities, [original_sid, config.cell.sid]);
            let bursts = decode_forward_control(
                &samples,
                RATE as f64,
                LogicalChannel::Ebcch,
                config.cell.dvcc,
            )
            .unwrap();
            let mut times = Vec::new();
            let mut neighbor_lists = Vec::new();
            let mut receiver = crate::link::broadcast::EbcchReceiver::default();
            let MandatoryBroadcast::DcchStructure(structure) = &stored.messages()[0] else {
                panic!()
            };
            for burst in bursts {
                let slot = (burst.slot_start_sample
                    / ((crate::phy::traffic::SLOT_BITS / 2) * SAMPLES_PER_SYMBOL) as f64)
                    .round() as u64;
                let sf = slot / PHYSICAL_SLOTS;
                let phase = (slot % PHYSICAL_SLOTS) / 3;
                if sf < 4
                    || phase < u64::from(structure.fbcch_slots)
                    || phase >= u64::from(structure.fbcch_slots + structure.ebcch_slots)
                {
                    continue;
                }
                if let Ok(packets) = receiver.push(
                    burst
                        .fields
                        .block
                        .information
                        .as_slice()
                        .try_into()
                        .unwrap(),
                ) {
                    for packet in packets {
                        if let Ok(list) = crate::l3::neighbors::decode(&packet) {
                            neighbor_lists.push(list);
                        }
                    }
                }
                if let Ok((cycle, packets)) = crate::link::broadcast::decode_ebcch_begin(
                    burst
                        .fields
                        .block
                        .information
                        .as_slice()
                        .try_into()
                        .unwrap(),
                ) {
                    assert_eq!(cycle, 16);
                    for packet in packets {
                        if let Ok(time) = crate::l3::time_date::TimeAndDate::decode(&packet) {
                            times.push(time);
                        }
                    }
                }
            }
            assert!(!times.is_empty());
            assert!(
                neighbor_lists
                    .iter()
                    .any(|list| list == &config.cell.neighbors)
            );
            assert_eq!(
                crate::l3::neighbors::decode(stored.neighbor_packet().unwrap()).unwrap(),
                config.cell.neighbors
            );
            let now = super::super::super::system_time::sample(0).unwrap();
            assert!(times.iter().all(|time| time.time_zone == now.time_zone
                && time.seconds_since_1980.abs_diff(now.seconds_since_1980) <= 5));
        } else {
            first_revision = Some(confirmed);
        }
    }
}
