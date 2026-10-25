use std::{collections::VecDeque, error::Error, fmt, sync::Arc};

use super::traffic::SignalingWord;
use crate::phy::{
    sacch::{SacchEncoder, WORD_SLOTS},
    traffic::SACCH_BITS,
};
// IS-54-B §2.7.3.1.2.1 emits coded bit j in assigned slot m+j.
const INTERLEAVER_DELAY_SLOTS: u64 = (SACCH_BITS - 1) as u64;
#[derive(Debug, Clone)]
pub struct CommandHandle(Arc<()>);
impl PartialEq for CommandHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for CommandHandle {}
#[derive(Debug, Clone)]
pub struct PreparedField {
    interval: u64,
    field: [bool; SACCH_BITS],
    token: Arc<()>,
}
impl PreparedField {
    pub fn interval(&self) -> u64 {
        self.interval
    }
    pub fn field(&self) -> &[bool; SACCH_BITS] {
        &self.field
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
    Empty,
    Continuation,
    Prepared,
    QueueFull {
        available_words: usize,
        required_words: usize,
    },
    Interval {
        expected: u64,
        received: u64,
    },
    IntervalOverflow,
    StaleField,
}
impl fmt::Display for TxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SACCH scheduling failed: {self:?}")
    }
}
impl Error for TxError {}
struct QueuedWord {
    word: SignalingWord,
    command: CommandHandle,
    last: bool,
}
struct ActiveWord {
    fields: [[bool; SACCH_BITS]; WORD_SLOTS],
    cursor: usize,
}
struct InFlightWord {
    command: CommandHandle,
    ends_at: u64,
    last: bool,
}
struct NewWord {
    active: ActiveWord,
    encoder: SacchEncoder,
    tag: Option<InFlightWord>,
}
struct PendingField {
    prepared: PreparedField,
    new_word: Option<NewWord>,
}
pub struct SacchTx {
    dvcc: u8,
    max_pending_words: usize,
    words: VecDeque<QueuedWord>,
    encoder: SacchEncoder,
    active: Option<ActiveWord>,
    in_flight: VecDeque<InFlightWord>,
    pending: Option<PendingField>,
    next_interval: u64,
}
impl SacchTx {
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
            encoder: SacchEncoder::default(),
            active: None,
            in_flight: VecDeque::new(),
            pending: None,
            next_interval: first_interval,
        })
    }
    pub fn pending_words(&self) -> usize {
        self.words.len()
    }
    /// Supply a complete message word stream valid for SACCH. The first word starts a message and subsequent words continue it.
    pub fn enqueue(&mut self, words: &[SignalingWord]) -> Result<CommandHandle, TxError> {
        self.check_unprepared()?;
        if words.is_empty() {
            return Err(TxError::Empty);
        }
        if words[0].continuation || words[1..].iter().any(|word| !word.continuation) {
            return Err(TxError::Continuation);
        }
        let available_words = self.max_pending_words - self.words.len();
        if words.len() > available_words {
            return Err(TxError::QueueFull {
                available_words,
                required_words: words.len(),
            });
        }
        let command = CommandHandle(Arc::new(()));
        for (index, word) in words.iter().enumerate() {
            self.words.push_back(QueuedWord {
                word: word.clone(),
                command: command.clone(),
                last: index + 1 == words.len(),
            });
        }
        Ok(command)
    }
    /// Cancellation removes unsent words. Continuous coding and already transmitted diagonal bits cannot be reset mid-bearer.
    pub fn cancel(&mut self, command: &CommandHandle) -> Result<Cancellation, TxError> {
        self.check_unprepared()?;
        let before = self.words.len();
        self.words.retain(|word| &word.command != command);
        Ok(Cancellation {
            removed_words: before - self.words.len(),
            in_flight: self.in_flight.iter().any(|word| &word.command == command),
        })
    }
    pub fn prepare(&mut self, interval: u64) -> Result<&PreparedField, TxError> {
        if interval != self.next_interval {
            return Err(TxError::Interval {
                expected: self.next_interval,
                received: interval,
            });
        }
        interval.checked_add(1).ok_or(TxError::IntervalOverflow)?;
        if self.pending.is_none() {
            let new_word = if self.active.is_none() {
                let filler = SignalingWord::filler();
                let source = self.words.front();
                let word = source.map(|word| &word.word).unwrap_or(&filler);
                let tag = if let Some(source) = source {
                    Some(InFlightWord {
                        command: source.command.clone(),
                        ends_at: interval
                            .checked_add(WORD_SLOTS as u64 - 1)
                            .and_then(|end| end.checked_add(INTERLEAVER_DELAY_SLOTS))
                            .ok_or(TxError::IntervalOverflow)?,
                        last: source.last,
                    })
                } else {
                    None
                };
                let mut encoder = self.encoder.clone();
                let fields = encoder.encode_word(word.continuation, &word.contents, self.dvcc);
                Some(NewWord {
                    active: ActiveWord { fields, cursor: 0 },
                    encoder,
                    tag,
                })
            } else {
                None
            };
            let active = new_word
                .as_ref()
                .map(|word| &word.active)
                .or(self.active.as_ref())
                .expect("existing or newly encoded SACCH word");
            self.pending = Some(PendingField {
                prepared: PreparedField {
                    interval,
                    field: active.fields[active.cursor],
                    token: Arc::new(()),
                },
                new_word,
            });
        }
        Ok(&self
            .pending
            .as_ref()
            .expect("field prepared above")
            .prepared)
    }
    /// Report the complete transmitted slot. A word closes only after its final diagonal bit has reached the air.
    pub fn transmitted(
        &mut self,
        prepared: &PreparedField,
    ) -> Result<Option<CommandHandle>, TxError> {
        self.advance_scheduled(prepared)
    }
    /// Advance after irrevocable IQ submission. The returned command still requires an on-air completion receipt.
    pub fn submitted(
        &mut self,
        prepared: &PreparedField,
    ) -> Result<Option<CommandHandle>, TxError> {
        self.advance_scheduled(prepared)
    }
    fn advance_scheduled(
        &mut self,
        prepared: &PreparedField,
    ) -> Result<Option<CommandHandle>, TxError> {
        self.check_prepared(prepared)?;
        let pending = self.pending.take().expect("matching field checked above");
        if let Some(word) = pending.new_word {
            if let Some(tag) = word.tag {
                self.words
                    .pop_front()
                    .expect("queued word retained while field is prepared");
                self.in_flight.push_back(tag);
            }
            // The active word must drain before encoding another word, because this state includes all eleven input rows.
            self.encoder = word.encoder;
            self.active = Some(word.active);
        }
        let active = self
            .active
            .as_mut()
            .expect("prepared field owns an active word");
        active.cursor += 1;
        if active.cursor == WORD_SLOTS {
            self.active = None;
        }
        let completed = if self
            .in_flight
            .front()
            .is_some_and(|word| word.ends_at == prepared.interval)
        {
            let word = self
                .in_flight
                .pop_front()
                .expect("closing word checked above");
            word.last.then_some(word.command)
        } else {
            None
        };
        self.next_interval += 1;
        Ok(completed)
    }
    /// Discard only if none of this slot reached the air. A partial slot requires stopping the bearer.
    pub fn discard(&mut self, prepared: &PreparedField) -> Result<(), TxError> {
        self.check_prepared(prepared)?;
        self.pending = None;
        Ok(())
    }
    fn check_unprepared(&self) -> Result<(), TxError> {
        if self.pending.is_some() {
            return Err(TxError::Prepared);
        }
        Ok(())
    }
    pub(super) fn check_prepared(&self, prepared: &PreparedField) -> Result<(), TxError> {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| !Arc::ptr_eq(&pending.prepared.token, &prepared.token))
        {
            return Err(TxError::StaleField);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
