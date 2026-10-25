use super::*;
use crate::l3::registration::{RegistrationOptions, RegistrationRequest};
const MIXED_STOP_SLOTS: usize = 1500;
const MIXED_DURATION_SECONDS: u64 = 10;
const MAX_INTERRUPTION_FRAMES: usize = 32;
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn fragmented_message_waiting_confirms_while_two_silent_arq_registrations_remain_pending() {
    let _paced = PACED_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = Database(std::env::temp_dir().join(format!(
        "136bts-mwi-arq-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = db.0.clone();
    config.subscribers[0].number = "12345678901234567890123456789012".into();
    config.subscribers[1].number = "22345678901234567890123456789012".into();
    config.subscribers.push(crate::config::SubscriberConfig {
        identity: crate::config::IdentityConfig::Min34 { value: 3 },
        number: "1003".into(),
    });
    let messages = messages(&config);
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) = &messages[0] else {
        panic!()
    };
    let allocation = PagingAllocation::try_new(structure).unwrap();
    let ordinary = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 15,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    let long = RegistrationRequest {
        registration_type: RegistrationType::PowerUp,
        scm: 15,
        protocol_version: ProtocolVersion::Pv5,
        options: RegistrationOptions {
            pfc: Some(7),
            sid_report: Some(42),
            number_request: Some(1),
            ..Default::default()
        },
    }
    .encode()
    .unwrap();
    let mut packets: Vec<_> = access_packets(MobileIdentity::Min34(3), &ordinary)
        .into_iter()
        .map(|(offset, packet)| (ACCESS_BLOCK + offset, packet))
        .collect();
    let quiet = arq_reuse_policy(&config).unwrap().quiet_blocks();
    for (identity, start) in [
        (1, quiet + ACCESS_BLOCK),
        (2, quiet + SECOND_REGISTRATION_BLOCK),
    ] {
        packets.extend(
            access_packets(MobileIdentity::Min34(identity), &long)
                .into_iter()
                .map(|(offset, packet)| (start + offset, packet)),
        );
    }
    let mut idle = Handsets::new(ReplyOn::Secondary, allocation, QueryMode::MessageWaiting);
    idle.waiting =
        waiting::Observer::with_identities([MobileIdentity::Min34(3), MobileIdentity::Min34(4)]);
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        tx_observation_offset_ticks: 0,
        epoch: None,
        tx: Vec::new(),
        slot_count: 0,
        stop_slots: MIXED_STOP_SLOTS,
        underflow_after_slot: None,
        stall_after_slot: None,
        realtime_epoch: Some(Instant::now()),
        handset_segmented: None,
        handset_sms: None,
        handset_voice: None,
        handset_arq: Some(arq_loop::Handsets::new(arq_loop::Mode::Silent)),
        handset_idle: Some(idle),
        traffic_packets: Vec::new(),
        packets,
        tx_active: false,
        rx_active: false,
        shutdown: shutdown.clone(),
    }));
    let prepared = PreparedRadio::from_radio(&config, Box::new(Device(state.clone()))).unwrap();
    let (hub, interface) = crate::operations::Hub::new(&config);
    let operator_hub = hub.clone();
    let operator_stop = shutdown.clone();
    let operator_state = state.clone();
    let operator = std::thread::spawn(move || {
        while !operator_stop.load(Ordering::Relaxed) {
            let ready = {
                let state = operator_state.lock().unwrap();
                state
                    .handset_arq
                    .as_ref()
                    .is_some_and(|arq| arq.started_identities.len() == 2)
                    && state.handset_idle.as_ref().is_some_and(|idle| {
                        idle.registration_seen.contains(&MobileIdentity::Min34(3))
                    })
            };
            if ready {
                let (response, result) = tokio::sync::oneshot::channel();
                operator_hub
                    .commands
                    .try_send(PendingCommand {
                        command: Command::MessageWaiting(proto::SendMessageWaitingRequest {
                            subscriber_id: Some(proto::SubscriberIdentity {
                                kind: 2,
                                value: 3,
                                esn: None,
                            }),
                            messages: waiting::message()
                                .entries
                                .iter()
                                .map(|entry| proto::WaitingMessage {
                                    kind: u32::from(entry.kind),
                                    count: u32::from(entry.count),
                                })
                                .collect(),
                            display: waiting::message().display,
                        }),
                        response,
                    })
                    .unwrap();
                assert!(result.blocking_recv().unwrap().is_ok());
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    });
    let result = run_prepared_with_interface(
        &config,
        prepared,
        &shutdown,
        Some(Duration::from_secs(MIXED_DURATION_SECONDS)),
        Some(interface),
    );
    shutdown.store(true, Ordering::Relaxed);
    assert!(operator.join().unwrap());
    let summary = result.unwrap();
    assert_eq!(summary.completed_registrations, 1);
    assert!(summary.stop_acknowledged);
    let snapshot = hub.snapshot();
    let phone = snapshot
        .subscribers
        .iter()
        .find(|phone| phone.number == "1003")
        .unwrap();
    assert!(phone.registered);
    assert_eq!(phone.last_registration_superframe, Some(0));
    assert!(
        phone
            .measurements
            .iter()
            .any(|metric| metric.name == "Idle Message Waiting"
                && metric.value.as_deref() == Some("Answered")),
        "{:?}",
        phone.measurements
    );
    assert!(
        !snapshot
            .air_messages
            .iter()
            .any(|message| message.name.contains("ARQ") && message.status.contains("Expired"))
    );
    let indication = snapshot
        .air_messages
        .iter()
        .find(|message| message.name == "Message Waiting" && message.direction == "Forward")
        .unwrap();
    let confirmation = snapshot
        .air_messages
        .iter()
        .find(|message| message.name == "SPACH Confirmation" && message.identity == "Min34(3)")
        .unwrap();
    assert!(indication.sequence < confirmation.sequence);
    for identity in ["Min34(1)", "Min34(2)"] {
        assert!(
            snapshot
                .air_messages
                .iter()
                .any(|message| message.name == "ARQ BEGIN"
                    && message.identity == identity
                    && message.sequence < indication.sequence)
        );
        assert!(snapshot.air_messages.iter().any(|message| matches!(
            message.name.as_str(),
            "ARQ BEGIN" | "ARQ CONTINUE"
        ) && message.identity == identity
            && message.sequence > confirmation.sequence));
    }
    let state = state.lock().unwrap();
    let idle = state.handset_idle.as_ref().unwrap();
    let arq = state.handset_arq.as_ref().unwrap();
    assert!(idle.responded[0] && idle.secondary[0] && idle.primary[0].is_some());
    let mut bodies = std::collections::HashMap::new();
    for message in &arq.messages {
        assert_eq!(message.channel, crate::link::spach::Channel::Arch);
        let options = crate::l3::registration::AcceptOptions::decode(&message.message).unwrap();
        assert_eq!(options.pfc, Some(0));
        let number = match message.identity {
            MobileIdentity::Min34(1) => "12345678901234567890123456789012",
            MobileIdentity::Min34(2) => "22345678901234567890123456789012",
            _ => panic!(),
        };
        assert_eq!(
            options.directory_address.unwrap().address,
            crate::l3::address::AddressValue::Tbcd(number.into())
        );
        assert!(options.ignored_bits.is_empty() && options.unparsed_bits.is_empty());
        if let Some(previous) = bodies.insert(message.identity, message.message.clone()) {
            assert_eq!(previous, message.message);
        }
    }
    assert_eq!(bodies.len(), 2);
    assert_eq!(arq.applications, 2);
    assert_eq!(arq.statuses, 0);
    assert_eq!(arq.confirmed, 0);
    assert!(arq.echoes.len() >= 4);
    let response_block = idle.response_blocks[0].unwrap();
    for identity in [MobileIdentity::Min34(1), MobileIdentity::Min34(2)] {
        assert!(arq.last_data_blocks[&identity] > response_block);
    }
    assert!(
        arq.max_interruption <= MAX_INTERRUPTION_FRAMES,
        "{}",
        arq.max_interruption
    );
    assert!(!state.tx_active && !state.rx_active);
}
