use std::{error::Error, fmt, sync::Arc};

use super::{
    sacch_tx::{self, SacchTx},
    traffic::{self, SignalingWord},
    traffic_tx::{self, TrafficTx},
    voice::TransmitFrame,
};
use crate::{
    l3::legacy_traffic::TrafficMessage,
    phy::{
        color,
        sync::SyncWord,
        traffic::{self as phy_traffic, SLOT_BITS, SlotError},
    },
};
const DEFAULT_QUEUE_WORDS: usize = 16;
#[derive(Debug, Clone, Copy)]
pub struct QueueLimits {
    pub facch_words: usize,
    pub sacch_words: usize,
}
impl Default for QueueLimits {
    fn default() -> Self {
        Self {
            facch_words: DEFAULT_QUEUE_WORDS,
            sacch_words: DEFAULT_QUEUE_WORDS,
        }
    }
}
#[derive(Debug, Clone)]
pub struct PreparedBurst {
    interval: u64,
    sync: SyncWord,
    bits: [bool; SLOT_BITS],
    token: Arc<()>,
    traffic: traffic_tx::PreparedInterval,
    sacch: sacch_tx::PreparedField,
}
impl PreparedBurst {
    pub fn interval(&self) -> u64 {
        self.interval
    }
    pub fn sync(&self) -> SyncWord {
        self.sync
    }
    pub fn bits(&self) -> &[bool; SLOT_BITS] {
        &self.bits
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub interval: u64,
    pub facch: Option<traffic_tx::CommandHandle>,
    pub sacch: Option<sacch_tx::CommandHandle>,
}
#[derive(Debug, Clone)]
pub struct ScheduledCompletion {
    pub interval: u64,
    pub facch: Option<traffic_tx::CommandHandle>,
    pub sacch: Option<sacch_tx::CommandHandle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxError {
    Traffic(traffic_tx::TxError),
    Sacch(sacch_tx::TxError),
    Framing(SlotError),
    Locator(u8),
    SacchMessage(u8),
    Prepared,
    StaleBurst,
    Interval {
        expected: u64,
        received: u64,
    },
    Sync {
        expected: SyncWord,
        received: SyncWord,
    },
}
impl fmt::Display for TxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bearer transmission failed: {self:?}")
    }
}
impl Error for TxError {}
pub struct BearerTx {
    dvcc: u8,
    locator: u8,
    traffic: TrafficTx,
    sacch: SacchTx,
    pending: Option<PreparedBurst>,
}
impl BearerTx {
    pub fn try_new(
        dvcc: u8,
        locator: u8,
        limits: QueueLimits,
        first_interval: u64,
    ) -> Result<Self, TxError> {
        if color::encode_locator(locator).is_none() {
            return Err(TxError::Locator(locator));
        }
        Ok(Self {
            dvcc,
            locator,
            traffic: TrafficTx::try_new(dvcc, limits.facch_words, first_interval)
                .map_err(TxError::Traffic)?,
            sacch: SacchTx::try_new(dvcc, limits.sacch_words, first_interval)
                .map_err(TxError::Sacch)?,
            pending: None,
        })
    }
    pub fn enqueue_facch(
        &mut self,
        message: &TrafficMessage,
    ) -> Result<traffic_tx::CommandHandle, TxError> {
        self.check_unprepared()?;
        self.traffic.enqueue(message).map_err(TxError::Traffic)
    }
    pub fn enqueue_sacch(
        &mut self,
        message: &TrafficMessage,
    ) -> Result<sacch_tx::CommandHandle, TxError> {
        self.check_unprepared()?;
        if !matches!(
            message,
            TrafficMessage::Audit | TrafficMessage::StopMeasurements | TrafficMessage::Relay(_)
        ) {
            return Err(TxError::SacchMessage(message.message_type()));
        }
        let words = traffic::encode(message)
            .map_err(|error| TxError::Traffic(traffic_tx::TxError::Message(error)))?;
        self.sacch.enqueue(&words).map_err(TxError::Sacch)
    }
    /// The complete word stream must be valid for the SACCH channel.
    pub fn enqueue_sacch_words(
        &mut self,
        words: &[SignalingWord],
    ) -> Result<sacch_tx::CommandHandle, TxError> {
        self.check_unprepared()?;
        self.sacch.enqueue(words).map_err(TxError::Sacch)
    }
    pub(crate) fn facch_pending(&self) -> bool {
        self.traffic.pending_words() != 0
    }
    pub(crate) fn prepared(&self) -> bool {
        self.pending.is_some()
    }
    pub fn offer_media(&mut self, interval: u64, frame: TransmitFrame) -> Result<(), TxError> {
        self.check_unprepared()?;
        self.traffic
            .offer_media(interval, frame)
            .map_err(TxError::Traffic)
    }
    pub fn cancel_facch(
        &mut self,
        command: &traffic_tx::CommandHandle,
    ) -> Result<traffic_tx::Cancellation, TxError> {
        self.check_unprepared()?;
        self.traffic.cancel(command).map_err(TxError::Traffic)
    }
    pub fn cancel_sacch(
        &mut self,
        command: &sacch_tx::CommandHandle,
    ) -> Result<sacch_tx::Cancellation, TxError> {
        self.check_unprepared()?;
        self.sacch.cancel(command).map_err(TxError::Sacch)
    }
    pub fn prepare(&mut self, interval: u64, sync: SyncWord) -> Result<&PreparedBurst, TxError> {
        if let Some(pending) = &self.pending {
            if interval != pending.interval {
                return Err(TxError::Interval {
                    expected: pending.interval,
                    received: interval,
                });
            }
            if sync != pending.sync {
                return Err(TxError::Sync {
                    expected: pending.sync,
                    received: sync,
                });
            }
        } else {
            let traffic = self
                .traffic
                .prepare(interval)
                .map_err(TxError::Traffic)?
                .clone();
            let sacch = match self.sacch.prepare(interval) {
                Ok(field) => field.clone(),
                Err(error) => {
                    self.traffic.discard(&traffic).map_err(TxError::Traffic)?;
                    return Err(TxError::Sacch(error));
                }
            };
            let bits = match phy_traffic::encode_forward(
                sync,
                sacch.field(),
                traffic.data(),
                self.dvcc,
                self.locator,
            ) {
                Ok(bits) => bits,
                Err(error) => {
                    self.traffic.discard(&traffic).map_err(TxError::Traffic)?;
                    self.sacch.discard(&sacch).map_err(TxError::Sacch)?;
                    return Err(TxError::Framing(error));
                }
            };
            self.pending = Some(PreparedBurst {
                interval,
                sync,
                bits,
                token: Arc::new(()),
                traffic,
                sacch,
            });
        }
        Ok(self
            .pending
            .as_ref()
            .expect("combined burst prepared above"))
    }
    /// Report only after the complete physical slot reached the air. Both channels advance under the same receipt.
    pub fn transmitted(&mut self, burst: &PreparedBurst) -> Result<Completion, TxError> {
        let scheduled = self.submitted(burst)?;
        Ok(Completion {
            interval: scheduled.interval,
            facch: scheduled.facch,
            sacch: scheduled.sacch,
        })
    }
    /// Advance coding after irrevocable IQ submission. Apply the returned commands only after their complete pulse tails elapse.
    pub fn submitted(&mut self, burst: &PreparedBurst) -> Result<ScheduledCompletion, TxError> {
        self.check_prepared(burst)?;
        let pending = self.pending.as_ref().expect("matching burst checked above");
        let facch = self
            .traffic
            .submitted(&pending.traffic)
            .map_err(TxError::Traffic)?;
        let sacch = self
            .sacch
            .submitted(&pending.sacch)
            .map_err(TxError::Sacch)?;
        self.pending = None;
        Ok(ScheduledCompletion {
            interval: burst.interval,
            facch,
            sacch,
        })
    }
    /// Discard only before any slot samples reached the air. Partial transmission requires stopping the bearer.
    pub fn discard(&mut self, burst: &PreparedBurst) -> Result<(), TxError> {
        self.check_prepared(burst)?;
        let pending = self.pending.as_ref().expect("matching burst checked above");
        self.traffic
            .discard(&pending.traffic)
            .map_err(TxError::Traffic)?;
        self.sacch.discard(&pending.sacch).map_err(TxError::Sacch)?;
        self.pending = None;
        Ok(())
    }
    fn check_unprepared(&self) -> Result<(), TxError> {
        if self.pending.is_some() {
            return Err(TxError::Prepared);
        }
        Ok(())
    }
    fn check_prepared(&self, burst: &PreparedBurst) -> Result<(), TxError> {
        let pending = self.pending.as_ref().ok_or(TxError::StaleBurst)?;
        if !Arc::ptr_eq(&pending.token, &burst.token) {
            return Err(TxError::StaleBurst);
        }
        self.traffic
            .check_prepared(&pending.traffic)
            .map_err(TxError::Traffic)?;
        self.sacch
            .check_prepared(&pending.sacch)
            .map_err(TxError::Sacch)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests;
