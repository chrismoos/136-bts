mod device_subscribers;
pub(crate) mod sms_hub;
pub(crate) use device_subscribers::DeviceSubscriber;
pub mod broadcast;
pub mod incoming_journal;
pub(crate) mod ordinary_replay;
pub mod outgoing_journal;
pub(crate) mod replica;
pub mod requests;
use super::registry::{LocalRegistry, ProvisionedSubscriber, RegistryError};
use crate::{
    l3::{
        address::{AddressInfo, AddressValue},
        legacy_traffic::{TrafficMessageError, TrafficRelay},
        relay::{
            Direction, HigherLayerProtocol, RelayMessage,
            addressing::{AddressedRelay, AddressingError, RelayAddresses},
        },
        sms::{self, SmsError, SmsMessage},
    },
    link::rach::MobileIdentity,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{error::Error, fmt, path::Path, time::Duration};

const ORIGINATING_NUMBER_TYPE: u8 = 0;
const TELEPHONY_PLAN: u8 = 1;
fn record_relay_accept_in(
    transaction: &rusqlite::Transaction<'_>,
    id: SmsId,
    recipient: MobileIdentity,
) -> Result<QueueState, StoreError> {
    let (kind, _, value) = recipient.parts();
    let (recipient_kind, recipient_value, payload, state) = transaction
        .query_row(
            "SELECT recipient_type, recipient_value, sms_pdu, state FROM sms_queue WHERE id = ?1",
            [id.0],
            |row| {
                Ok((
                    row.get::<_, u8>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::MissingMessage)?;
    if (kind, value as i64) != (recipient_kind, recipient_value) {
        return Err(StoreError::RecipientMismatch);
    }
    if !matches!(state, QUEUED | DELIVERED) {
        return Err(StoreError::InvalidTransition);
    }
    let decoded = sms::decode_prefix(&payload, Direction::Forward).map_err(StoreError::Sms)?;
    if !decoded.optional_octets.is_empty() {
        return Err(StoreError::CorruptState);
    }
    let awaiting_receipt = match decoded.message {
        SmsMessage::Deliver { metadata, .. } => {
            metadata.delivery_acknowledgment || metadata.manual_acknowledgment
        }
        SmsMessage::DeliveryAck { .. } | SmsMessage::ManualAck { .. } => false,
        _ => return Err(StoreError::MessageType),
    };
    let target = if awaiting_receipt && state == QUEUED {
        QUEUED
    } else {
        DELIVERED
    };
    transaction.execute(
        "UPDATE sms_queue SET relay_accepted = 1, state = ?1 WHERE id = ?2",
        params![target, id.0],
    )?;
    Ok(if target == QUEUED {
        QueueState::Queued
    } else {
        QueueState::Delivered
    })
}
fn submission_dedup_key(key: &str) -> String {
    format!("submit:{key}")
}
enum EnqueueScope {
    SingleRelay,
    CompleteTeleservice,
}
const SCHEMA_VERSION: i64 = 14;
const HUB_SCHEMA: &str = include_str!("../../migrations/0014_sms_hub.sql");
const OPERATOR_SCHEMA_VERSION: i64 = 13;
const OPERATOR_SCHEMA: &str = include_str!("../../migrations/0013_operator_sms_origins.sql");
const ESN_SCHEMA_VERSION: i64 = 12;
const ESN_SCHEMA: &str = include_str!("../../migrations/0012_esn_subscribers.sql");
const BROADCAST_SCHEMA_VERSION: i64 = 11;
const BROADCAST_SCHEMA: &str = include_str!("../../migrations/0011_broadcast_revisions.sql");
const ORDINARY_REPLAY_SCHEMA_VERSION: i64 = 10;
const OUTGOING_RETIREMENT_SCHEMA_VERSION: i64 = 9;
const ORDINARY_REPLAY_SCHEMA: &str = include_str!("../../migrations/0010_ordinary_sms_replay.sql");
const OUTGOING_JOURNAL_SCHEMA_VERSION: i64 = 8;
const OUTGOING_RETIREMENT_SCHEMA: &str =
    include_str!("../../migrations/0009_outgoing_tsar_retirement.sql");
const INCOMING_RESPONSE_SCHEMA_VERSION: i64 = 7;
const OUTGOING_TSAR_SCHEMA: &str = include_str!("../../migrations/0008_outgoing_tsar_journal.sql");
const INCOMING_JOURNAL_SCHEMA_VERSION: i64 = 6;
const INCOMING_RESPONSE_SCHEMA: &str =
    include_str!("../../migrations/0007_incoming_tsar_responses.sql");
const MANAGEMENT_SCHEMA_VERSION: i64 = 5;
const INCOMING_TSAR_SCHEMA: &str = include_str!("../../migrations/0006_incoming_tsar_journal.sql");
const RELAY_SCHEMA_VERSION: i64 = 4;
const MANAGEMENT_SCHEMA: &str = include_str!("../../migrations/0005_subscriber_management.sql");
const MANUAL_SCHEMA_VERSION: i64 = 3;
const RELAY_ACCEPTANCE_SCHEMA: &str = include_str!("../../migrations/0004_relay_acceptance.sql");
const DELIVERY_SCHEMA_VERSION: i64 = 2;
const MANUAL_RECEIPT_SCHEMA: &str = include_str!("../../migrations/0003_manual_receipts.sql");
const FIRST_SCHEMA_VERSION: i64 = 1;
const RECEIPT_SCHEMA: &str = include_str!("../../migrations/0002_delivery_receipts.sql");
const BUSY_TIMEOUT_SECONDS: u64 = 5;
const QUEUED: i64 = 0;
const DELIVERED: i64 = 1;
const FAILED: i64 = 2;
const INITIAL_SCHEMA: &str = include_str!("../../migrations/0001_initial.sql");
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmsId(i64);
impl SmsId {
    pub fn value(self) -> i64 {
        self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueState {
    Queued,
    Delivered,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedSms {
    pub id: SmsId,
    pub sender_number: String,
    pub sms_pdu: Vec<u8>,
    pub created_at_unix_ms: i64,
}
#[derive(Debug)]
pub struct NewSms<'a> {
    pub dedup_key: &'a str,
    pub sender: MobileIdentity,
    pub recipient: MobileIdentity,
    pub message: &'a SmsMessage,
    pub created_at_unix_ms: i64,
}
pub struct NewOperatorSms<'a> {
    pub dedup_key: &'a str,
    pub sender_number: &'a str,
    pub recipient: MobileIdentity,
    pub message: &'a SmsMessage,
    pub created_at_unix_ms: i64,
}
#[derive(Clone, Copy)]
enum SmsOrigin<'a> {
    Subscriber(MobileIdentity),
    Operator(&'a str),
}
struct EnqueueRequest<'a> {
    dedup_key: &'a str,
    origin: SmsOrigin<'a>,
    recipient: MobileIdentity,
    message: &'a SmsMessage,
    created_at_unix_ms: i64,
}
#[derive(Debug, Default)]
pub(crate) struct MessageFilter {
    pub before_id: Option<i64>,
    pub phase: Option<crate::operations::proto::SmsPhase>,
    /// Matches the sender or the recipient.
    pub number: Option<String>,
}
#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Hub(sms_hub::HubError),
    IncomingJournal(incoming_journal::JournalError),
    OutgoingJournal(outgoing_journal::JournalError),
    Registry(RegistryError),
    Sms(SmsError),
    Addressing(AddressingError),
    Traffic(TrafficMessageError),
    SchemaVersion(i64),
    MissingSubscriber,
    DisabledSubscriber,
    MessageType,
    DedupKey,
    IdempotencyConflict,
    SubmissionCapacity,
    InvalidTransition,
    MissingMessage,
    Timestamp,
    CorruptState,
    ReceiptMismatch,
    RecipientMismatch,
    SenderMismatch,
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "persistent state failed: {self:?}")
    }
}
impl Error for StoreError {}
impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AcknowledgmentReference {
    pub(crate) kind: AcknowledgmentKind,
    pub(crate) message_reference: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcknowledgmentKind {
    Delivery = 1,
    Manual = 2,
}
impl AcknowledgmentKind {
    pub(crate) fn from_message(
        message: &SmsMessage,
    ) -> Result<AcknowledgmentReference, StoreError> {
        match message {
            SmsMessage::DeliveryAck {
                message_reference, ..
            } => Ok(AcknowledgmentReference {
                kind: Self::Delivery,
                message_reference: *message_reference,
            }),
            SmsMessage::ManualAck {
                message_reference, ..
            } => Ok(AcknowledgmentReference {
                kind: Self::Manual,
                message_reference: *message_reference,
            }),
            _ => Err(StoreError::MessageType),
        }
    }
    pub(crate) fn requested(self, metadata: &sms::Metadata) -> bool {
        match self {
            Self::Delivery => metadata.delivery_acknowledgment,
            Self::Manual => metadata.manual_acknowledgment,
        }
    }
}
#[derive(Debug)]
pub struct StateStore {
    connection: Connection,
    replication: Option<replica::Replication>,
}
impl StateStore {
    /// Newest timestamp recorded by the tables that reject an older clock.
    pub fn latest_unix_ms(&self) -> Result<Option<i64>, StoreError> {
        Ok(self.connection.query_row(
            "SELECT max(latest) FROM (SELECT max(coalesce(confirmed_at_unix_ms,prepared_at_unix_ms)) AS latest FROM broadcast_revisions UNION ALL SELECT max(coalesce(settled_at_unix_ms,observed_at_unix_ms)) FROM ordinary_sms_requests)",
            [],
            |row| row.get(0),
        )?)
    }
    pub(crate) fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(Duration::from_secs(BUSY_TIMEOUT_SECONDS))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Err(StoreError::SchemaVersion(version));
        }
        Ok(Self {
            connection,
            replication: None,
        })
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(BUSY_TIMEOUT_SECONDS))?;
        let original_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        // Referenced tables must be rebuilt with foreign keys disabled before the transaction.
        connection.pragma_update(None, "foreign_keys", original_version >= SCHEMA_VERSION)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                transaction.execute_batch(INITIAL_SCHEMA)?;
                transaction.execute_batch(RECEIPT_SCHEMA)?;
                transaction.execute_batch(MANUAL_RECEIPT_SCHEMA)?;
                transaction.execute_batch(RELAY_ACCEPTANCE_SCHEMA)?;
                transaction.execute_batch(MANAGEMENT_SCHEMA)?;
                transaction.pragma_update(None, "user_version", MANAGEMENT_SCHEMA_VERSION)?;
            }
            FIRST_SCHEMA_VERSION => {
                transaction.execute_batch(RECEIPT_SCHEMA)?;
                transaction.execute_batch(MANUAL_RECEIPT_SCHEMA)?;
                transaction.execute_batch(RELAY_ACCEPTANCE_SCHEMA)?;
                transaction.execute_batch(MANAGEMENT_SCHEMA)?;
                transaction.pragma_update(None, "user_version", MANAGEMENT_SCHEMA_VERSION)?;
            }
            DELIVERY_SCHEMA_VERSION => {
                transaction.execute_batch(MANUAL_RECEIPT_SCHEMA)?;
                transaction.execute_batch(RELAY_ACCEPTANCE_SCHEMA)?;
                transaction.execute_batch(MANAGEMENT_SCHEMA)?;
                transaction.pragma_update(None, "user_version", MANAGEMENT_SCHEMA_VERSION)?;
            }
            MANUAL_SCHEMA_VERSION => {
                transaction.execute_batch(RELAY_ACCEPTANCE_SCHEMA)?;
                transaction.execute_batch(MANAGEMENT_SCHEMA)?;
                transaction.pragma_update(None, "user_version", MANAGEMENT_SCHEMA_VERSION)?;
            }
            RELAY_SCHEMA_VERSION => {
                transaction.execute_batch(MANAGEMENT_SCHEMA)?;
                transaction.pragma_update(None, "user_version", MANAGEMENT_SCHEMA_VERSION)?;
            }
            MANAGEMENT_SCHEMA_VERSION
            | INCOMING_JOURNAL_SCHEMA_VERSION
            | INCOMING_RESPONSE_SCHEMA_VERSION
            | OUTGOING_JOURNAL_SCHEMA_VERSION
            | OUTGOING_RETIREMENT_SCHEMA_VERSION
            | ORDINARY_REPLAY_SCHEMA_VERSION
            | BROADCAST_SCHEMA_VERSION
            | ESN_SCHEMA_VERSION
            | OPERATOR_SCHEMA_VERSION
            | SCHEMA_VERSION => {}
            _ => return Err(StoreError::SchemaVersion(version)),
        }
        if version < INCOMING_JOURNAL_SCHEMA_VERSION {
            transaction.execute_batch(INCOMING_TSAR_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < INCOMING_RESPONSE_SCHEMA_VERSION {
            transaction.execute_batch(INCOMING_RESPONSE_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < OUTGOING_JOURNAL_SCHEMA_VERSION {
            transaction.execute_batch(OUTGOING_TSAR_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < OUTGOING_RETIREMENT_SCHEMA_VERSION {
            transaction.execute_batch(OUTGOING_RETIREMENT_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < ORDINARY_REPLAY_SCHEMA_VERSION {
            transaction.execute_batch(ORDINARY_REPLAY_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < BROADCAST_SCHEMA_VERSION {
            transaction.execute_batch(BROADCAST_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < ESN_SCHEMA_VERSION {
            transaction.execute_batch(ESN_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < OPERATOR_SCHEMA_VERSION {
            transaction.execute_batch(OPERATOR_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < SCHEMA_VERSION {
            transaction.execute_batch(HUB_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if transaction
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some()
        {
            return Err(StoreError::CorruptState);
        }
        transaction.commit()?;
        connection.pragma_update(None, "foreign_keys", true)?;
        Ok(Self {
            connection,
            replication: None,
        })
    }
    pub fn seed_once(&mut self, provisioning: &[ProvisionedSubscriber]) -> Result<(), StoreError> {
        LocalRegistry::try_new(provisioning.to_vec()).map_err(StoreError::Registry)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seeded: bool = transaction.query_row(
            "SELECT seeded FROM provisioning_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if !seeded {
            for subscriber in provisioning {
                let (kind, _, value) = subscriber.identity.parts();
                transaction.execute("INSERT INTO subscribers(identity_type, identity_value, number, enabled) VALUES(?1, ?2, ?3, 1)", params![kind, value as i64, subscriber.number])?;
            }
            transaction.execute("UPDATE provisioning_state SET seeded = 1 WHERE id = 1", [])?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub(crate) fn save_subscriber(
        &mut self,
        subscriber: &ProvisionedSubscriber,
    ) -> Result<(), StoreError> {
        LocalRegistry::try_new(vec![subscriber.clone()]).map_err(StoreError::Registry)?;
        let (kind, _, value) = subscriber.identity.parts();
        self.connection.execute("INSERT INTO subscribers(identity_type, identity_value, number, enabled) VALUES(?1, ?2, ?3, 1) ON CONFLICT(identity_type, identity_value) DO UPDATE SET number = excluded.number, enabled = 1",params![kind, value as i64, subscriber.number])?;
        Ok(())
    }
    pub(crate) fn disable_subscriber(
        &mut self,
        identity: MobileIdentity,
    ) -> Result<(), StoreError> {
        let (kind, _, value) = identity.parts();
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE esn_subscribers SET enabled=0 WHERE identity_type=?1 AND identity_value=?2",
            params![kind, value as i64],
        )?;
        let changed = transaction.execute("UPDATE subscribers SET enabled = 0 WHERE identity_type = ?1 AND identity_value = ?2 AND enabled = 1",params![kind,value as i64])?;
        if changed == 0 {
            return Err(StoreError::MissingSubscriber);
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn reconcile(&mut self, provisioning: &[ProvisionedSubscriber]) -> Result<(), StoreError> {
        LocalRegistry::try_new(provisioning.to_vec()).map_err(StoreError::Registry)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("UPDATE subscribers SET enabled = 0", [])?;
        for subscriber in provisioning {
            let (kind, _, value) = subscriber.identity.parts();
            transaction.execute("INSERT INTO subscribers(identity_type, identity_value, number, enabled) VALUES (?1, ?2, ?3, 1)
                ON CONFLICT(identity_type, identity_value) DO UPDATE SET number = excluded.number, enabled = 1",
                params![kind, value as i64, subscriber.number])?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn enabled_subscribers(&self) -> Result<Vec<ProvisionedSubscriber>, StoreError> {
        let mut statement = self.connection.prepare("SELECT identity_type, identity_value, number FROM subscribers WHERE enabled = 1 ORDER BY number")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, u8>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut subscribers = Vec::new();
        for row in rows {
            let (kind, value, number) = row?;
            let value = u64::try_from(value).map_err(|_| StoreError::CorruptState)?;
            let identity = match kind {
                0 => MobileIdentity::Tmsi20(
                    u32::try_from(value).map_err(|_| StoreError::CorruptState)?,
                ),
                1 => MobileIdentity::Tmsi24(
                    u32::try_from(value).map_err(|_| StoreError::CorruptState)?,
                ),
                2 => MobileIdentity::Min34(value),
                3 => MobileIdentity::Imsi50(value),
                _ => return Err(StoreError::CorruptState),
            };
            subscribers.push(ProvisionedSubscriber { identity, number });
        }
        LocalRegistry::try_new(subscribers.clone()).map_err(StoreError::Registry)?;
        Ok(subscribers)
    }
    #[cfg(test)]
    pub(crate) fn operation_messages(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::operations::proto::Sms>, StoreError> {
        self.operation_messages_page(&MessageFilter::default(), 0, limit)
            .map(|(messages, _)| messages)
    }
    pub(crate) fn queued_message_count(&self) -> Result<u64, StoreError> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM sms_queue WHERE state=?1",
            [QUEUED],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }
    /// Returns one page of messages, newest first, and the number of messages that match.
    pub(crate) fn operation_messages_page(
        &self,
        filter: &MessageFilter,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<crate::operations::proto::Sms>, u64), StoreError> {
        let state = filter.phase.map(|phase| match phase {
            crate::operations::proto::SmsPhase::Queued => QUEUED,
            crate::operations::proto::SmsPhase::Delivered => DELIVERED,
            crate::operations::proto::SmsPhase::Failed => FAILED,
            crate::operations::proto::SmsPhase::Unspecified => -1,
        });
        let total: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM sms_queue q WHERE (?1 IS NULL OR q.id < ?1) AND (?2 IS NULL OR q.state = ?2) AND (?3 IS NULL OR q.sender_number = ?3 OR q.recipient_number = ?3)",
            params![filter.before_id, state, filter.number],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare("SELECT q.id, q.sender_number, q.recipient_number, q.state, q.created_at_unix_ms, q.sms_pdu, q.relay_accepted, EXISTS(SELECT 1 FROM sms_receipts WHERE original_id=q.id AND acknowledgment_kind=1), EXISTS(SELECT 1 FROM sms_receipts WHERE original_id=q.id AND acknowledgment_kind=2), (SELECT (SELECT COUNT(*) FROM tsar_outgoing_segments seg WHERE seg.stream_id=j.id) FROM tsar_outgoing j WHERE j.sms_id=q.id AND j.retired_at_unix_ms IS NOT NULL) FROM sms_queue q LEFT JOIN subscribers s ON s.identity_type=q.recipient_type AND s.identity_value=q.recipient_value WHERE (?2 IS NULL OR q.id < ?2) AND (?3 IS NULL OR q.state = ?3) AND (?4 IS NULL OR q.sender_number = ?4 OR q.recipient_number = ?4) ORDER BY q.id DESC LIMIT ?1 OFFSET ?5")?;
        let rows = statement.query_map(
            params![
                limit as i64,
                filter.before_id,
                state,
                filter.number,
                offset as i64
            ],
            |row| {
                Ok(crate::operations::proto::Sms {
                    id: row.get(0)?,
                    sender: row.get(1)?,
                    recipient: row.get(2)?,
                    phase: match row.get::<_, i64>(3)? {
                        QUEUED => crate::operations::proto::SmsPhase::Queued,
                        DELIVERED => crate::operations::proto::SmsPhase::Delivered,
                        FAILED => crate::operations::proto::SmsPhase::Failed,
                        _ => crate::operations::proto::SmsPhase::Unspecified,
                    } as i32,
                    created_at_unix_ms: row.get(4)?,
                    pdu: row.get(5)?,
                    relay_accepted: row.get(6)?,
                    delivery_acknowledged: row.get(7)?,
                    manual_acknowledged: row.get(8)?,
                    measurements: row
                        .get::<_, Option<u32>>(9)?
                        .map_or_else(Vec::new, |segments| {
                            vec![
                                crate::operations::metric("Segmentation state", "Retired", ""),
                                crate::operations::metric("Segments", segments, ""),
                            ]
                        }),
                    ..Default::default()
                })
            },
        )?;
        let mut messages = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for message in &mut messages {
            let decoded =
                sms::decode_prefix(&message.pdu, Direction::Forward).map_err(StoreError::Sms)?;
            match decoded.message {
                SmsMessage::Deliver {
                    metadata,
                    user_data,
                    ..
                }
                | SmsMessage::Submit {
                    metadata,
                    user_data,
                } => {
                    message.message_reference = u32::from(metadata.message_reference);
                    message.text = user_data.text();
                }
                SmsMessage::DeliveryAck {
                    message_reference, ..
                }
                | SmsMessage::ManualAck {
                    message_reference, ..
                } => {
                    message.message_reference = u32::from(message_reference);
                }
            }
            if !self.append_hub_details(message)? {
                Self::operation_details(message);
            }
        }
        Ok((messages, total as u64))
    }
    fn operation_details(message: &mut crate::operations::proto::Sms) {
        use crate::operations::{metric, unavailable};
        let control_relay = crate::l3::relay::addressing::AddressedRelay {
            message: crate::l3::relay::RelayMessage::Data {
                transaction_id: 0,
                protocol: HigherLayerProtocol::Sms,
                payload: message.pdu.clone(),
            },
            addresses: crate::l3::relay::addressing::RelayAddresses {
                originating: Some(crate::l3::address::AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: crate::l3::address::AddressValue::Tbcd(message.sender.clone()),
                }),
                ..Default::default()
            },
        };
        let control_supported = control_relay.encode(Direction::Forward).is_ok();
        let traffic_supported = crate::l3::legacy_traffic::TrafficRelay::from_addressed(
            &control_relay,
        )
        .is_ok_and(|relay| {
            crate::link::traffic::encode(&crate::l3::legacy_traffic::TrafficMessage::Relay(relay))
                .is_ok()
        });
        let requirement = match (control_supported, traffic_supported) {
            (false, false) => Some("PV1+ segmented SMS"),
            (false, true) => Some("Active call"),
            (true, false) => Some("Idle phone"),
            (true, true) => None,
        };
        if let Some(requirement) = requirement {
            message
                .measurements
                .push(metric("Delivery channel requirement", requirement, ""));
        }
        message
            .measurements
            .push(unavailable("Retry count", "Retry history is not persisted"));
        message.measurements.push(unavailable(
            "Expiry",
            "Local messages have indefinite validity",
        ));
    }
    pub fn enqueue(&mut self, sms: NewSms<'_>) -> Result<SmsId, StoreError> {
        self.enqueue_for(sms, EnqueueScope::SingleRelay, None, None)
    }
    pub(crate) fn enqueue_teleservice(&mut self, sms: NewSms<'_>) -> Result<SmsId, StoreError> {
        self.enqueue_for(sms, EnqueueScope::CompleteTeleservice, None, None)
    }
    pub(crate) fn enqueue_bound_teleservice(
        &mut self,
        sms: NewSms<'_>,
        number: &str,
        sender_number: Option<&str>,
    ) -> Result<SmsId, StoreError> {
        self.enqueue_for(
            sms,
            EnqueueScope::CompleteTeleservice,
            Some(number),
            sender_number,
        )
    }
    pub fn enqueue_operator(&mut self, sms: NewOperatorSms<'_>) -> Result<SmsId, StoreError> {
        self.enqueue_origin(
            EnqueueRequest {
                dedup_key: sms.dedup_key,
                origin: SmsOrigin::Operator(sms.sender_number),
                recipient: sms.recipient,
                message: sms.message,
                created_at_unix_ms: sms.created_at_unix_ms,
            },
            EnqueueScope::SingleRelay,
            None,
            Some(sms.sender_number),
        )
    }
    fn enqueue_for(
        &mut self,
        sms: NewSms<'_>,
        scope: EnqueueScope,
        expected_recipient_number: Option<&str>,
        expected_sender_number: Option<&str>,
    ) -> Result<SmsId, StoreError> {
        self.enqueue_origin(
            EnqueueRequest {
                dedup_key: sms.dedup_key,
                origin: SmsOrigin::Subscriber(sms.sender),
                recipient: sms.recipient,
                message: sms.message,
                created_at_unix_ms: sms.created_at_unix_ms,
            },
            scope,
            expected_recipient_number,
            expected_sender_number,
        )
    }
    fn enqueue_origin(
        &mut self,
        sms: EnqueueRequest<'_>,
        scope: EnqueueScope,
        expected_recipient_number: Option<&str>,
        expected_sender_number: Option<&str>,
    ) -> Result<SmsId, StoreError> {
        if !matches!(sms.message, SmsMessage::Deliver { .. }) {
            return Err(StoreError::MessageType);
        }
        if sms.dedup_key.is_empty() {
            return Err(StoreError::DedupKey);
        }
        if sms.created_at_unix_ms < 0 {
            return Err(StoreError::Timestamp);
        }
        let payload = sms.message.encode().map_err(StoreError::Sms)?;
        let dedup_key = submission_dedup_key(sms.dedup_key);
        let sender = match sms.origin {
            SmsOrigin::Subscriber(identity) => Some(identity.parts()),
            SmsOrigin::Operator(_) => None,
        };
        let sender_kind = sender.map(|parts| parts.0);
        let sender_value = sender.map(|parts| parts.2 as i64);
        let (recipient_kind, _, recipient_value) = sms.recipient.parts();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction.query_row("SELECT id, sender_type, sender_value, recipient_type, recipient_value, sms_pdu, sender_number, recipient_number FROM sms_queue WHERE dedup_key = ?1",
            [&dedup_key], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<u8>>(1)?, row.get::<_, Option<i64>>(2)?, row.get::<_, u8>(3)?, row.get::<_, i64>(4)?, row.get::<_, Vec<u8>>(5)?, row.get::<_, String>(6)?, row.get::<_, String>(7)?))).optional()?;
        if let Some((
            id,
            old_sender_kind,
            old_sender,
            old_recipient_kind,
            old_recipient,
            old_payload,
            old_sender_number,
            old_recipient_number,
        )) = existing
        {
            if (
                old_sender_kind,
                old_sender,
                old_recipient_kind,
                old_recipient,
                old_payload.as_slice(),
            ) != (
                sender_kind,
                sender_value,
                recipient_kind,
                recipient_value as i64,
                payload.as_slice(),
            ) {
                return Err(StoreError::IdempotencyConflict);
            }
            if expected_sender_number.is_some_and(|number| number != old_sender_number)
                || expected_recipient_number.is_some_and(|number| number != old_recipient_number)
            {
                return Err(StoreError::IdempotencyConflict);
            }
            return Ok(SmsId(id));
        }
        let sender_number = match sms.origin {
            SmsOrigin::Subscriber(identity) => {
                let (kind, _, value) = identity.parts();
                enabled_number(&transaction, kind, value)?
            }
            SmsOrigin::Operator(number) => number.to_owned(),
        };
        if expected_sender_number.is_some_and(|number| number != sender_number) {
            return Err(StoreError::SenderMismatch);
        }
        let recipient_number = enabled_number(&transaction, recipient_kind, recipient_value)?;
        if expected_recipient_number.is_some_and(|number| number != recipient_number) {
            return Err(StoreError::RecipientMismatch);
        }
        let originating = AddressInfo {
            number_type: ORIGINATING_NUMBER_TYPE,
            numbering_plan: TELEPHONY_PLAN,
            address: AddressValue::Tbcd(sender_number.clone()),
        };
        match scope {
            EnqueueScope::SingleRelay => validate_delivery_relay(&AddressedRelay {
                message: RelayMessage::Data {
                    transaction_id: 0,
                    protocol: HigherLayerProtocol::Sms,
                    payload: payload.clone(),
                },
                addresses: RelayAddresses {
                    originating: Some(originating),
                    ..RelayAddresses::default()
                },
            })?,
            EnqueueScope::CompleteTeleservice => {
                originating
                    .encode()
                    .map_err(|error| StoreError::Addressing(AddressingError::Address(error)))?;
            }
        }
        transaction.execute("INSERT INTO sms_queue(dedup_key, sender_type, sender_value, recipient_type, recipient_value, sender_number, sms_pdu, created_at_unix_ms, recipient_number) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![dedup_key, sender_kind, sender_value, recipient_kind, recipient_value as i64, sender_number, payload, sms.created_at_unix_ms, recipient_number])?;
        let id = SmsId(transaction.last_insert_rowid());
        transaction.commit()?;
        Ok(id)
    }
    pub fn pending_for(&self, recipient: MobileIdentity) -> Result<Vec<QueuedSms>, StoreError> {
        let (kind, _, value) = recipient.parts();
        let mut statement = self.connection.prepare("SELECT id, sender_number, sms_pdu, created_at_unix_ms FROM sms_queue WHERE recipient_type = ?1 AND recipient_value = ?2 AND state = ?3 AND relay_accepted = 0 ORDER BY id")?;
        let rows = statement.query_map(params![kind, value as i64, QUEUED], |row| {
            Ok(QueuedSms {
                id: SmsId(row.get(0)?),
                sender_number: row.get(1)?,
                sms_pdu: row.get(2)?,
                created_at_unix_ms: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
    pub fn receipt_candidates_for(
        &self,
        recipient: MobileIdentity,
    ) -> Result<Vec<QueuedSms>, StoreError> {
        let (kind, _, value) = recipient.parts();
        let mut statement = self.connection.prepare("SELECT id, sender_number, sms_pdu, created_at_unix_ms FROM sms_queue WHERE recipient_type = ?1 AND recipient_value = ?2 AND state IN (?3, ?4) ORDER BY id")?;
        let rows = statement.query_map(params![kind, value as i64, QUEUED, DELIVERED], |row| {
            Ok(QueuedSms {
                id: SmsId(row.get(0)?),
                sender_number: row.get(1)?,
                sms_pdu: row.get(2)?,
                created_at_unix_ms: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn record_delivery_ack(
        &mut self,
        original: SmsId,
        actor: MobileIdentity,
        acknowledgment: &SmsMessage,
        created_at_unix_ms: i64,
    ) -> Result<SmsId, StoreError> {
        if !matches!(acknowledgment, SmsMessage::DeliveryAck { .. }) {
            return Err(StoreError::MessageType);
        }
        self.record_acknowledgment(original, actor, acknowledgment, created_at_unix_ms)
    }
    pub fn record_manual_ack(
        &mut self,
        original: SmsId,
        actor: MobileIdentity,
        acknowledgment: &SmsMessage,
        created_at_unix_ms: i64,
    ) -> Result<SmsId, StoreError> {
        if !matches!(acknowledgment, SmsMessage::ManualAck { .. }) {
            return Err(StoreError::MessageType);
        }
        self.record_acknowledgment(original, actor, acknowledgment, created_at_unix_ms)
    }
    pub(crate) fn record_acknowledgment(
        &mut self,
        original: SmsId,
        actor: MobileIdentity,
        acknowledgment: &SmsMessage,
        created_at_unix_ms: i64,
    ) -> Result<SmsId, StoreError> {
        let AcknowledgmentReference {
            kind,
            message_reference,
        } = AcknowledgmentKind::from_message(acknowledgment)?;
        if created_at_unix_ms < 0 {
            return Err(StoreError::Timestamp);
        }
        let payload = acknowledgment.encode().map_err(StoreError::Sms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (sender_kind, sender_value, recipient_kind, recipient_value, original_pdu, state) = transaction.query_row(
            "SELECT sender_type, sender_value, recipient_type, recipient_value, sms_pdu, state FROM sms_queue WHERE id = ?1", [original.0],
            |row| Ok((row.get::<_, Option<u8>>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, u8>(2)?, row.get::<_, i64>(3)?, row.get::<_, Vec<u8>>(4)?, row.get::<_, i64>(5)?))).optional()?.ok_or(StoreError::MissingMessage)?;
        let (Some(sender_kind), Some(sender_value)) = (sender_kind, sender_value) else {
            return Err(StoreError::ReceiptMismatch);
        };
        let (actor_kind, _, actor_value) = actor.parts();
        if (actor_kind, actor_value as i64) != (recipient_kind, recipient_value) {
            return Err(StoreError::ReceiptMismatch);
        }
        let SmsMessage::Deliver { metadata, .. } =
            sms::decode_prefix(&original_pdu, Direction::Forward)
                .map_err(StoreError::Sms)?
                .message
        else {
            return Err(StoreError::ReceiptMismatch);
        };
        if metadata.message_reference != message_reference || !kind.requested(&metadata) {
            return Err(StoreError::ReceiptMismatch);
        }
        let existing = transaction.query_row("SELECT sms_receipts.receipt_id, sms_queue.sms_pdu FROM sms_receipts JOIN sms_queue ON sms_queue.id = sms_receipts.receipt_id WHERE original_id = ?1 AND acknowledgment_kind = ?2", params![original.0, kind as i64],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))).optional()?;
        if let Some((id, previous)) = existing {
            if previous != payload {
                return Err(StoreError::IdempotencyConflict);
            }
            return Ok(SmsId(id));
        }
        if !matches!(state, QUEUED | DELIVERED) {
            return Err(StoreError::InvalidTransition);
        }
        let originating_number = enabled_number(&transaction, actor_kind, actor_value)?;
        validate_delivery_relay(&AddressedRelay {
            message: RelayMessage::Data {
                transaction_id: 0,
                protocol: HigherLayerProtocol::Sms,
                payload: payload.clone(),
            },
            addresses: RelayAddresses {
                originating: Some(AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: AddressValue::Tbcd(originating_number.clone()),
                }),
                ..RelayAddresses::default()
            },
        })?;
        let key = format!("acknowledgment:{}:{}", original.0, kind as i64);
        transaction.execute("INSERT INTO sms_queue(dedup_key, sender_type, sender_value, recipient_type, recipient_value, sender_number, sms_pdu, created_at_unix_ms, recipient_number) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![key, recipient_kind, recipient_value, sender_kind, sender_value, originating_number, payload, created_at_unix_ms, enabled_number(&transaction, sender_kind, u64::try_from(sender_value).map_err(|_| StoreError::CorruptState)?)?])?;
        let receipt = SmsId(transaction.last_insert_rowid());
        transaction.execute(
            "INSERT INTO sms_receipts(original_id, acknowledgment_kind, receipt_id) VALUES (?1, ?2, ?3)",
            params![original.0, kind as i64, receipt.0],
        )?;
        transaction.execute(
            "UPDATE sms_queue SET state = ?1 WHERE id = ?2",
            params![DELIVERED, original.0],
        )?;
        transaction.commit()?;
        Ok(receipt)
    }

    /// The caller must match the received R-Transaction Identifier to this queued message.
    pub fn record_relay_accept(
        &mut self,
        id: SmsId,
        recipient: MobileIdentity,
    ) -> Result<QueueState, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = record_relay_accept_in(&transaction, id, recipient)?;
        transaction.commit()?;
        Ok(state)
    }
    pub fn relay_accepted(&self, id: SmsId) -> Result<bool, StoreError> {
        self.connection
            .query_row(
                "SELECT relay_accepted FROM sms_queue WHERE id = ?1",
                [id.0],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::MissingMessage)
    }

    pub fn state(&self, id: SmsId) -> Result<QueueState, StoreError> {
        let state = self
            .connection
            .query_row("SELECT state FROM sms_queue WHERE id = ?1", [id.0], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .ok_or(StoreError::MissingMessage)?;
        match state {
            QUEUED => Ok(QueueState::Queued),
            DELIVERED => Ok(QueueState::Delivered),
            FAILED => Ok(QueueState::Failed),
            _ => Err(StoreError::CorruptState),
        }
    }
    pub fn mark_delivered(&self, id: SmsId) -> Result<(), StoreError> {
        self.finish(id, DELIVERED)
    }
    pub fn mark_failed(&self, id: SmsId) -> Result<(), StoreError> {
        self.finish(id, FAILED)
    }
    fn finish(&self, id: SmsId, target: i64) -> Result<(), StoreError> {
        let changed = self.connection.execute(
            "UPDATE sms_queue SET state = ?1 WHERE id = ?2 AND state IN (?3, ?1)",
            params![target, id.0, QUEUED],
        )?;
        if changed == 0 {
            return Err(StoreError::InvalidTransition);
        }
        Ok(())
    }
}
fn enabled_number(connection: &Connection, kind: u8, value: u64) -> Result<String, StoreError> {
    let row = connection.query_row("SELECT number, enabled FROM subscribers WHERE identity_type = ?1 AND identity_value = ?2", params![kind, value as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))).optional()?.ok_or(StoreError::MissingSubscriber)?;
    if !row.1 {
        return Err(StoreError::DisabledSubscriber);
    }
    Ok(row.0)
}

fn validate_delivery_relay(relay: &AddressedRelay) -> Result<(), StoreError> {
    if relay.encode(Direction::Forward).is_ok() {
        return Ok(());
    }
    TrafficRelay::from_addressed(relay)
        .map(|_| ())
        .map_err(StoreError::Traffic)
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_twelve_upgrade_preserves_sms_receipts_and_idempotency() {
        let database = Database::new();
        let payload = message(b"retained");
        let connection = Connection::open(&database.0).unwrap();
        for migration in [
            INITIAL_SCHEMA,
            RECEIPT_SCHEMA,
            MANUAL_RECEIPT_SCHEMA,
            RELAY_ACCEPTANCE_SCHEMA,
            MANAGEMENT_SCHEMA,
            INCOMING_TSAR_SCHEMA,
            INCOMING_RESPONSE_SCHEMA,
            OUTGOING_TSAR_SCHEMA,
            OUTGOING_RETIREMENT_SCHEMA,
            ORDINARY_REPLAY_SCHEMA,
            BROADCAST_SCHEMA,
            ESN_SCHEMA,
        ] {
            connection.execute_batch(migration).unwrap();
        }
        for subscriber in subscribers() {
            let (kind, _, value) = subscriber.identity.parts();
            connection.execute("INSERT INTO subscribers(identity_type,identity_value,number,enabled) VALUES(?1,?2,?3,1)",params![kind,value as i64,subscriber.number]).unwrap();
        }
        for key in [
            "submit:before-operator-migration",
            "receipt:before-operator-migration",
        ] {
            connection.execute("INSERT INTO sms_queue(dedup_key,sender_type,sender_value,recipient_type,recipient_value,sender_number,sms_pdu,created_at_unix_ms,recipient_number) VALUES(?1,2,1,2,2,'1001',?2,1000,'1002')", params![key,payload.encode().unwrap()]).unwrap();
        }
        connection
            .execute(
                "INSERT INTO sms_receipts(original_id,acknowledgment_kind,receipt_id) VALUES(1,1,2)",
                [],
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", ESN_SCHEMA_VERSION)
            .unwrap();
        drop(connection);
        let mut store = StateStore::open(&database.0).unwrap();
        assert_eq!(
            store
                .enqueue(item("before-operator-migration", &payload))
                .unwrap(),
            SmsId(1)
        );
        assert_eq!(
            store.pending_for(MobileIdentity::Min34(2)).unwrap().len(),
            2
        );
        let receipt: i64 = store
            .connection
            .query_row(
                "SELECT receipt_id FROM sms_receipts WHERE original_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipt, 2);
        assert!(
            store
                .connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn operator_origins_survive_restart_without_subscriber_records() {
        let database = Database::new();
        let payload = message(b"external sender");
        let request = NewOperatorSms {
            dedup_key: "operator-origin",
            sender_number: "18005551234",
            recipient: MobileIdentity::Min34(2),
            message: &payload,
            created_at_unix_ms: 1000,
        };
        let id = {
            let mut store = StateStore::open(&database.0).unwrap();
            store.reconcile(&subscribers()[1..]).unwrap();
            let id = store.enqueue_operator(request).unwrap();
            assert_eq!(store.enabled_subscribers().unwrap().len(), 1);
            id
        };
        let mut store = StateStore::open(&database.0).unwrap();
        let queued = store.pending_for(MobileIdentity::Min34(2)).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, id);
        assert_eq!(queued[0].sender_number, "18005551234");
        assert_eq!(
            store
                .enqueue_operator(NewOperatorSms {
                    dedup_key: "operator-origin",
                    sender_number: "18005551234",
                    recipient: MobileIdentity::Min34(2),
                    message: &payload,
                    created_at_unix_ms: 1000,
                })
                .unwrap(),
            id
        );
        assert!(matches!(
            store.enqueue_operator(NewOperatorSms {
                dedup_key: "operator-origin",
                sender_number: "9999",
                recipient: MobileIdentity::Min34(2),
                message: &payload,
                created_at_unix_ms: 1000,
            }),
            Err(StoreError::IdempotencyConflict)
        ));
        assert_eq!(store.enabled_subscribers().unwrap().len(), 1);
    }

    #[test]
    fn sqlite_provisioning_survives_restart_and_removed_numbers_can_be_reused() {
        let database = Database::new();
        let initial = subscribers();
        {
            let mut store = StateStore::open(&database.0).unwrap();
            store.seed_once(&initial).unwrap();
            store.disable_subscriber(initial[0].identity).unwrap();
            store
                .save_subscriber(&ProvisionedSubscriber {
                    identity: MobileIdentity::Min34(3),
                    number: initial[0].number.clone(),
                })
                .unwrap();
        }
        let mut store = StateStore::open(&database.0).unwrap();
        store.seed_once(&initial).unwrap();
        let active = store.enabled_subscribers().unwrap();
        assert_eq!(active.len(), 2);
        assert!(
            active
                .iter()
                .any(|subscriber| subscriber.identity == MobileIdentity::Min34(3))
        );
        assert!(
            !active
                .iter()
                .any(|subscriber| subscriber.identity == initial[0].identity)
        );
    }

    #[test]
    fn subscriber_edits_preserve_sms_directory_history() {
        let database = Database::new();
        let mut store = StateStore::open(&database.0).unwrap();
        store.seed_once(&subscribers()).unwrap();
        store
            .enqueue(item("history", &message(b"retained")))
            .unwrap();
        store
            .save_subscriber(&ProvisionedSubscriber {
                identity: MobileIdentity::Min34(2),
                number: "2002".into(),
            })
            .unwrap();
        store.disable_subscriber(MobileIdentity::Min34(2)).unwrap();
        let messages = store.operation_messages(10).unwrap();
        assert_eq!(messages[0].recipient, "1002");
        assert_eq!(messages[0].sender, "1001");
    }
    use super::*;
    use crate::l3::sms::{self, DisplayTime, UserData, Validity};
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct Database(std::path::PathBuf);
    impl Database {
        fn new() -> Self {
            Self::with_stamp(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            )
        }
        fn with_stamp(stamp: u128) -> Self {
            loop {
                let sequence = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "136bts-state-{}-{stamp}-{sequence}.sqlite3",
                    std::process::id()
                ));
                match std::fs::File::create_new(&path) {
                    Ok(_) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("creating store test database failed: {error}"),
                }
            }
        }
    }
    #[test]
    fn identical_clock_timestamps_cannot_share_or_remove_another_test_database() {
        let first = Database::with_stamp(0);
        let second = Database::with_stamp(0);
        assert_ne!(first.0, second.0);
        drop(first);
        assert!(StateStore::open(&second.0).is_ok());
        assert!(second.0.is_file());
    }
    impl Drop for Database {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn subscribers() -> Vec<ProvisionedSubscriber> {
        vec![
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(1),
                number: "1001".into(),
            },
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(2),
                number: "1002".into(),
            },
        ]
    }
    fn message(text: &[u8]) -> SmsMessage {
        SmsMessage::Deliver {
            metadata: sms::Metadata {
                message_reference: 42,
                privacy: 0,
                urgency: 1,
                delivery_acknowledgment: true,
                manual_acknowledgment: false,
                new_message: true,
            },
            validity: Validity::Indefinite,
            display_time: DisplayTime::Default,
            user_data: UserData::Ira(text.to_vec()),
        }
    }
    fn item<'a>(key: &'a str, message: &'a SmsMessage) -> NewSms<'a> {
        NewSms {
            dedup_key: key,
            sender: MobileIdentity::Min34(1),
            recipient: MobileIdentity::Min34(2),
            message,
            created_at_unix_ms: 1000,
        }
    }
    #[test]
    fn queue_and_terminal_state_survive_reopen_and_repeated_requests_are_idempotent() {
        let database = Database::new();
        let payload = message(b"persistent message");
        let id = {
            let mut store = StateStore::open(&database.0).unwrap();
            store.reconcile(&subscribers()).unwrap();
            let id = store.enqueue(item("request-1", &payload)).unwrap();
            assert_eq!(store.enqueue(item("request-1", &payload)).unwrap(), id);
            assert_eq!(store.state(id).unwrap(), QueueState::Queued);
            id
        };
        {
            let mut store = StateStore::open(&database.0).unwrap();
            let pending = store.pending_for(MobileIdentity::Min34(2)).unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].id, id);
            assert_eq!(pending[0].sender_number, "1001");
            assert_eq!(
                sms::decode_prefix(&pending[0].sms_pdu, Direction::Forward)
                    .unwrap()
                    .message,
                payload
            );
            store.mark_delivered(id).unwrap();
            store.mark_delivered(id).unwrap();
            assert!(matches!(
                store.mark_failed(id),
                Err(StoreError::InvalidTransition)
            ));
            assert_eq!(store.enqueue(item("request-1", &payload)).unwrap(), id);
            assert!(
                store
                    .pending_for(MobileIdentity::Min34(2))
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(
            StateStore::open(&database.0).unwrap().state(id).unwrap(),
            QueueState::Delivered
        );
    }
    #[test]
    fn reconciliation_rolls_back_conflicts_and_preserves_queue_for_disabled_subscriber() {
        let database = Database::new();
        let mut store = StateStore::open(&database.0).unwrap();
        let initial = subscribers();
        store.reconcile(&initial).unwrap();
        store
            .enqueue(item("queued", &message(b"retained")))
            .unwrap();
        let conflicting = [
            initial[0].clone(),
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(3),
                number: "1001".into(),
            },
        ];
        assert!(matches!(
            store.reconcile(&conflicting),
            Err(StoreError::Registry(_))
        ));
        assert_eq!(store.enabled_subscribers().unwrap(), initial);
        store.reconcile(&initial[..1]).unwrap();
        assert_eq!(store.enabled_subscribers().unwrap(), initial[..1]);
        assert_eq!(
            store.pending_for(MobileIdentity::Min34(2)).unwrap().len(),
            1
        );
        assert!(matches!(
            store.enqueue(item("new", &message(b"blocked"))),
            Err(StoreError::DisabledSubscriber)
        ));
        store.reconcile(&initial).unwrap();
        assert_eq!(
            store.pending_for(MobileIdentity::Min34(2)).unwrap().len(),
            1
        );
    }
    #[test]
    fn idempotency_conflicts_and_untransmittable_payloads_do_not_create_queue_entries() {
        let database = Database::new();
        let mut store = StateStore::open(&database.0).unwrap();
        store.reconcile(&subscribers()).unwrap();
        store
            .enqueue(item("same-key", &message(b"original")))
            .unwrap();
        assert!(matches!(
            store.enqueue(item("same-key", &message(b"different"))),
            Err(StoreError::IdempotencyConflict)
        ));
        assert!(matches!(
            store.enqueue(item("oversized", &message(&[b'x'; 254]))),
            Err(StoreError::Traffic(_))
        ));
        assert_eq!(
            store.pending_for(MobileIdentity::Min34(2)).unwrap().len(),
            1
        );
    }
    #[test]
    fn version_one_upgrade_preserves_payload_and_idempotency_key() {
        let database = Database::new();
        let payload = message(b"before migration");
        let connection = Connection::open(&database.0).unwrap();
        connection.execute_batch(INITIAL_SCHEMA).unwrap();
        for subscriber in subscribers() {
            let (kind, _, value) = subscriber.identity.parts();
            connection.execute("INSERT INTO subscribers(identity_type, identity_value, number, enabled) VALUES (?1, ?2, ?3, 1)", params![kind, value as i64, subscriber.number]).unwrap();
        }
        connection.execute("INSERT INTO sms_queue(dedup_key, sender_type, sender_value, recipient_type, recipient_value, sender_number, deliver_pdu, created_at_unix_ms) VALUES ('old-key', 2, 1, 2, 2, '1001', ?1, 1000)", [payload.encode().unwrap()]).unwrap();
        let id = SmsId(connection.last_insert_rowid());
        connection
            .pragma_update(None, "user_version", FIRST_SCHEMA_VERSION)
            .unwrap();
        drop(connection);
        let mut store = StateStore::open(&database.0).unwrap();
        let pending = store.pending_for(MobileIdentity::Min34(2)).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].sms_pdu, payload.encode().unwrap());
        assert_eq!(store.enqueue(item("old-key", &payload)).unwrap(), id);
        assert_eq!(
            store.pending_for(MobileIdentity::Min34(2)).unwrap().len(),
            1
        );
    }
    #[test]
    fn receipt_actor_reference_and_repeated_content_are_validated() {
        let database = Database::new();
        let mut store = StateStore::open(&database.0).unwrap();
        store.reconcile(&subscribers()).unwrap();
        let original = store.enqueue(item("receipt", &message(b"hello"))).unwrap();
        let acknowledgment = SmsMessage::DeliveryAck {
            message_reference: 42,
            user_data: UserData::Ira(Vec::new()),
        };
        assert!(matches!(
            store.record_delivery_ack(original, MobileIdentity::Min34(1), &acknowledgment, 0),
            Err(StoreError::ReceiptMismatch)
        ));
        let wrong = SmsMessage::DeliveryAck {
            message_reference: 43,
            user_data: UserData::Ira(Vec::new()),
        };
        assert!(matches!(
            store.record_delivery_ack(original, MobileIdentity::Min34(2), &wrong, 0),
            Err(StoreError::ReceiptMismatch)
        ));
        assert_eq!(store.state(original).unwrap(), QueueState::Queued);
        let receipt = store
            .record_delivery_ack(original, MobileIdentity::Min34(2), &acknowledgment, 0)
            .unwrap();
        assert_eq!(
            store
                .record_delivery_ack(original, MobileIdentity::Min34(2), &acknowledgment, 1)
                .unwrap(),
            receipt
        );
        let conflict = SmsMessage::DeliveryAck {
            message_reference: 42,
            user_data: UserData::Ira(b"changed".to_vec()),
        };
        assert!(matches!(
            store.record_delivery_ack(original, MobileIdentity::Min34(2), &conflict, 1),
            Err(StoreError::IdempotencyConflict)
        ));
        assert_eq!(
            store.pending_for(MobileIdentity::Min34(1)).unwrap().len(),
            1
        );
    }

    #[test]
    fn version_two_upgrade_retains_delivery_receipt_link_and_allows_manual_receipt() {
        let database = Database::new();
        let mut payload = message(b"both requested");
        let SmsMessage::Deliver { metadata, .. } = &mut payload else {
            unreachable!()
        };
        metadata.manual_acknowledgment = true;
        let delivery = SmsMessage::DeliveryAck {
            message_reference: 42,
            user_data: UserData::Ira(Vec::new()),
        };
        let connection = Connection::open(&database.0).unwrap();
        connection.execute_batch(INITIAL_SCHEMA).unwrap();
        connection.execute_batch(RECEIPT_SCHEMA).unwrap();
        for subscriber in subscribers() {
            let (kind, _, value) = subscriber.identity.parts();
            connection.execute("INSERT INTO subscribers(identity_type, identity_value, number, enabled) VALUES (?1, ?2, ?3, 1)", params![kind, value as i64, subscriber.number]).unwrap();
        }
        connection.execute("INSERT INTO sms_queue(dedup_key, sender_type, sender_value, recipient_type, recipient_value, sender_number, sms_pdu, created_at_unix_ms, state) VALUES ('submit:old', 2, 1, 2, 2, '1001', ?1, 0, 1)", [payload.encode().unwrap()]).unwrap();
        let original = SmsId(connection.last_insert_rowid());
        connection.execute("INSERT INTO sms_queue(dedup_key, sender_type, sender_value, recipient_type, recipient_value, sender_number, sms_pdu, created_at_unix_ms) VALUES ('delivery-receipt:old', 2, 2, 2, 1, '1002', ?1, 1)", [delivery.encode().unwrap()]).unwrap();
        let receipt = SmsId(connection.last_insert_rowid());
        connection
            .execute(
                "INSERT INTO sms_receipts(original_id, receipt_id) VALUES (?1, ?2)",
                params![original.0, receipt.0],
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", DELIVERY_SCHEMA_VERSION)
            .unwrap();
        drop(connection);
        let mut store = StateStore::open(&database.0).unwrap();
        assert_eq!(
            store
                .record_delivery_ack(original, MobileIdentity::Min34(2), &delivery, 2)
                .unwrap(),
            receipt
        );
        let manual = SmsMessage::ManualAck {
            message_reference: 42,
            user_data: UserData::Ira(b"read".to_vec()),
        };
        let manual_id = store
            .record_manual_ack(original, MobileIdentity::Min34(2), &manual, 2)
            .unwrap();
        assert_ne!(manual_id, receipt);
        assert_eq!(
            store.pending_for(MobileIdentity::Min34(1)).unwrap().len(),
            2
        );
        assert_eq!(store.state(original).unwrap(), QueueState::Delivered);
    }

    #[test]
    fn relay_acceptance_persists_and_requested_receipts_remain_matchable() {
        for (delivery_requested, manual_requested) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let database = Database::new();
            let mut store = StateStore::open(&database.0).unwrap();
            store.reconcile(&subscribers()).unwrap();
            let mut payload = message(b"transport acceptance");
            let SmsMessage::Deliver { metadata, .. } = &mut payload else {
                unreachable!()
            };
            metadata.delivery_acknowledgment = delivery_requested;
            metadata.manual_acknowledgment = manual_requested;
            let original = store.enqueue(item("relay", &payload)).unwrap();
            assert!(matches!(
                store.record_relay_accept(original, MobileIdentity::Min34(1)),
                Err(StoreError::RecipientMismatch)
            ));
            assert!(!store.relay_accepted(original).unwrap());
            assert_eq!(
                store.pending_for(MobileIdentity::Min34(2)).unwrap().len(),
                1
            );
            let expected = if delivery_requested || manual_requested {
                QueueState::Queued
            } else {
                QueueState::Delivered
            };
            assert_eq!(
                store
                    .record_relay_accept(original, MobileIdentity::Min34(2))
                    .unwrap(),
                expected
            );
            assert_eq!(
                store
                    .record_relay_accept(original, MobileIdentity::Min34(2))
                    .unwrap(),
                expected
            );
            drop(store);
            let mut store = StateStore::open(&database.0).unwrap();
            assert!(store.relay_accepted(original).unwrap());
            assert_eq!(store.state(original).unwrap(), expected);
            assert!(
                store
                    .pending_for(MobileIdentity::Min34(2))
                    .unwrap()
                    .is_empty()
            );
            if !delivery_requested && !manual_requested {
                continue;
            }
            assert!(
                store
                    .receipt_candidates_for(MobileIdentity::Min34(2))
                    .unwrap()
                    .iter()
                    .any(|candidate| candidate.id == original)
            );
            let acknowledgment = if delivery_requested {
                SmsMessage::DeliveryAck {
                    message_reference: 42,
                    user_data: UserData::Ira(Vec::new()),
                }
            } else {
                SmsMessage::ManualAck {
                    message_reference: 42,
                    user_data: UserData::Ira(b"read".to_vec()),
                }
            };
            let receipt = store
                .record_acknowledgment(original, MobileIdentity::Min34(2), &acknowledgment, 1)
                .unwrap();
            assert_eq!(
                store
                    .record_relay_accept(receipt, MobileIdentity::Min34(1))
                    .unwrap(),
                QueueState::Delivered
            );
            assert!(
                store
                    .pending_for(MobileIdentity::Min34(1))
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(store.state(original).unwrap(), QueueState::Delivered);
        }
    }
    #[test]
    fn failed_message_rejects_transport_acceptance_and_late_accept_keeps_receipt_completion() {
        let database = Database::new();
        let mut store = StateStore::open(&database.0).unwrap();
        store.reconcile(&subscribers()).unwrap();
        let failed = store.enqueue(item("failed", &message(b"failed"))).unwrap();
        store.mark_failed(failed).unwrap();
        assert!(matches!(
            store.record_relay_accept(failed, MobileIdentity::Min34(2)),
            Err(StoreError::InvalidTransition)
        ));
        assert!(!store.relay_accepted(failed).unwrap());
        let original = store
            .enqueue(item("receipt-first", &message(b"receipt first")))
            .unwrap();
        let receipt = store
            .record_delivery_ack(
                original,
                MobileIdentity::Min34(2),
                &SmsMessage::DeliveryAck {
                    message_reference: 42,
                    user_data: UserData::Ira(Vec::new()),
                },
                1,
            )
            .unwrap();
        assert_eq!(
            store
                .record_relay_accept(original, MobileIdentity::Min34(2))
                .unwrap(),
            QueueState::Delivered
        );
        assert_eq!(
            store.pending_for(MobileIdentity::Min34(1)).unwrap()[0].id,
            receipt
        );
    }
    #[test]
    fn version_three_upgrade_preserves_unaccepted_queue_and_receipt_records() {
        let database = Database::new();
        let mut store = StateStore::open(&database.0).unwrap();
        store.reconcile(&subscribers()).unwrap();
        let original = store
            .enqueue(item("pre-upgrade", &message(b"old version")))
            .unwrap();
        let acknowledgment = SmsMessage::DeliveryAck {
            message_reference: 42,
            user_data: UserData::Ira(Vec::new()),
        };
        let receipt = store
            .record_delivery_ack(original, MobileIdentity::Min34(2), &acknowledgment, 1)
            .unwrap();
        store
            .connection
            .execute_batch("DROP TABLE sms_hub_parts; DROP TABLE sms_hub_outbox; DROP TABLE sms_hub_inbox; DROP TABLE esn_subscribers; DROP TABLE broadcast_revisions; DROP TABLE ordinary_sms_requests; DROP TABLE tsar_outgoing_events; DROP TABLE tsar_outgoing_segments; DROP TABLE tsar_outgoing; DROP TABLE tsar_incoming_responses; DROP TABLE tsar_incoming_events; DROP TABLE tsar_incoming; ALTER TABLE sms_queue DROP COLUMN relay_accepted; ALTER TABLE sms_queue DROP COLUMN recipient_number; DROP TABLE provisioning_state")
            .unwrap();
        store
            .connection
            .pragma_update(None, "user_version", MANUAL_SCHEMA_VERSION)
            .unwrap();
        drop(store);
        let mut store = StateStore::open(&database.0).unwrap();
        assert!(!store.relay_accepted(receipt).unwrap());
        assert_eq!(
            store.pending_for(MobileIdentity::Min34(1)).unwrap()[0].id,
            receipt
        );
        assert_eq!(
            store
                .record_delivery_ack(original, MobileIdentity::Min34(2), &acknowledgment, 2)
                .unwrap(),
            receipt
        );
        assert_eq!(
            store
                .record_relay_accept(receipt, MobileIdentity::Min34(1))
                .unwrap(),
            QueueState::Delivered
        );
    }

    #[test]
    fn newer_schema_is_rejected_without_rewriting_existing_data() {
        let database = Database::new();
        let store = StateStore::open(&database.0).unwrap();
        store
            .connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(store);
        assert!(matches!(
            StateStore::open(&database.0),
            Err(StoreError::SchemaVersion(version)) if version == SCHEMA_VERSION + 1
        ));
        let connection = Connection::open(&database.0).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION + 1
        );
    }
    #[test]
    fn queue_retains_control_only_addressing_and_accepts_maximum_dtc_payload() {
        let database = Database::new();
        let mut store = StateStore::open(&database.0).unwrap();
        let mut directory = subscribers();
        directory[0].number = "1".repeat(130);
        store.reconcile(&directory).unwrap();
        store
            .enqueue(item(
                "control-only",
                &message(b"short with a long origin address"),
            ))
            .unwrap();
        store.reconcile(&subscribers()).unwrap();
        store
            .enqueue(item(
                "dtc-maximum",
                &message(&vec![b'x'; sms::MAX_SINGLE_RELAY_IRA_CHARACTERS]),
            ))
            .unwrap();
        let pending = store.pending_for(MobileIdentity::Min34(2)).unwrap();
        assert_eq!(pending.len(), 2);
        assert!(
            TrafficRelay::from_addressed(
                &crate::core::messaging::DeliveryOffer {
                    recipient: MobileIdentity::Min34(2),
                    queued: pending[0].clone()
                }
                .relay(0)
            )
            .is_err()
        );
        assert!(
            crate::core::messaging::DeliveryOffer {
                recipient: MobileIdentity::Min34(2),
                queued: pending[0].clone()
            }
            .relay(0)
            .encode(Direction::Forward)
            .is_ok()
        );
        assert_eq!(
            pending[1].sms_pdu.len(),
            crate::l3::relay::MAX_DATA_PAYLOAD_OCTETS
        );
    }
}
