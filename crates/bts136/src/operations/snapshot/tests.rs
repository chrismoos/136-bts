use super::*;
use crate::{
    core::{messaging::LocalMessaging, registry::ProvisionedSubscriber, store::StateStore},
    link::rach::MobileIdentity,
    scheduler::paging::PagingAllocation,
};
fn sms() -> SmsRuntime {
    let mut store = StateStore::open(":memory:").unwrap();
    store
        .reconcile(&[
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(1),
                number: "1001".into(),
            },
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(2),
                number: "1002".into(),
            },
        ])
        .unwrap();
    let config = crate::config::Config::from_yaml(
        "radio:\n  kind: noop\ncell:\n  band: cellular800\n  channel: 1\n  sid: 42\n  dvcc: 1\n",
    )
    .unwrap();
    let messages = crate::runtime::serving::messages(&config);
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) = &messages[0] else {
        panic!()
    };
    let allocation = PagingAllocation::try_new(structure).unwrap();
    SmsRuntime::try_new(
        LocalMessaging::from_store(store).unwrap(),
        allocation,
        "operations-test".into(),
    )
    .unwrap()
}
fn request(text: &str) -> proto::SendSmsRequest {
    proto::SendSmsRequest {
        request_id: "one-message".into(),
        sender_number: "1001".into(),
        recipient_number: "1002".into(),
        text: text.into(),
    }
}
#[test]
fn operator_sms_is_durable_idempotent_and_remains_queued_for_offline_recipient() {
    let mut runtime = sms();
    let first = send_sms(
        &mut runtime,
        request("Hello TDMA"),
        crate::l3::sms::DisplayTime::Default,
    )
    .unwrap();
    let duplicate = send_sms(
        &mut runtime,
        request("Hello TDMA"),
        crate::l3::sms::DisplayTime::Default,
    )
    .unwrap();
    assert_eq!(first.sms_id, duplicate.sms_id);
    let queued = runtime
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(&queued[0].sms_pdu[..4], &[0x00, 0x00, 0x09, 0x08]);

    let conflict = send_sms(
        &mut runtime,
        request("Different"),
        crate::l3::sms::DisplayTime::Default,
    )
    .unwrap_err();
    assert_eq!(conflict.code(), tonic::Code::AlreadyExists);
    let messages = runtime.operation_messages(200).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text.as_deref(), Some("Hello TDMA"));
    assert_eq!(
        messages[0].phase(),
        crate::operations::proto::SmsPhase::Queued
    );
    assert_eq!(messages[0].recipient, "1002");
    assert!(!messages[0].relay_accepted);
    runtime.advance(13000).unwrap();
    assert_eq!(
        runtime.operation_messages(200).unwrap()[0].phase(),
        crate::operations::proto::SmsPhase::Queued
    );
}

#[test]
fn message_history_pages_stay_stable_under_an_id_anchor() {
    let mut runtime = sms();
    for sequence in 0..5 {
        let mut request = request(&format!("Message {sequence}"));
        request.request_id = format!("history-{sequence}");
        send_sms(&mut runtime, request, crate::l3::sms::DisplayTime::Default).unwrap();
    }
    let store = runtime.core().store();
    let (first, total) = store
        .operation_messages_page(&crate::core::store::MessageFilter::default(), 0, 2)
        .unwrap();
    assert_eq!(total, 5);
    assert_eq!(
        first
            .iter()
            .map(|message| message.text.as_deref())
            .collect::<Vec<_>>(),
        [Some("Message 4"), Some("Message 3")]
    );
    let anchor = crate::core::store::MessageFilter {
        before_id: Some(first[0].id + 1),
        ..Default::default()
    };
    let (second, total) = store.operation_messages_page(&anchor, 2, 2).unwrap();
    assert_eq!(total, 5);
    assert_eq!(
        second
            .iter()
            .map(|message| message.text.as_deref())
            .collect::<Vec<_>>(),
        [Some("Message 2"), Some("Message 1")]
    );
    assert!(second.iter().all(|message| message.id < first[1].id));
    let queued = crate::core::store::MessageFilter {
        phase: Some(crate::operations::proto::SmsPhase::Queued),
        number: Some("1002".into()),
        ..Default::default()
    };
    assert_eq!(store.operation_messages_page(&queued, 0, 2).unwrap().1, 5);
    assert_eq!(store.queued_message_count().unwrap(), 5);
}

#[test]
fn operator_sms_can_request_invoke_display_time() {
    let config = crate::config::Config::from_yaml("radio:\n  kind: noop\ncell:\n  band: cellular800\n  channel: 1\n  sid: 42\n  dvcc: 1\nsms:\n  operator_display_time: invoke\n").unwrap();
    let mut runtime = sms();
    send_sms(
        &mut runtime,
        request("Read later"),
        config.sms.display_time.wire(),
    )
    .unwrap();
    let queued = runtime
        .core()
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(&queued[0].sms_pdu[..4], &[0x00, 0x00, 0x09, 0x10]);
}

#[test]
fn omitted_sms_display_time_defaults_to_invoke_and_default_remains_selectable() {
    let base =
        "radio:\n  kind: noop\ncell:\n  band: cellular800\n  channel: 1\n  sid: 42\n  dvcc: 1\n";
    for (suffix, expected) in [
        ("", crate::l3::sms::DisplayTime::Invoke),
        ("sms: {}\n", crate::l3::sms::DisplayTime::Invoke),
        (
            "sms:\n  operator_display_time: default\n",
            crate::l3::sms::DisplayTime::Default,
        ),
        (
            "sms:\n  operator_display_time: temporary\n",
            crate::l3::sms::DisplayTime::Temporary,
        ),
    ] {
        let config = crate::config::Config::from_yaml(&format!("{base}{suffix}")).unwrap();
        assert_eq!(config.sms.display_time.wire(), expected);
    }
}

#[test]
fn radio_status_publication_does_not_wait_for_the_sms_database() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "136bts-publication-{}-{stamp}.sqlite3",
        std::process::id()
    ));
    let mut config = crate::config::Config::from_yaml(
        "radio:\n  kind: noop\ncell:\n  band: cellular800\n  channel: 1\n  sid: 42\n  dvcc: 1\n",
    )
    .unwrap();
    config.core.state_path = path.clone();
    let store = StateStore::open(&path).unwrap();
    let prepared = config.prepare().unwrap();
    let broadcasts = crate::runtime::serving::messages(&config);
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) = &broadcasts[0] else {
        panic!()
    };
    let allocation = PagingAllocation::try_new(structure).unwrap();
    let sms = SmsRuntime::try_new(
        LocalMessaging::from_store(store).unwrap(),
        allocation,
        "publication".into(),
    )
    .unwrap();
    let voice = VoiceRuntime::try_new(
        prepared.traffic,
        allocation,
        1,
        config.cell.traffic_power_level,
        &config.calls,
    )
    .unwrap();
    let idle = crate::runtime::idle_queries::IdleQueries::new(allocation);
    let (_hub, mut interface) = crate::operations::Hub::new(&config);
    let lock = rusqlite::Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let (done, completion) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        interface.publish(
            &sms,
            &voice,
            &idle,
            Observation {
                hardware_tick: 0,
                health: Default::default(),
                state: crate::radio::timed_tx::State::Running,
            },
            RuntimeStatus {
                position: crate::scheduler::broadcast::Position {
                    hyperframe_counter: 0,
                    superframe: crate::scheduler::broadcast::Superframe::Primary,
                },
                now_ms: 0,
                submitted: 0,
                completed_registrations: 0,
                skipped_slots: 0,
                rx_discontinuities: 0,
                queue_depth: 4,
                access_bursts: 0,
                traffic_bursts: 0,
                rx_power_dbfs: None,
                last_frequency_offset_hz: None,
                last_sync_correlation: None,
            },
        );
        done.send(()).unwrap();
    });
    let published = completion.recv_timeout(Duration::from_millis(250));
    lock.execute_batch("COMMIT").unwrap();
    worker.join().unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(
        published.is_ok(),
        "Status publication waited for SQLite history"
    );
}
#[test]
fn registration_period_shows_superframes_and_time() {
    assert_eq!(registration_period(10), "10 superframes (6 s)");
    assert_eq!(registration_period(940), "940 superframes (10 min 1 s)");
    assert_eq!(registration_period(5625), "5625 superframes (1 h)");
}
