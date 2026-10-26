use super::*;
use crate::l3::{
    address::{AddressInfo, AddressValue},
    relay::{
        Direction, HigherLayerProtocol, RelayMessage, Teleservice,
        addressing::{AddressedRelay, RelayAddresses},
    },
    sms::{Metadata, SmsMessage, UserData},
    tsar::{self, AcknowledgmentWindow, MessagePlan},
};

const FIRST_SUBMISSION_SLOT: u64 = 20;
const FIRST_RELAY_ID: u8 = 11;
const TSAR_ID: u8 = 3;
const MAX_CMT_USER_OCTETS: usize = 254;
const SEGMENT_PAYLOAD_BUDGET: usize = 80;

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn generated_segmented_sms_submission_iq_is_durably_stored_and_acknowledged() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let database = Database(std::env::temp_dir().join(format!(
        "136bts-serving-tsar-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = database.0.clone();
    let payload = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 42,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(vec![b'x'; MAX_CMT_USER_OCTETS]),
    }
    .encode()
    .unwrap();
    assert!(payload.len() > crate::l3::relay::MAX_DATA_PAYLOAD_OCTETS);
    let MessagePlan::Segmented(plan) = tsar::plan_message(
        &payload,
        SEGMENT_PAYLOAD_BUDGET,
        TSAR_ID,
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
    )
    .unwrap() else {
        panic!()
    };
    let mut packets = vec![(ACCESS_BLOCK, packet_version(ProtocolVersion::Pv1))];
    let mut next_slot = FIRST_SUBMISSION_SLOT;
    for (index, segment) in plan.segments().iter().enumerate() {
        let message = AddressedRelay {
            message: RelayMessage::Data {
                transaction_id: FIRST_RELAY_ID + index as u8,
                protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
                payload: segment.encode(ProtocolVersion::Pv1).unwrap(),
            },
            addresses: RelayAddresses {
                destination: (index == 0).then(|| AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: AddressValue::Tbcd("1002".into()),
                }),
                ..Default::default()
            },
        }
        .encode(Direction::Reverse)
        .unwrap();
        let fragments = access_packets(MobileIdentity::Min34(1), &message);
        let end = fragments.last().unwrap().0;
        packets.extend(
            fragments
                .into_iter()
                .map(|(offset, waveform)| (next_slot + offset, waveform)),
        );
        next_slot += end + ACCESS_PATH_BLOCKS;
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        clock_sample: 0,
        tx_observation_offset_ticks: 0,
        epoch: None,
        tx: Vec::new(),
        slot_count: 0,
        stop_slots: STOP_SLOTS * 3,
        underflow_after_slot: None,
        stall_after_slot: None,
        realtime_epoch: None,
        handset_segmented: None,
        handset_sms: None,
        handset_voice: None,
        handset_arq: None,
        handset_idle: None,
        traffic_packets: Vec::new(),
        packets,
        tx_active: false,
        rx_active: false,
        shutdown: shutdown.clone(),
    }));
    let prepared = PreparedRadio::from_radio(&config, Box::new(Device(state.clone()))).unwrap();
    let summary = run_prepared(&config, prepared, &shutdown, Some(Duration::from_secs(5))).unwrap();
    assert_eq!(summary.completed_registrations, 1);
    assert!(summary.stop_acknowledged);
    let store = crate::core::store::StateStore::open(&database.0).unwrap();
    let queued = store.pending_for(MobileIdentity::Min34(2)).unwrap();
    assert_eq!(queued.len(), 1);
    let SmsMessage::Deliver {
        user_data: UserData::Ira(body),
        ..
    } = crate::l3::sms::decode_prefix(&queued[0].sms_pdu, Direction::Forward)
        .unwrap()
        .message
    else {
        panic!()
    };
    assert_eq!(body, vec![b'x'; MAX_CMT_USER_OCTETS]);
    let journal = store.active_incoming_journals(0, 2).unwrap().remove(0);
    assert_eq!(journal.revision as usize, plan.segments().len() + 1);
    assert_eq!(
        store.pending_incoming_responses(32).unwrap()[0]
            .response
            .status,
        tsar::Status::Received
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
    let mut accepted = Vec::new();
    for burst in controls {
        if let Ok(Some(message)) = receiver.push(&burst.fields.block.information)
            && message.channel == crate::link::spach::Channel::Smsch
            && message.identity == MobileIdentity::Min34(1)
            && let Ok(response) =
                crate::l3::relay::decode_prefix(&message.message, Direction::Forward)
            && let RelayMessage::Accept { transaction_id } = response.message
        {
            accepted.push(transaction_id);
        }
    }
    assert_eq!(
        accepted,
        (0..plan.segments().len())
            .map(|index| FIRST_RELAY_ID + index as u8)
            .collect::<Vec<_>>()
    );
}

const OBSERVE_SLOTS: usize = 12;
const WINDOW_SLOTS: usize = 24;
const FULL_EXCHANGE_STOP_SLOTS: usize = 4800;
const FINAL_RESPONSE_ID: u8 = 230;
const SECOND_REGISTRATION_SLOT: u64 = 12;
const SMS_CONFIRMATION: [u8; 2] = [15, 104];
const RETIREMENT_WAIT_MS: u64 = 256 * crate::runtime::timers::SUPERFRAME_MS;
const WALL_RETIREMENT_WAIT_MS: u64 = RETIREMENT_WAIT_MS + 4 * crate::runtime::timers::SUPERFRAME_MS;
const TRANSACTION_IDS: u8 = 8;
const REPEATED_EXCHANGES: u8 = 12;
enum RetirementProbe {
    AwaitingCompletion,
    AwaitingWallCompletion,
    WallDraining(usize),
    Draining(usize),
    Advanced,
}
pub(super) struct Handsets {
    retirement: Option<RetirementProbe>,
    receiver: SpachReceiver,
    arq: arq_loop::Handsets,
    last_sample: Option<f64>,
    parts: std::collections::BTreeMap<u8, Vec<u8>>,
    end_sequence: Option<u8>,
    received: Option<Vec<u8>>,
    source_received: bool,
    target_response_accepted: bool,
    source_accepts: Vec<u8>,
    source_segments: usize,
}
impl Handsets {
    fn new(source_segments: usize) -> Self {
        Self {
            retirement: None,
            receiver: SpachReceiver::default(),
            arq: arq_loop::Handsets::new(arq_loop::Mode::Normal),
            last_sample: None,
            parts: std::collections::BTreeMap::new(),
            end_sequence: None,
            received: None,
            source_received: false,
            target_response_accepted: false,
            source_accepts: Vec::new(),
            source_segments,
        }
    }
    pub(super) fn observe(&mut self, state: &mut Backend) -> Result<(), HalError> {
        if !state.slot_count.is_multiple_of(OBSERVE_SLOTS) {
            return Ok(());
        }
        self.arq.observe(state)?;
        let mut messages = std::mem::take(&mut self.arq.messages);
        let offset = state
            .tx
            .len()
            .saturating_sub(WINDOW_SLOTS * SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL);
        let samples: Vec<_> = state.tx[offset..]
            .iter()
            .map(|sample| Symbol {
                i: f64::from(sample.re),
                q: f64::from(sample.im),
            })
            .collect();
        for burst in decode_forward_control(&samples, RATE as f64, LogicalChannel::Spach, 1)
            .map_err(|error| HalError::from(error.to_string()))?
        {
            let first = offset as f64 + burst.slot_start_sample;
            if self.last_sample.is_some_and(|last| {
                first < last + (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL / 2) as f64
            }) {
                continue;
            }
            self.last_sample = Some(first);
            if let Ok(Some(message)) = self.receiver.push(&burst.fields.block.information) {
                messages.push(message);
            }
        }
        for message in messages {
            if message.channel == crate::link::spach::Channel::Pch
                && message.message == crate::l3::notification::sms_notification()
            {
                sms_loop::respond(state, message.identity, &SMS_CONFIRMATION);
                continue;
            }
            if message.channel != crate::link::spach::Channel::Smsch {
                continue;
            }
            let decoded = crate::l3::relay::decode_prefix(&message.message, Direction::Forward)
                .map_err(|error| HalError::from(error.to_string()))?;
            let (transaction_id, payload) = match decoded.message {
                RelayMessage::Accept { transaction_id } => {
                    if message.identity == MobileIdentity::Min34(1) {
                        self.source_accepts.push(transaction_id);
                    }
                    if message.identity == MobileIdentity::Min34(2)
                        && transaction_id == FINAL_RESPONSE_ID
                    {
                        self.target_response_accepted = true;
                    }
                    continue;
                }
                RelayMessage::Data {
                    transaction_id,
                    protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
                    payload,
                } => (transaction_id, payload),
                other => return Err(format!("unexpected segmented SMS response {other:?}").into()),
            };
            if let Ok(segment) = tsar::decode_segment(&payload, ProtocolVersion::Pv1) {
                if message.identity != MobileIdentity::Min34(2) {
                    return Err("segmented deliver sent to wrong handset".into());
                }
                let segment = segment.segment;
                if let Some(previous) = self
                    .parts
                    .insert(segment.sequence_number, segment.segment_data.clone())
                    && previous != segment.segment_data
                {
                    return Err("conflicting segment bytes".into());
                }
                if matches!(segment.kind, tsar::SegmentKind::End { .. }) {
                    self.end_sequence = Some(segment.sequence_number);
                }
                sms_loop::respond(
                    state,
                    message.identity,
                    &RelayMessage::Accept { transaction_id }
                        .encode(Direction::Reverse)
                        .unwrap(),
                );
                if let Some(end) = self.end_sequence
                    && (0..=end).all(|sequence| self.parts.contains_key(&sequence))
                    && self.received.is_none()
                {
                    self.received = Some(
                        (0..=end)
                            .flat_map(|sequence| self.parts[&sequence].clone())
                            .collect(),
                    );
                    let response = tsar::Response {
                        transaction_id: segment.transaction_id,
                        status: tsar::Status::Received,
                        acknowledgments: tsar::AcknowledgmentMap::try_new(vec![
                            true;
                            usize::from(end) + 1
                        ])
                        .unwrap(),
                        optional_services_support: None,
                    };
                    sms_loop::respond(
                        state,
                        message.identity,
                        &RelayMessage::Data {
                            transaction_id: FINAL_RESPONSE_ID,
                            protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
                            payload: response.encode(ProtocolVersion::Pv1).unwrap(),
                        }
                        .encode(Direction::Reverse)
                        .unwrap(),
                    );
                }
            } else {
                let response = tsar::decode_response(&payload, ProtocolVersion::Pv1)
                    .map_err(|error| HalError::from(error.to_string()))?;
                if message.identity != MobileIdentity::Min34(1)
                    || response.response.status != tsar::Status::Received
                {
                    return Err("unexpected source TSAR response".into());
                }
                self.source_received = true;
                sms_loop::respond(
                    state,
                    message.identity,
                    &RelayMessage::Accept { transaction_id }
                        .encode(Direction::Reverse)
                        .unwrap(),
                );
            }
        }
        if self.source_received
            && self.target_response_accepted
            && (0..self.source_segments).all(|index| {
                self.source_accepts
                    .contains(&(FIRST_RELAY_ID + index as u8))
            })
        {
            match &mut self.retirement {
                None => state.shutdown.store(true, Ordering::Relaxed),
                Some(probe @ RetirementProbe::AwaitingWallCompletion) => {
                    let slots = WALL_RETIREMENT_WAIT_MS as usize * RATE
                        / 1000
                        / (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL);
                    *probe = RetirementProbe::WallDraining(state.slot_count + slots);
                }
                Some(RetirementProbe::WallDraining(slot)) if state.slot_count >= *slot => {
                    state.shutdown.store(true, Ordering::Relaxed);
                }
                Some(probe @ RetirementProbe::AwaitingCompletion) => {
                    *probe = RetirementProbe::Draining(state.slot_count + OBSERVE_SLOTS * 2);
                }
                Some(RetirementProbe::Draining(slot)) if state.slot_count >= *slot => {
                    let jump_samples = (RATE as u64 * RETIREMENT_WAIT_MS / 1000) as usize;
                    let jump_slots = jump_samples / (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL);
                    state.sample += jump_samples;
                    state.realtime_epoch = state.realtime_epoch.map(|epoch| {
                        epoch
                            .checked_sub(Duration::from_millis(RETIREMENT_WAIT_MS))
                            .unwrap()
                    });
                    state.stop_slots = state.slot_count + jump_slots + OBSERVE_SLOTS * 8;
                    self.retirement = Some(RetirementProbe::Advanced);
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn generated_long_sms_crosses_both_handsets_and_receives_final_tsar_acknowledgment() {
    long_exchange(false);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn generated_long_sms_retires_both_transaction_directions_after_the_radio_guard() {
    long_exchange(true);
}
fn long_exchange(retire_transactions: bool) {
    let _paced = PACED_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let database = Database(std::path::PathBuf::from(format!(
        "/var/tmp/136bts-serving-full-tsar-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mode = retire_transactions.then_some(RetirementProbe::AwaitingCompletion);
    exchange(&database, 0, mode, u32::from(retire_transactions));
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn twelve_long_sms_iq_exchanges_reuse_ids_across_cell_restarts() {
    let _paced = PACED_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let database = Database(std::path::PathBuf::from(format!(
        "/var/tmp/136bts-serving-repeated-tsar-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    for ordinal in 0..REPEATED_EXCHANGES {
        let mode =
            (ordinal == TRANSACTION_IDS - 1).then_some(RetirementProbe::AwaitingWallCompletion);
        let retired = if ordinal >= TRANSACTION_IDS - 1 {
            u32::from(TRANSACTION_IDS)
        } else {
            0
        };
        eprintln!(
            "Starting full IQ exchange {}/{}",
            ordinal + 1,
            REPEATED_EXCHANGES
        );
        exchange(&database, ordinal, mode, retired);
        eprintln!(
            "Completed full IQ exchange {}/{}",
            ordinal + 1,
            REPEATED_EXCHANGES
        );
        std::thread::sleep(Duration::from_millis(crate::runtime::timers::SUPERFRAME_MS));
    }
}
fn exchange(
    database: &Database,
    ordinal: u8,
    retirement: Option<RetirementProbe>,
    expected_retired: u32,
) {
    let retire_transactions = retirement.is_some();
    let wall_wait = matches!(retirement, Some(RetirementProbe::AwaitingWallCompletion));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = database.0.clone();
    let payload = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 42 + u16::from(ordinal),
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(vec![b'x'; MAX_CMT_USER_OCTETS]),
    }
    .encode()
    .unwrap();
    let MessagePlan::Segmented(plan) = tsar::plan_message(
        &payload,
        SEGMENT_PAYLOAD_BUDGET,
        (TSAR_ID + ordinal) % TRANSACTION_IDS,
        AcknowledgmentWindow::Last,
        ProtocolVersion::Pv1,
    )
    .unwrap() else {
        panic!()
    };
    let registration = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 15,
        protocol_version: ProtocolVersion::Pv1,
    }
    .encode()
    .unwrap();
    let mut packets = vec![(ACCESS_BLOCK, packet_version(ProtocolVersion::Pv1))];
    packets.extend(
        access_packets(MobileIdentity::Min34(2), &registration)
            .into_iter()
            .map(|(offset, waveform)| (SECOND_REGISTRATION_SLOT + offset, waveform)),
    );
    let mut next = FIRST_SUBMISSION_SLOT;
    for (index, segment) in plan.segments().iter().enumerate() {
        let relay = AddressedRelay {
            message: RelayMessage::Data {
                transaction_id: FIRST_RELAY_ID + index as u8,
                protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
                payload: segment.encode(ProtocolVersion::Pv1).unwrap(),
            },
            addresses: RelayAddresses {
                destination: (index == 0).then(|| AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: AddressValue::Tbcd("1002".into()),
                }),
                ..RelayAddresses::default()
            },
        }
        .encode(Direction::Reverse)
        .unwrap();
        let fragments = access_packets(MobileIdentity::Min34(1), &relay);
        let end = fragments.last().unwrap().0;
        packets.extend(
            fragments
                .into_iter()
                .map(|(offset, waveform)| (next + offset, waveform)),
        );
        next += end + ACCESS_PATH_BLOCKS;
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut handsets = Handsets::new(plan.segments().len());
    handsets.retirement = retirement;
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        clock_sample: 0,
        tx_observation_offset_ticks: 0,
        epoch: None,
        tx: Vec::new(),
        slot_count: 0,
        stop_slots: FULL_EXCHANGE_STOP_SLOTS
            + if wall_wait {
                WALL_RETIREMENT_WAIT_MS as usize * RATE
                    / 1000
                    / (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL)
            } else {
                0
            },
        underflow_after_slot: None,
        stall_after_slot: None,
        realtime_epoch: Some(Instant::now()),
        handset_segmented: Some(handsets),
        handset_sms: None,
        handset_voice: None,
        handset_arq: None,
        handset_idle: None,
        traffic_packets: Vec::new(),
        packets,
        tx_active: false,
        rx_active: false,
        shutdown: shutdown.clone(),
    }));
    let prepared = PreparedRadio::from_radio(&config, Box::new(Device(state.clone()))).unwrap();
    let duration = Duration::from_secs(40)
        + if wall_wait {
            Duration::from_millis(WALL_RETIREMENT_WAIT_MS)
        } else {
            Duration::ZERO
        };
    let summary = run_prepared(&config, prepared, &shutdown, Some(duration)).unwrap();
    assert_eq!(summary.completed_registrations, 2);
    assert!(summary.stop_acknowledged);
    let mut store = crate::core::store::StateStore::open(&database.0).unwrap();
    let outgoing = store
        .outgoing_journals(0, u32::from(REPEATED_EXCHANGES) + 1)
        .unwrap();
    assert_eq!(outgoing.len(), usize::from(ordinal) + 1);
    let latest = outgoing.last().unwrap();
    assert_eq!(
        outgoing
            .iter()
            .filter(|journal| journal.retirement.is_some())
            .count(),
        expected_retired as usize
    );
    assert_eq!(latest.retirement.is_some(), retire_transactions);
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    let (total, retired): (u32, u32) = connection
        .query_row(
            "SELECT count(*),count(retired_at_unix_ms) FROM tsar_incoming",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(total, u32::from(ordinal) + 1);
    assert_eq!(retired, expected_retired);

    assert_eq!(
        latest.replay().unwrap().state(),
        crate::link::tsar::SenderState::Complete(crate::link::tsar::Completion::PeerReceived)
    );
    assert!(store.relay_accepted(latest.sms_id).unwrap());
    assert_eq!(
        store.state(latest.sms_id).unwrap(),
        crate::core::store::QueueState::Delivered
    );
    let state = state.lock().unwrap();
    assert!(!state.tx_active && !state.rx_active);
    let handsets = state.handset_segmented.as_ref().unwrap();
    assert!(handsets.source_received);
    assert!(handsets.target_response_accepted);
    let mut accepted = handsets.source_accepts.clone();
    accepted.sort_unstable();
    accepted.dedup();
    assert_eq!(
        accepted,
        (0..plan.segments().len())
            .map(|sequence| FIRST_RELAY_ID + sequence as u8)
            .collect::<Vec<_>>()
    );
    assert!(store.pending_incoming_responses(32).unwrap().is_empty());
    let received = handsets.received.as_ref().unwrap();
    let SmsMessage::Deliver {
        metadata,
        user_data: UserData::Ira(body),
        ..
    } = crate::l3::sms::decode_prefix(received, Direction::Forward)
        .unwrap()
        .message
    else {
        panic!()
    };
    assert_eq!(metadata.message_reference, 42 + u16::from(ordinal));
    assert_eq!(body, vec![b'x'; MAX_CMT_USER_OCTETS]);
}
