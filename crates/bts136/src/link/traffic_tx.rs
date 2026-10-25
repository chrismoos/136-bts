use std::{collections::VecDeque, error::Error, fmt, sync::Arc};

use super::{traffic, voice::TransmitFrame};
use crate::{
    l3::legacy_traffic::{TrafficMessage, TrafficMessageError},
    phy::traffic::DATA_BITS,
};

#[derive(Debug, Clone)]
pub struct CommandHandle(Arc<()>);
impl PartialEq for CommandHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for CommandHandle {}
#[derive(Debug, Clone)]
pub struct PreparedInterval {
    interval: u64,
    data: [bool; DATA_BITS],
    token: Arc<()>,
}
impl PreparedInterval {
    pub fn interval(&self) -> u64 {
        self.interval
    }
    pub fn data(&self) -> &[bool; DATA_BITS] {
        &self.data
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancellation {
    pub removed_words: usize,
    pub in_flight: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxError {
    Capacity,
    SacchOnly,
    QueueFull {
        available_words: usize,
        required_words: usize,
    },
    Message(TrafficMessageError),
    Prepared,
    MediaPending,
    Interval {
        expected: u64,
        received: u64,
    },
    IntervalOverflow,
    StaleInterval,
}
impl fmt::Display for TxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "traffic transmission scheduling failed: {self:?}")
    }
}
impl Error for TxError {}
#[derive(Clone)]
struct QueuedWord {
    frame: TransmitFrame,
    command: CommandHandle,
    last: bool,
}
struct PendingInterval {
    prepared: PreparedInterval,
    current: TransmitFrame,
    queued: bool,
    completion: Option<CommandHandle>,
    command: Option<CommandHandle>,
}

pub struct TrafficTx {
    dvcc: u8,
    max_pending_words: usize,
    words: VecDeque<QueuedWord>,
    media: Option<TransmitFrame>,
    previous: TransmitFrame,
    previous_completion: Option<CommandHandle>,
    previous_command: Option<CommandHandle>,
    pending: Option<PendingInterval>,
    next_interval: u64,
}
impl TrafficTx {
    pub fn try_new(
        dvcc: u8,
        max_pending_words: usize,
        first_interval: u64,
    ) -> Result<Self, TxError> {
        if max_pending_words == 0 {
            return Err(TxError::Capacity);
        }
        Ok(Self {
            dvcc,
            max_pending_words,
            words: VecDeque::new(),
            media: None,
            previous: TransmitFrame::facch(&traffic::SignalingWord::filler(), dvcc),
            previous_completion: None,
            previous_command: None,
            pending: None,
            next_interval: first_interval,
        })
    }
    pub fn pending_words(&self) -> usize {
        self.words.len()
    }
    pub fn enqueue(&mut self, message: &TrafficMessage) -> Result<CommandHandle, TxError> {
        self.check_unprepared()?;
        if matches!(message, TrafficMessage::Audit) {
            return Err(TxError::SacchOnly);
        }
        let words = traffic::encode(message).map_err(TxError::Message)?;
        let available_words = self.max_pending_words - self.words.len();
        if words.len() > available_words {
            return Err(TxError::QueueFull {
                available_words,
                required_words: words.len(),
            });
        }
        let command = CommandHandle(Arc::new(()));
        let last = words.len() - 1;
        for (index, word) in words.iter().enumerate() {
            self.words.push_back(QueuedWord {
                frame: TransmitFrame::facch(word, self.dvcc),
                command: command.clone(),
                last: index == last,
            });
        }
        Ok(command)
    }
    /// Offer media for the next interval. FACCH steals that interval and discards its media instead of delaying it.
    pub fn offer_media(&mut self, interval: u64, frame: TransmitFrame) -> Result<(), TxError> {
        self.check_unprepared()?;
        self.check_interval(interval)?;
        if self.media.is_some() {
            return Err(TxError::MediaPending);
        }
        self.media = Some(frame);
        Ok(())
    }
    /// Cancellation cannot undo a word whose first interleaved half has already been transmitted.
    pub fn cancel(&mut self, command: &CommandHandle) -> Result<Cancellation, TxError> {
        self.check_unprepared()?;
        let before = self.words.len();
        self.words.retain(|word| &word.command != command);
        Ok(Cancellation {
            removed_words: before - self.words.len(),
            in_flight: self.previous_command.as_ref() == Some(command),
        })
    }
    pub fn prepare(&mut self, interval: u64) -> Result<&PreparedInterval, TxError> {
        self.check_interval(interval)?;
        interval.checked_add(1).ok_or(TxError::IntervalOverflow)?;
        if self.pending.is_none() {
            let (current, queued, completion) = if let Some(word) = self.words.front() {
                (
                    word.frame.clone(),
                    true,
                    word.last.then(|| word.command.clone()),
                )
            } else {
                (
                    self.media.clone().unwrap_or_else(|| {
                        TransmitFrame::facch(&traffic::SignalingWord::filler(), self.dvcc)
                    }),
                    false,
                    None,
                )
            };
            self.pending = Some(PendingInterval {
                prepared: PreparedInterval {
                    interval,
                    data: TransmitFrame::interleave(&self.previous, &current),
                    token: Arc::new(()),
                },
                current,
                queued,
                completion,
                command: self.words.front().map(|word| word.command.clone()),
            });
        }
        Ok(&self
            .pending
            .as_ref()
            .expect("pending interval prepared above")
            .prepared)
    }
    /// Call only after the entire prepared slot was transmitted. Completion includes the preceding final word's second half.
    pub fn transmitted(
        &mut self,
        prepared: &PreparedInterval,
    ) -> Result<Option<CommandHandle>, TxError> {
        self.advance_scheduled(prepared)
    }
    /// Advance after irrevocable IQ submission. The returned command still requires an on-air completion receipt.
    pub fn submitted(
        &mut self,
        prepared: &PreparedInterval,
    ) -> Result<Option<CommandHandle>, TxError> {
        self.advance_scheduled(prepared)
    }
    fn advance_scheduled(
        &mut self,
        prepared: &PreparedInterval,
    ) -> Result<Option<CommandHandle>, TxError> {
        self.check_prepared(prepared)?;
        let pending = self
            .pending
            .take()
            .expect("matching pending interval checked above");
        if pending.queued {
            self.words
                .pop_front()
                .expect("selected word retained while interval is prepared");
        }
        self.media = None;
        self.previous = pending.current;
        self.previous_command = pending.command;
        let completed = self.previous_completion.take();
        self.previous_completion = pending.completion;
        self.next_interval += 1;
        Ok(completed)
    }
    /// Discard only if none of this slot reached the air. Partial transmission requires stopping the bearer.
    pub fn discard(&mut self, prepared: &PreparedInterval) -> Result<(), TxError> {
        self.check_prepared(prepared)?;
        self.pending = None;
        Ok(())
    }
    pub(super) fn check_prepared(&self, prepared: &PreparedInterval) -> Result<(), TxError> {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| !Arc::ptr_eq(&pending.prepared.token, &prepared.token))
        {
            return Err(TxError::StaleInterval);
        }
        Ok(())
    }
    fn check_unprepared(&self) -> Result<(), TxError> {
        if self.pending.is_some() {
            return Err(TxError::Prepared);
        }
        Ok(())
    }
    fn check_interval(&self, interval: u64) -> Result<(), TxError> {
        if interval != self.next_interval {
            return Err(TxError::Interval {
                expected: self.next_interval,
                received: interval,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
