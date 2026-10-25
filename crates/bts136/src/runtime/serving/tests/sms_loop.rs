use super::*;
use crate::l3::{
    address::{AddressInfo, AddressValue},
    relay::{
        Direction, HigherLayerProtocol, RelayMessage,
        addressing::{AddressedRelay, RelayAddresses},
    },
    sms::{Metadata, SmsMessage, UserData},
};
const OBSERVE_SLOTS: usize = 12;
const WINDOW_SLOTS: usize = 24;
const RESPONSE_LEAD_BLOCKS: u64 = 4;
const SECOND_REGISTRATION_BLOCK: u64 = 12;
const SUBMISSION_BLOCK: u64 = 20;
const REPLY_SUBMISSION_BLOCK: u64 = 50;
const TRANSACTION_ID: u8 = 4;
const MESSAGE_REFERENCE: u16 = 12;
const STATION_CLASS_MARK: u8 = 15;
const NUMBER_TYPE: u8 = 0;
const NUMBERING_PLAN: u8 = 1;
const ZERO_PRIORITY: u8 = 0;
const TEST_DURATION_SECONDS: u64 = 20;
const TEST_STOP_SLOTS: usize = 2400;
const LOST_STATUS_RECOVERY_BLOCKS: usize = 64;
const MAX_ARQ_INTERRUPTION_FRAMES: usize = 32;
const CONFIRMATION: [u8; 2] = [15, 104];
const TEXT: &[u8] = b"message through the running cell";
const REPLY: &[u8] = b"reply from the second handset";

pub(super) struct Handset {
    receiver: SpachReceiver,
    arq: arq_loop::Handsets,
    last_sample: Option<f64>,
    notified: [bool; 2],
    notifications: [usize; 2],
    injected_registration: bool,
    arq_mode_silent: bool,
    received: Vec<(MobileIdentity, Vec<u8>)>,
    registration_replies: Vec<MobileIdentity>,
    accepted_transaction: [Option<u8>; 2],
    applied: Vec<(MobileIdentity, Vec<u8>)>,
    submission_accepted: [bool; 2],
}
impl Handset {
    fn new(mode: arq_loop::Mode) -> Self {
        Self {
            receiver: SpachReceiver::default(),
            arq: arq_loop::Handsets::new(mode),
            last_sample: None,
            notified: [false; 2],
            notifications: [0; 2],
            injected_registration: false,
            arq_mode_silent: mode == arq_loop::Mode::Silent,
            received: Vec::new(),
            registration_replies: Vec::new(),
            accepted_transaction: [None; 2],
            applied: Vec::new(),
            submission_accepted: [false; 2],
        }
    }
    pub(super) fn observe(&mut self, state: &mut Backend) -> Result<(), HalError> {
        if !state.slot_count.is_multiple_of(OBSERVE_SLOTS) {
            return Ok(());
        }
        self.arq.observe(state)?;
        if self.arq.first_begin_block.is_some()
            && self.arq_mode_silent
            && !self.injected_registration
        {
            self.injected_registration = true;
            let registration = AccessMessage::Registration {
                registration_type: RegistrationType::PowerUp,
                scm: STATION_CLASS_MARK,
                protocol_version: ProtocolVersion::Is136RevA,
            }
            .encode()
            .unwrap();
            respond(state, MobileIdentity::Min34(3), &registration);
        }
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
        let bursts = decode_forward_control(&samples, RATE as f64, LogicalChannel::Spach, 1)
            .map_err(|error| HalError::from(error.to_string()))?;
        for burst in bursts {
            let first = offset as f64 + burst.slot_start_sample;
            if self.last_sample.is_some_and(|last| {
                first < last + (SYMBOLS_PER_SLOT * SAMPLES_PER_SYMBOL / 2) as f64
            }) {
                continue;
            }
            self.last_sample = Some(first);
            let Ok(Some(message)) = self.receiver.push(&burst.fields.block.information) else {
                continue;
            };
            messages.push(message);
        }
        for message in messages {
            if message.channel == crate::link::spach::Channel::Arch
                && crate::l3::registration::RegistrationResponse::decode_prefix(&message.message)
                    .is_ok_and(|decoded| {
                        decoded.message == crate::l3::registration::RegistrationResponse::Accept
                    })
            {
                self.registration_replies.push(message.identity);
            }
            let index = match message.identity {
                MobileIdentity::Min34(1) => 0,
                MobileIdentity::Min34(2) => 1,
                _ => continue,
            };
            if message.channel == crate::link::spach::Channel::Pch
                && message.message == crate::l3::notification::sms_notification()
            {
                self.notifications[index] += 1;
                if !self.notified[index] {
                    self.notified[index] = true;
                    respond(state, message.identity, &CONFIRMATION);
                }
            } else if message.channel == crate::link::spach::Channel::Smsch {
                let response =
                    crate::l3::relay::decode_prefix(&message.message, Direction::Forward)
                        .map_err(|error| HalError::from(error.to_string()))?;
                if matches!(
                    response.message,
                    RelayMessage::Accept {
                        transaction_id: TRANSACTION_ID
                    }
                ) {
                    self.submission_accepted[index] = true;
                    continue;
                }
                let relay =
                    crate::l3::relay::addressing::decode(&message.message, Direction::Forward)
                        .map_err(|error| HalError::from(error.to_string()))?;
                let RelayMessage::Data {
                    transaction_id,
                    payload,
                    ..
                } = relay.relay.message
                else {
                    return Err("expected MT R-DATA".into());
                };
                let SmsMessage::Deliver { user_data, .. } =
                    crate::l3::sms::decode_prefix(&payload, Direction::Forward)
                        .map_err(|error| HalError::from(error.to_string()))?
                        .message
                else {
                    return Err("expected SMS delivery".into());
                };
                let text = user_data
                    .text()
                    .ok_or_else(|| HalError::from("expected supported text encoding"))?
                    .into_bytes();
                if !self.notified[index] {
                    return Err("SMS arrived without notification".into());
                }
                if !self
                    .applied
                    .contains(&(message.identity, message.message.clone()))
                {
                    self.applied
                        .push((message.identity, message.message.clone()));
                    self.received.push((message.identity, text));
                }
                self.accepted_transaction[index] = Some(transaction_id);
                respond(
                    state,
                    message.identity,
                    &RelayMessage::Accept { transaction_id }
                        .encode(Direction::Reverse)
                        .map_err(|error| HalError::from(error.to_string()))?,
                );
            }
        }
        Ok(())
    }
}
pub(super) fn respond(state: &mut Backend, identity: MobileIdentity, message: &[u8]) {
    let packets = access_packets(identity, message);
    let mut first_block =
        (state.slot_count as u64).div_ceil(CONTROL_SLOT_PERIOD) + RESPONSE_LEAD_BLOCKS;
    while packets.iter().any(|(offset, _)| {
        state
            .packets
            .iter()
            .any(|(block, _)| *block == first_block + offset)
    }) {
        first_block += 1;
    }
    state.packets.extend(
        packets
            .into_iter()
            .map(|(offset, packet)| (first_block + offset, packet)),
    );
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registered_handsets_exchange_sms_through_running_iq_cell() {
    sms_arq_loop(arq_loop::Mode::Normal);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn control_sms_arq_recovers_an_erased_continuation_over_iq() {
    sms_arq_loop(arq_loop::Mode::DropContinue);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn control_sms_relay_accept_can_precede_lost_arq_status_over_iq() {
    sms_arq_loop(arq_loop::Mode::LoseStatus);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn active_silent_arq_allows_other_registration_and_sms_control_with_required_paging_copies() {
    sms_arq_loop(arq_loop::Mode::Silent);
}
fn sms_arq_loop(mode: arq_loop::Mode) {
    sms_text_arq_loop(mode, [TEXT, REPLY]);
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn registered_handsets_exchange_latin1_and_bmp_sms_over_control_iq() {
    sms_text_arq_loop(
        arq_loop::Mode::Normal,
        ["café déjà vu".as_bytes(), "你好 Ελληνικά".as_bytes()],
    );
}
fn sms_text_arq_loop(mode: arq_loop::Mode, texts: [&[u8]; 2]) {
    let _paced = PACED_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = Database(std::env::temp_dir().join(format!(
        "136bts-serving-mt-sms-{}-{stamp}.sqlite3",
        std::process::id()
    )));
    let mut config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.core.state_path = db.0.clone();
    if mode == arq_loop::Mode::Silent {
        config.subscribers.push(crate::config::SubscriberConfig {
            identity: crate::config::IdentityConfig::Min34 { value: 3 },
            number: "1003".into(),
        });
    }
    let registration = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: STATION_CLASS_MARK,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    let outbound = submission("1002", texts[0]);
    let reply = submission("1001", texts[1]);
    let mut packets = vec![(ACCESS_BLOCK, packet())];
    packets.extend(
        access_packets(MobileIdentity::Min34(2), &registration)
            .into_iter()
            .map(|(offset, packet)| (SECOND_REGISTRATION_BLOCK + offset, packet)),
    );
    packets.extend(
        access_packets(MobileIdentity::Min34(1), &outbound)
            .into_iter()
            .map(|(offset, packet)| (SUBMISSION_BLOCK + offset, packet)),
    );
    packets.extend(
        access_packets(MobileIdentity::Min34(2), &reply)
            .into_iter()
            .map(|(offset, packet)| (REPLY_SUBMISSION_BLOCK + offset, packet)),
    );
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(Backend {
        sample: 0,
        tx_observation_offset_ticks: 0,
        epoch: None,
        tx: Vec::new(),
        slot_count: 0,
        stop_slots: TEST_STOP_SLOTS
            + if mode == arq_loop::Mode::LoseStatus {
                LOST_STATUS_RECOVERY_BLOCKS * CONTROL_SLOT_PERIOD as usize
            } else {
                0
            },
        underflow_after_slot: None,
        stall_after_slot: None,
        realtime_epoch: Some(Instant::now()),
        handset_segmented: None,
        handset_sms: Some(Handset::new(mode)),
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
    let (hub, interface) = crate::operations::Hub::new(&config);
    let summary = run_prepared_with_interface(
        &config,
        prepared,
        &shutdown,
        Some(Duration::from_secs(TEST_DURATION_SECONDS)),
        Some(interface),
    )
    .unwrap();
    assert_eq!(
        summary.completed_registrations,
        if mode == arq_loop::Mode::Silent { 3 } else { 2 }
    );
    assert!(summary.stop_acknowledged);
    let state = state.lock().unwrap();
    assert!(!state.tx_active);
    assert!(!state.rx_active);
    let handset = state.handset_sms.as_ref().unwrap();
    assert_eq!(handset.received.len(), 2);
    if mode == arq_loop::Mode::Silent {
        assert_eq!(handset.arq.statuses, 0);
        assert_eq!(handset.arq.confirmed, 0);
    } else if mode == arq_loop::Mode::LoseStatus {
        // The relay ACCEPT settles the transfer whose status was lost, so the base stops
        // polling it and only the other transfer's status reaches the base.
        assert!(handset.arq.dropped_status);
        assert!(
            handset.arq.statuses >= 1 && handset.arq.confirmed >= 1,
            "statuses={} confirmed={} echoes={:?}",
            handset.arq.statuses,
            handset.arq.confirmed,
            handset.arq.echoes,
        );
    } else {
        assert!(
            handset.arq.statuses >= 2,
            "statuses={} confirmed={} echoes={:?} trace={:#?}",
            handset.arq.statuses,
            handset.arq.confirmed,
            handset.arq.echoes,
            hub.snapshot().air_messages
        );
        assert!(handset.arq.confirmed >= 2);
    }
    assert!(handset.notifications.iter().all(|copies| *copies >= 2));
    assert!(handset.arq.max_interruption < MAX_ARQ_INTERRUPTION_FRAMES);
    let trace = hub.snapshot().air_messages;
    let begins: Vec<_> = trace
        .iter()
        .filter(|message| message.name == "ARQ BEGIN")
        .collect();
    assert!(begins.iter().all(|message| message.channel == "SMSCH"));
    assert!(begins.len() >= 2);
    if mode == arq_loop::Mode::Silent {
        let registration = trace
            .iter()
            .find(|message| {
                message.direction == "Forward"
                    && message.name == "Registration response"
                    && message.identity == "Min34(3)"
            })
            .unwrap();
        assert!(begins[0].sequence < registration.sequence);
        assert!(
            trace.iter().any(|message| message.name == "ARQ CONTINUE"
                && message.sequence > registration.sequence)
        );
        assert!(
            !trace
                .iter()
                .any(|message| message.status == "ARQ confirmed")
        );
        assert!(handset.injected_registration);
        assert!(
            handset
                .registration_replies
                .contains(&MobileIdentity::Min34(3))
        );
    }

    if mode == arq_loop::Mode::LoseStatus {
        let confirmed = trace
            .iter()
            .filter(|message| message.status == "ARQ confirmed")
            .collect::<Vec<_>>();
        assert_eq!(confirmed.len(), 1);
        assert!(
            trace.iter().any(|accept| accept.direction == "Reverse"
                && accept.name == "R-ACCEPT"
                && confirmed
                    .iter()
                    .all(|confirmation| confirmation.identity != accept.identity)),
            "the transfer whose status was lost completes through its R-ACCEPT"
        );
    }
    if mode == arq_loop::Mode::DropContinue {
        assert!(handset.arq.statuses >= 3);
        assert_eq!(handset.arq.echoes.len(), 2);
    }
    assert!(
        handset
            .received
            .contains(&(MobileIdentity::Min34(2), texts[0].to_vec()))
    );
    assert!(
        handset
            .received
            .contains(&(MobileIdentity::Min34(1), texts[1].to_vec()))
    );
    assert!(handset.accepted_transaction.iter().all(Option::is_some));
    assert!(handset.submission_accepted.iter().all(|accepted| *accepted));
    for identity in [MobileIdentity::Min34(1), MobileIdentity::Min34(2)] {
        assert!(
            crate::core::store::StateStore::open(&db.0)
                .unwrap()
                .pending_for(identity)
                .unwrap()
                .is_empty()
        );
    }
}

fn submission(destination: &str, text: &[u8]) -> Vec<u8> {
    let payload = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: MESSAGE_REFERENCE,
            privacy: ZERO_PRIORITY,
            urgency: ZERO_PRIORITY,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::from_text(std::str::from_utf8(text).unwrap()).unwrap(),
    }
    .encode()
    .unwrap();
    AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: TRANSACTION_ID,
            protocol: HigherLayerProtocol::Sms,
            payload,
        },
        addresses: RelayAddresses {
            destination: Some(AddressInfo {
                number_type: NUMBER_TYPE,
                numbering_plan: NUMBERING_PLAN,
                address: AddressValue::Tbcd(destination.into()),
            }),
            ..Default::default()
        },
    }
    .encode(Direction::Reverse)
    .unwrap()
}
