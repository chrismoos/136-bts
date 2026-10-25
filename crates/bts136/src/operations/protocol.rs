mod access_call;
mod alert;
mod audit;
mod call_control;
mod call_queue;
mod capabilities;
mod capability_report;
mod control_management;
mod decode;
mod maca;
mod maca_broadcast;
mod message_waiting;
mod page;
mod query;
mod registration;
mod security;
mod termination;
mod test_registration;
mod user_alert;
mod vendor_delivery;
use super::{proto, unix_ms};
use crate::{
    link::rach::MobileIdentity,
    phy::traffic::SLOT_BITS,
    radio::{carrier::SlotObserver, timed_tx::Receipt},
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
const MESSAGE_CAPACITY: usize = 1024;
const MAX_RECORD_BYTES: usize = 1024;
const BROADCAST_CAPACITY: usize = 128;
const WORK_CAPACITY: usize = 128;
const CONTROL_PERIOD: u64 = 3;
const WORK_TIMEOUT_MS: u64 = 50;
struct ForwardSlot {
    index: u64,
    ebcch_sequence: u64,
    bits: [bool; SLOT_BITS],
    dvcc: u8,
    channel: crate::phy::control::LogicalChannel,
}
impl Records {
    /// The latest burst measurement for a phone, with its recent uplink bit error rate.
    fn reverse_quality(&self, identity: &str) -> Option<proto::ReverseQuality> {
        let mut quality = self
            .qualities
            .iter()
            .find(|(id, _)| id == identity)
            .map(|(_, quality)| *quality)?;
        if let Some((_, window)) = self.bit_errors.iter().find(|(id, _)| id == identity) {
            let total = window.total();
            quality.bit_error_percent = total.percent();
            quality.checked_bits = total.checked;
        }
        Some(quality)
    }
}
struct Records {
    unapplied: mpsc::Receiver<proto::WireMessage>,
    messages: VecDeque<proto::WireMessage>,
    broadcasts: VecDeque<proto::WireMessage>,
    observed: VecDeque<proto::ObservedPhone>,
    qualities: VecDeque<(String, proto::ReverseQuality)>,
    bit_errors: VecDeque<(String, crate::phy::bit_errors::BitErrorWindow)>,
}
pub(crate) struct ProtocolLog {
    records: Mutex<Records>,
    // Writers never take the records lock, so a console read cannot make them drop a record.
    recorded: mpsc::Sender<proto::WireMessage>,
    sequence: AtomicU64,
    dropped: AtomicU64,
    jobs: mpsc::SyncSender<ForwardSlot>,
    receiver: Mutex<Option<mpsc::Receiver<ForwardSlot>>>,
}
impl Default for ProtocolLog {
    fn default() -> Self {
        let (jobs, receiver) = mpsc::sync_channel(WORK_CAPACITY);
        let (recorded, unapplied) = mpsc::channel();
        Self {
            recorded,
            records: Mutex::new(Records {
                unapplied,
                messages: VecDeque::new(),
                broadcasts: VecDeque::new(),
                observed: VecDeque::new(),
                qualities: VecDeque::new(),
                bit_errors: VecDeque::new(),
            }),
            sequence: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            jobs,
            receiver: Mutex::new(Some(receiver)),
        }
    }
}
fn pack_bits(bits: &[bool]) -> Vec<u8> {
    const OCTET_BITS: usize = 8;
    bits.chunks(OCTET_BITS)
        .map(|chunk| {
            chunk.iter().enumerate().fold(0, |value, (bit, set)| {
                value | (u8::from(*set) << (OCTET_BITS - 1 - bit))
            })
        })
        .collect()
}
pub(crate) fn debug_reverse_access(identity: MobileIdentity, bytes: &[u8]) {
    if !tracing::enabled!(target: "bts136::runtime::access", tracing::Level::DEBUG) {
        return;
    }
    let (message, decoded) = decode::control("Reverse", bytes);
    tracing::debug!(target: "bts136::runtime::access", channel="RACH", identity=?identity,
        message, decoded, bit_length=bytes.len()*u8::BITS as usize, raw_hex=%format!("{bytes:02x?}"),
        "Reverse access message decoded");
}
impl ProtocolLog {
    pub(crate) fn message(
        &self,
        direction: &str,
        channel: &str,
        identity: MobileIdentity,
        bytes: &[u8],
        status: &str,
    ) {
        let (name, decoded) = decode::control(direction, bytes);
        self.record(proto::WireMessage {
            direction: direction.into(),
            channel: channel.into(),
            identity: format!("{identity:?}"),
            name,
            decoded,
            raw: bytes.to_vec(),
            bit_length: (bytes.len() * 8) as u32,
            status: status.into(),
            ..Default::default()
        });
    }
    pub(crate) fn traffic(
        &self,
        direction: &str,
        channel: &str,
        identity: MobileIdentity,
        message: &crate::l3::legacy_traffic::TrafficMessage,
    ) {
        let Ok(bits) = message.encode() else {
            return;
        };
        let raw = bits
            .chunks(8)
            .map(|chunk| {
                chunk.iter().enumerate().fold(0u8, |value, (bit, set)| {
                    value | (u8::from(*set) << (7 - bit))
                })
            })
            .collect();
        self.record(proto::WireMessage {
            direction: direction.into(),
            channel: channel.into(),
            identity: format!("{identity:?}"),
            name: decode::traffic_name(message).into(),
            decoded: decode::traffic_details(direction, message),
            raw,
            bit_length: bits.len() as u32,
            status: if direction == "Forward" {
                "TX completed by host clock"
            } else {
                "RX decoded"
            }
            .into(),
            ..Default::default()
        });
    }
    pub(crate) fn arq_frame(
        &self,
        identity: MobileIdentity,
        bits: &[bool; crate::phy::control::INFORMATION_BITS],
    ) {
        use crate::link::spach::{
            Channel,
            arq::{self, ArqFrame},
        };
        let Ok(frame) = arq::decode(bits) else {
            return;
        };
        let (header, name, extra) = match frame {
            ArqFrame::Begin {
                header,
                message_octets,
                ..
            } => (header, "ARQ BEGIN", format!("L3 octets={message_octets}")),
            ArqFrame::Continue {
                header,
                frame_number,
                ..
            } => (header, "ARQ CONTINUE", format!("FRNO={frame_number}")),
        };
        self.record(proto::WireMessage {
            direction: "Forward".into(),
            channel: match header.channel {
                Channel::Arch => "ARCH",
                Channel::Smsch => "SMSCH",
                Channel::Pch => "PCH",
            }
            .into(),
            identity: format!("{identity:?}"),
            name: name.into(),
            decoded: format!(
                "PEA={} PI={} ARM={:?}\n{extra}",
                header.partial_echo, header.polling, header.response_mode
            ),
            raw: pack_bits(bits),
            bit_length: bits.len() as u32,
            status: "TX completed by host clock".into(),
            ..Default::default()
        });
    }
    pub(crate) fn arq_outcome(&self, identity: MobileIdentity, outcome: &str) {
        self.record(proto::WireMessage {
            direction: "Forward".into(),
            channel: "ARCH".into(),
            identity: format!("{identity:?}"),
            name: "ARQ transaction".into(),
            decoded: "Registration reply".into(),
            status: outcome.into(),
            ..Default::default()
        });
    }
    pub(crate) fn arq_status(
        &self,
        identity: Option<MobileIdentity>,
        status: &crate::link::rach::ArqStatus,
        bits: &[bool],
        matched: bool,
    ) {
        let map: String = status
            .received_frames
            .iter()
            .map(|set| if *set { '1' } else { '0' })
            .collect();
        self.record(proto::WireMessage {
            direction: "Reverse".into(),
            channel: "RACH".into(),
            identity: identity.map_or_else(String::new, |identity| format!("{identity:?}")),
            name: "SPACH ARQ status".into(),
            decoded: format!("PEA={}\nFRNO MAP={map}", status.partial_echo),
            raw: pack_bits(bits),
            bit_length: bits.len() as u32,
            status: if matched {
                "Matched active ARQ transaction"
            } else {
                "Unknown or stale PEA"
            }
            .into(),
            ..Default::default()
        });
    }
    fn record(&self, mut message: proto::WireMessage) {
        message.raw.truncate(MAX_RECORD_BYTES);
        if message.decoded.len() > MAX_RECORD_BYTES {
            let mut end = MAX_RECORD_BYTES;
            while !message.decoded.is_char_boundary(end) {
                end -= 1;
            }
            message.decoded.truncate(end);
            message.decoded.push('…');
        }
        message.sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        message.unix_ms = unix_ms();
        message.repetitions = 1;
        let _ = self.recorded.send(message);
        if let Ok(mut records) = self.records.try_lock() {
            Self::apply_recorded(&mut records);
        }
    }
    fn apply_recorded(records: &mut Records) {
        while let Ok(message) = records.unapplied.try_recv() {
            Self::apply(records, message);
        }
    }
    fn apply(records: &mut Records, mut message: proto::WireMessage) {
        if message.direction == "Reverse" {
            message.reverse_quality = records.reverse_quality(&message.identity);
        }
        if message.direction == "Reverse" && !message.identity.is_empty() {
            let mut esn = (message.channel == "RACH")
                .then(|| crate::l3::registration::SerialNumber::decode(&message.raw).ok())
                .flatten()
                .map(|serial| serial.esn);
            if let Some(index) = records
                .observed
                .iter()
                .position(|phone| phone.identity == message.identity)
                && let Some(previous) = records.observed.remove(index)
            {
                esn = esn.or(previous.esn);
            }
            if records.observed.len() == MESSAGE_CAPACITY {
                records.observed.pop_front();
            }
            records.observed.push_back(proto::ObservedPhone {
                identity: message.identity.clone(),
                last_response_unix_ms: message.unix_ms,
                last_message: message.name.clone(),
                esn,
                reverse_quality: message.reverse_quality,
            });
        }
        let (rows, capacity) = if message.broadcast {
            (&mut records.broadcasts, BROADCAST_CAPACITY)
        } else {
            (&mut records.messages, MESSAGE_CAPACITY)
        };
        if message.broadcast
            && let Some(position) = rows
                .iter()
                .position(|old| old.raw == message.raw && old.channel == message.channel)
        {
            let old = rows.remove(position).expect("matching broadcast exists");
            message.repetitions = old.repetitions + 1;
        }
        if rows.len() == capacity {
            rows.pop_front();
        }
        rows.push_back(message);
    }
    #[cfg(test)]
    pub(crate) fn messages(&self) -> Vec<proto::WireMessage> {
        let mut records = self.records.lock().unwrap();
        Self::apply_recorded(&mut records);
        let mut messages: Vec<_> = records
            .messages
            .iter()
            .chain(&records.broadcasts)
            .cloned()
            .collect();
        messages.sort_by_key(|message| message.sequence);
        messages
    }
    pub(crate) fn observer(
        self: &Arc<Self>,
        dvcc: u8,
        fbcch_slots: u8,
        ebcch_slots: u8,
    ) -> Box<dyn SlotObserver> {
        Box::new(ForwardObserver {
            log: self.clone(),
            pending: VecDeque::new(),
            dvcc,
            fbcch_slots,
            ebcch_slots,
        })
    }
    pub(crate) fn start_worker(self: &Arc<Self>) -> Option<Worker> {
        let receiver = self.receiver.lock().ok()?.take()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let log = self.clone();
        let handle = thread::spawn(move || {
            let mut decoder = crate::link::broadcast::FbcchReceiver::default();
            let mut ebcch_decoder = crate::link::broadcast::EbcchReceiver::default();
            let mut previous_ebcch = None;
            let mut previous = None;
            let mut broadcast_version = None;
            while !worker_stop.load(Ordering::Relaxed) {
                let job = match receiver.recv_timeout(Duration::from_millis(WORK_TIMEOUT_MS)) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                if previous.is_some_and(|index| job.index != index + CONTROL_PERIOD)
                    || job.index % crate::scheduler::broadcast::PHYSICAL_SLOTS == 0
                {
                    decoder.discontinuity();
                }
                previous = Some(job.index);
                let Ok(fields) =
                    crate::phy::control::decode_forward(&job.bits, job.channel, job.dvcc)
                else {
                    decoder.discontinuity();
                    ebcch_decoder.discontinuity();
                    continue;
                };
                let information = fields
                    .block
                    .information
                    .as_slice()
                    .try_into()
                    .expect("forward information has fixed length");
                let is_ebcch = job.channel == crate::phy::control::LogicalChannel::Ebcch;
                let result = if is_ebcch {
                    if previous_ebcch.is_some_and(|index| job.ebcch_sequence != index + 1) {
                        ebcch_decoder.discontinuity();
                    }
                    previous_ebcch = Some(job.ebcch_sequence);
                    ebcch_decoder.push(information)
                } else {
                    decoder.push(information)
                };
                if let Ok(messages) = result {
                    for bytes in messages {
                        if let Ok(decoded) = crate::l3::broadcast::decode_prefix(&bytes)
                            && let crate::l3::broadcast::MandatoryBroadcast::SystemIdentity(
                                identity,
                            ) = decoded.message
                        {
                            broadcast_version = Some(identity.protocol_version);
                        }
                        let (name, decoded) =
                            decode::broadcast_in_version(&bytes, broadcast_version);
                        log.record(proto::WireMessage {
                            direction: "Forward".into(),
                            channel: if is_ebcch { "E-BCCH" } else { "F-BCCH" }.into(),
                            name,
                            decoded,
                            bit_length: (bytes.len() * 8) as u32,
                            raw: bytes,
                            status: "TX completed by host clock".into(),
                            broadcast: true,
                            ..Default::default()
                        });
                    }
                }
            }
        });
        Some(Worker {
            stop,
            handle: Some(handle),
        })
    }
}
pub(crate) struct Worker {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
struct ForwardObserver {
    log: Arc<ProtocolLog>,
    pending: VecDeque<(u64, ForwardSlot)>,
    dvcc: u8,
    fbcch_slots: u8,
    ebcch_slots: u8,
}
impl SlotObserver for ForwardObserver {
    fn submitted(&mut self, index: u64, bits: &[bool; SLOT_BITS], receipt: &Receipt) {
        if index.is_multiple_of(CONTROL_PERIOD)
            && (index % crate::scheduler::broadcast::PHYSICAL_SLOTS) / CONTROL_PERIOD
                < u64::from(self.fbcch_slots) + u64::from(self.ebcch_slots)
        {
            self.pending.push_back((
                receipt.sequence(),
                ForwardSlot {
                    index,
                    ebcch_sequence: (index / crate::scheduler::broadcast::PHYSICAL_SLOTS)
                        * u64::from(self.ebcch_slots)
                        + ((index % crate::scheduler::broadcast::PHYSICAL_SLOTS) / CONTROL_PERIOD)
                            .saturating_sub(u64::from(self.fbcch_slots)),
                    bits: *bits,
                    dvcc: self.dvcc,
                    channel: if (index % crate::scheduler::broadcast::PHYSICAL_SLOTS)
                        / CONTROL_PERIOD
                        >= u64::from(self.fbcch_slots)
                    {
                        crate::phy::control::LogicalChannel::Ebcch
                    } else {
                        crate::phy::control::LogicalChannel::Fbcch
                    },
                },
            ));
        }
    }
    fn completed(&mut self, receipt: &Receipt) {
        while self
            .pending
            .front()
            .is_some_and(|(sequence, _)| *sequence <= receipt.sequence())
        {
            let (sequence, job) = self
                .pending
                .pop_front()
                .expect("completed forward slot exists");
            if sequence == receipt.sequence() && self.log.jobs.try_send(job).is_err() {
                self.log.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_written_while_a_reader_holds_the_log_are_kept() {
        let log = ProtocolLog::default();
        let held = log.records.lock().unwrap();
        log.record(proto::WireMessage {
            name: "while held".into(),
            ..Default::default()
        });
        drop(held);
        let air_messages = log.messages();
        assert_eq!(log.dropped.load(Ordering::Relaxed), 0);
        assert!(
            air_messages
                .iter()
                .any(|message| message.name == "while held")
        );
    }
    #[test]
    fn unknown_messages_retain_identity_direction_and_bytes() {
        let log = ProtocolLog::default();
        log.message(
            "Reverse",
            "RACH",
            MobileIdentity::Min34(1),
            &[255, 254],
            "RX decoded",
        );
        let air_messages = log.messages();
        let message = &air_messages[0];
        assert_eq!(message.direction, "Reverse");
        assert_eq!(message.identity, "Min34(1)");
        assert_eq!(message.raw, [255, 254]);
        assert_eq!(message.bit_length, 16);
    }
    #[test]
    fn observed_esn_survives_other_reverse_messages_without_a_subscriber() {
        let log = ProtocolLog::default();
        let identity = MobileIdentity::Min34(3410151054);
        log.message(
            "Reverse",
            "RACH",
            identity,
            &[0x0d, 0x5e, 0xc4, 0xdb, 0xb7],
            "RX decoded",
        );
        log.message("Reverse", "RACH", identity, &[0xff], "RX decoded");
        log.message("Reverse", "RACH", identity, &[0x0d], "RX decoded");
        log.message(
            "Forward",
            "RACH",
            identity,
            &[0x0d, 0, 0, 0, 1],
            "TX completed",
        );
        log.message(
            "Reverse",
            "RACH",
            MobileIdentity::Min34(7),
            &[0xff],
            "RX decoded",
        );
        let observed_phones = log.annotate(&mut []);
        assert_eq!(
            observed_phones
                .iter()
                .find(|phone| phone.identity == "Min34(3410151054)")
                .unwrap()
                .esn,
            Some(0x5ec4dbb7)
        );
        assert_eq!(
            observed_phones
                .iter()
                .find(|phone| phone.identity == "Min34(7)")
                .unwrap()
                .esn,
            None
        );
    }
    #[test]
    fn broadcasts_collapse_without_evicting_phone_messages() {
        let log = ProtocolLog::default();
        log.message(
            "Reverse",
            "RACH",
            MobileIdentity::Min34(1),
            &[255],
            "RX decoded",
        );
        for _ in 0..MESSAGE_CAPACITY {
            log.record(proto::WireMessage {
                broadcast: true,
                channel: "F-BCCH".into(),
                raw: vec![1],
                ..Default::default()
            });
        }
        let air_messages = log.messages();
        assert_eq!(air_messages.len(), 2);
        assert_eq!(air_messages[1].repetitions, MESSAGE_CAPACITY as u64);
    }
}

#[cfg(test)]
mod arq_tests {
    use super::*;
    use crate::link::{
        rach::{self, ArqStatus},
        spach::{
            Channel, HeaderFlags,
            arq::{ArqHeader, ResponseMode, fragment_message},
        },
    };
    #[test]
    fn arq_records_show_frame_numbers_status_maps_and_unknown_identity_without_false_l3_success() {
        let log = ProtocolLog::default();
        let identity = MobileIdentity::Min34(1);
        let frames = fragment_message(
            ArqHeader {
                channel: Channel::Arch,
                flags: HeaderFlags::default(),
                partial_echo: 42,
                polling: true,
                response_mode: ResponseMode::Contention,
            },
            identity,
            &[1; 20],
        )
        .unwrap();
        log.arq_frame(identity, &frames[0].encode().unwrap());
        log.arq_frame(identity, &frames[1].encode().unwrap());
        let mut status = ArqStatus {
            partial_echo: 42,
            received_frames: [false; rach::ARQ_MAP_BITS],
        };
        status.received_frames[0] = true;
        let bits = rach::encode_arq_status(crate::phy::rach::BurstFormat::Normal, &status).unwrap();
        log.arq_status(Some(identity), &status, &bits, true);
        log.arq_status(None, &status, &bits, false);
        log.arq_outcome(identity, "Expired without ARQ confirmation");
        let air_messages = log.messages();
        let rows = air_messages;
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].name, "ARQ BEGIN");
        assert!(rows[0].decoded.contains("PEA=42"));
        assert_eq!(rows[1].name, "ARQ CONTINUE");
        assert!(rows[1].decoded.contains("FRNO=0"));
        assert_eq!(rows[2].identity, "Min34(1)");
        assert!(
            rows[2]
                .decoded
                .contains("FRNO MAP=10000000000000000000000000")
        );
        assert_eq!(rows[2].bit_length, bits.len() as u32);
        assert_eq!(rows[3].identity, "");
        assert_eq!(rows[3].status, "Unknown or stale PEA");
        assert_eq!(rows[4].name, "ARQ transaction");
        assert!(rows.iter().all(|row| row.name != "Registration response"));
    }
}

#[cfg(test)]
mod debug_logging {
    use super::*;
    #[derive(Clone)]
    struct Writer(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn reverse_debug_log_contains_identity_decoded_message_and_wire_bytes_without_a_console() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(output.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            debug_reverse_access(
                MobileIdentity::Min34(3410151054),
                &[0x07, 0x40, 0x18, 0x09, 0x11, 0x11, 0x60],
            )
        });
        let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        for field in [
            "3410151054",
            "Origination",
            "2222",
            "AnalogSpeech",
            "raw_hex",
            "07, 40, 18, 09, 11, 11, 60",
        ] {
            assert!(log.contains(field), "{field}: {log}");
        }
    }
}
