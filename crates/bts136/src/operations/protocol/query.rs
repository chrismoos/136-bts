use super::ProtocolLog;
use crate::operations::proto;
use std::sync::atomic::Ordering;
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: u32 = 200;
const MAX_SEARCH_BYTES: usize = 256;
fn routine(message: &proto::WireMessage) -> bool {
    !message.identity.is_empty()
        && matches!(
            message.name.as_str(),
            "ARQ BEGIN"
                | "ARQ CONTINUE"
                | "SPACH ARQ status"
                | "Acknowledgment"
                | "Channel quality report"
                | "Extended channel quality report"
                | "Physical layer control"
                | "Physical layer control acknowledgment"
                | "Measurement order"
                | "Measurement order acknowledgment"
                | "Stop measurements"
                | "Extended measurement order"
                | "Extended measurement order acknowledgment"
                | "Comfort noise parameters"
                | "AMR implicit comfort noise"
        )
}
impl ProtocolLog {
    pub(crate) fn quality<T>(
        &self,
        identity: Option<crate::link::rach::MobileIdentity>,
        burst: &crate::phy::acquisition::AcquiredSlot<T>,
    ) {
        let Some(measured) = burst.quality else {
            return;
        };
        let quality = proto::ReverseQuality {
            sync_evm_percent: measured.sync_evm_percent,
            sync_level_dbfs: measured.sync_level_dbfs,
            frequency_offset_hz: burst.frequency_offset_hz,
            sync_correlation: burst.sync_correlation,
            measured_unix_ms: crate::operations::unix_ms(),
            ..Default::default()
        };
        let Ok(mut records) = self.records.try_lock() else {
            return;
        };
        let Some(identity) = identity else {
            return;
        };
        let identity = format!("{identity:?}");
        if let Some(index) = records.qualities.iter().position(|(id, _)| *id == identity) {
            records.qualities.remove(index);
        }
        if records.qualities.len() == super::MESSAGE_CAPACITY {
            records.qualities.pop_front();
        }
        records.qualities.push_back((identity, quality));
    }

    pub(crate) fn bit_errors(
        &self,
        identity: crate::link::rach::MobileIdentity,
        block: crate::phy::bit_errors::BitErrors,
    ) {
        let Ok(mut records) = self.records.try_lock() else {
            return;
        };
        let identity = format!("{identity:?}");
        let mut window = records
            .bit_errors
            .iter()
            .position(|(id, _)| *id == identity)
            .and_then(|index| records.bit_errors.remove(index))
            .map(|(_, window)| window)
            .unwrap_or_default();
        window.push(block);
        if records.bit_errors.len() == super::MESSAGE_CAPACITY {
            records.bit_errors.pop_front();
        }
        records.bit_errors.push_back((identity, window));
    }

    pub(crate) fn last_uplink_unix_ms(&self) -> Option<i64> {
        let records = self.records.lock().ok()?;
        records
            .observed
            .iter()
            .map(|phone| phone.last_response_unix_ms)
            .max()
    }

    /// Fills each subscriber's latest response and reverse quality, and returns the
    /// observed phones, newest first.
    pub(crate) fn annotate(
        &self,
        subscribers: &mut [proto::Subscriber],
    ) -> Vec<proto::ObservedPhone> {
        let mut observed = Vec::new();
        if let Ok(records) = self.records.lock() {
            observed = records.observed.iter().rev().cloned().collect();
            for phone in &mut observed {
                phone.reverse_quality = records.reverse_quality(&phone.identity);
            }
            for subscriber in subscribers.iter_mut() {
                subscriber.reverse_quality = records.reverse_quality(&subscriber.identity);
                subscriber.last_response_unix_ms = records
                    .observed
                    .iter()
                    .find(|phone| phone.identity == subscriber.identity)
                    .map(|phone| phone.last_response_unix_ms);
            }
        }
        observed
    }

    pub(crate) fn query(
        &self,
        query: proto::SignalingQuery,
    ) -> Result<proto::SignalingPage, tonic::Status> {
        let scope = proto::SignalingScope::try_from(query.scope)
            .map_err(|_| tonic::Status::invalid_argument("Unknown signaling view"))?;
        if query.limit > MAX_LIMIT
            || query.search.len() > MAX_SEARCH_BYTES
            || query.identity.len() > MAX_SEARCH_BYTES
            || !matches!(query.direction.as_str(), "" | "Forward" | "Reverse")
        {
            return Err(tonic::Status::invalid_argument("Invalid signaling filters"));
        }
        let limit = if query.limit == 0 {
            DEFAULT_LIMIT
        } else {
            query.limit as usize
        };
        let search = query.search.to_lowercase();
        let records = self
            .records
            .lock()
            .map_err(|_| tonic::Status::internal("Signaling log unavailable"))?;
        let mut messages: Vec<_> = records
            .messages
            .iter()
            .chain(&records.broadcasts)
            .filter(|message| {
                let in_scope = match scope {
                    proto::SignalingScope::PhoneActivity => !message.broadcast && !routine(message),
                    proto::SignalingScope::AllPhoneSignaling => !message.broadcast,
                    proto::SignalingScope::SystemBroadcasts => message.broadcast,
                };
                in_scope
                    && (query.identity.is_empty() || message.identity == query.identity)
                    && (query.direction.is_empty() || message.direction == query.direction)
                    && query
                        .before_sequence
                        .is_none_or(|sequence| message.sequence < sequence)
                    && (search.is_empty()
                        || format!(
                            "{} {} {} {}",
                            message.identity, message.channel, message.name, message.decoded
                        )
                        .to_lowercase()
                        .contains(&search))
            })
            .cloned()
            .collect();
        messages.sort_unstable_by_key(|message| std::cmp::Reverse(message.sequence));
        let total = messages.len() as u32;
        Ok(proto::SignalingPage {
            messages: messages
                .into_iter()
                .skip(query.offset as usize)
                .take(limit)
                .collect(),
            dropped_messages: self.dropped.load(Ordering::Relaxed),
            total,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn record(log: &ProtocolLog, name: &str, identity: &str, broadcast: bool) {
        log.record(proto::WireMessage {
            name: name.into(),
            identity: identity.into(),
            direction: "Reverse".into(),
            broadcast,
            raw: name.as_bytes().to_vec(),
            ..Default::default()
        });
    }
    #[test]
    fn phone_view_keeps_pages_and_unidentified_messages_and_excludes_routine_overhead() {
        let log = ProtocolLog::default();
        record(&log, "Registration", "Min34(7)", false);
        record(&log, "Page", "Min34(7)", false);
        record(&log, "Unknown", "", false);
        record(&log, "Acknowledgment", "Min34(7)", false);
        record(&log, "System information", "", true);
        let page = log.query(proto::SignalingQuery::default()).unwrap();
        assert_eq!(
            page.messages
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["Unknown", "Page", "Registration"]
        );
        assert_eq!(
            log.query(proto::SignalingQuery {
                scope: proto::SignalingScope::SystemBroadcasts as i32,
                ..Default::default()
            })
            .unwrap()
            .messages
            .len(),
            1
        );
        assert_eq!(
            log.query(proto::SignalingQuery {
                scope: proto::SignalingScope::AllPhoneSignaling as i32,
                identity: "Min34(7)".into(),
                ..Default::default()
            })
            .unwrap()
            .messages
            .len(),
            3
        );
    }
    #[test]
    fn offset_pages_stay_stable_under_a_sequence_anchor() {
        let log = ProtocolLog::default();
        for n in 0..5 {
            record(&log, &format!("Page {n}"), "Min34(7)", false);
        }
        let sequences = |page: proto::SignalingPage| {
            page.messages.iter().map(|m| m.sequence).collect::<Vec<_>>()
        };
        let first = log
            .query(proto::SignalingQuery {
                limit: 2,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(first.total, 5);
        assert_eq!(sequences(first), [5, 4]);
        record(&log, "Page 5", "Min34(7)", false);
        let second = log
            .query(proto::SignalingQuery {
                before_sequence: Some(6),
                offset: 2,
                limit: 2,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(second.total, 5);
        assert_eq!(sequences(second), [3, 2]);
        assert_eq!(
            log.query(proto::SignalingQuery {
                search: "page 5".into(),
                ..Default::default()
            })
            .unwrap()
            .total,
            1
        );
        assert!(
            log.query(proto::SignalingQuery {
                direction: "invalid".into(),
                ..Default::default()
            })
            .is_err()
        );
    }
}

#[cfg(test)]
mod quality_tests {
    use super::*;
    #[test]
    fn reverse_messages_keep_their_burst_measurement_while_phone_quality_updates() {
        let log = ProtocolLog::default();
        let identity = crate::link::rach::MobileIdentity::Min34(7);
        let mut burst = crate::phy::acquisition::AcquiredSlot {
            slot_start_sample: 0.0,
            sync_correlation: 0.99,
            frequency_offset_hz: 12.5,
            fields: (),
            hard_bits: [false; crate::phy::traffic::SLOT_BITS],
            quality: Some(crate::phy::acquisition::quality::BurstQuality {
                sync_evm_percent: 6.25,
                sync_level_dbfs: -24.0,
            }),
        };
        log.quality(Some(identity), &burst);
        log.message("Reverse", "RACH", identity, &[255], "RX decoded");
        burst.quality.as_mut().unwrap().sync_evm_percent = 8.0;
        log.quality(Some(identity), &burst);
        let page = log.query(proto::SignalingQuery::default()).unwrap();
        assert_eq!(
            page.messages[0].reverse_quality.unwrap().sync_evm_percent,
            6.25
        );
        let mut subscribers = vec![proto::Subscriber {
            identity: "Min34(7)".into(),
            ..Default::default()
        }];
        log.bit_errors(
            identity,
            crate::phy::bit_errors::BitErrors {
                corrected: 3,
                checked: 200,
            },
        );
        log.annotate(&mut subscribers);
        let quality = subscribers[0].reverse_quality.unwrap();
        assert_eq!(quality.sync_evm_percent, 8.0);
        assert_eq!(quality.bit_error_percent, Some(1.5));
        assert_eq!(quality.checked_bits, 200);
    }
}
