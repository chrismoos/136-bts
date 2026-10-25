pub(crate) mod reuse;
use crate::{
    link::{
        rach::{ArqStatus, MobileIdentity},
        spach::{
            Channel, HeaderFlags, SpachError,
            arq::{self, ArqFrame, ArqHeader, ResponseMode},
        },
    },
    phy::control::INFORMATION_BITS,
};
use std::{collections::VecDeque, error::Error as StdError, fmt, sync::Arc};
const ECHO_VALUES: usize = 128;
// TIA/EIA-136-122-B §4.2.2 permits a 32-block unsolicited-status wait and a further 32-block final wait.
const RESTART_BLOCKS: u64 = 64;
const FULL_RATE_INTERRUPTION_FRAMES: usize = 32;
pub const REFRESH_CAPACITY: usize = FULL_RATE_INTERRUPTION_FRAMES / 2;
const REFRESH_BLOCKS: u64 = REFRESH_CAPACITY as u64;
// A delayed ARQ status has no MSID or generation, so retired PEA values stay quarantined past their deadline.
const ECHO_GUARD_MS: u64 = super::timers::REGISTRATION_MS + 2 * super::timers::SUPERFRAME_MS;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Limits,
    Capacity,
    EchoExhausted,
    Clock,
    Deadline,
    Stale,
    Wire(SpachError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SPACH ARQ sender failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug, Clone)]
pub struct TransferToken(Arc<()>);
impl PartialEq for TransferToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for TransferToken {}
#[derive(Debug, Clone)]
pub struct FrameToken {
    transaction: Arc<()>,
    frame: usize,
    submission: Arc<()>,
}
impl FrameToken {
    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.transaction, &other.transaction)
            && Arc::ptr_eq(&self.submission, &other.submission)
    }
}
#[derive(Debug, Clone)]
pub struct PreparedFrame {
    pub identity: MobileIdentity,
    pub information: [bool; INFORMATION_BITS],
    pub token: FrameToken,
    partial_echo: u8,
}
impl PreparedFrame {
    pub(crate) fn partial_echo(&self) -> u8 {
        self.partial_echo
    }
}
#[derive(Debug)]
pub enum Event<C> {
    Complete(C),
    Expired(C),
    Cancelled(C),
    Failed { completion: C, error: Error },
}
struct Transfer<C> {
    identity: MobileIdentity,
    echo: u8,
    owner: Arc<()>,
    generation: Arc<()>,
    frames: Vec<ArqFrame>,
    queue: VecDeque<usize>,
    in_flight: Vec<FrameToken>,
    transmitted: Vec<bool>,
    observed: Vec<bool>,
    received_status: bool,
    last_progress: Option<u64>,
    last_frame: Option<u64>,
    restart_requested: bool,
    waiting_since: Option<u64>,
    deadline_ms: u64,
    completion: C,
}
impl<C> Transfer<C> {
    fn create_frames(
        channel: Channel,
        identity: MobileIdentity,
        message: &[u8],
        echo: u8,
    ) -> Result<Vec<ArqFrame>, Error> {
        arq::fragment_message(
            ArqHeader {
                channel,
                flags: HeaderFlags::default(),
                partial_echo: echo,
                polling: false,
                response_mode: ResponseMode::Contention,
            },
            identity,
            message,
        )
        .map_err(Error::Wire)
    }
    // TIA/EIA-136-122-B §4.2.1 excludes BEGIN from the continuation status bitmap.
    fn done(&self) -> bool {
        self.received_status
            && self.in_flight.is_empty()
            && self.transmitted[0]
            && self
                .observed
                .iter()
                .enumerate()
                .all(|(index, received)| *received && self.transmitted[index + 1])
    }
    fn settle(&mut self, block: u64) {
        if !self.queue.is_empty() || !self.in_flight.is_empty() || self.done() {
            return;
        }
        if self.received_status {
            self.queue.extend(
                self.observed
                    .iter()
                    .enumerate()
                    .filter_map(|(index, received)| (!received).then_some(index + 1)),
            );
            self.received_status = false;
            self.waiting_since = None;
        } else {
            self.waiting_since.get_or_insert(block);
        }
    }
}
#[derive(Debug, Clone)]
pub struct Request<C> {
    pub identity: MobileIdentity,
    pub channel: Channel,
    pub message: Vec<u8>,
    pub completion: C,
    pub deadline_ms: u64,
}
/// Block arguments share one monotonic 20-ms TDMA clock, including blocks without SPACH frames.
pub struct Sender<C> {
    transfers: VecDeque<Transfer<C>>,
    events: VecDeque<Event<C>>,
    echo_until_ms: [u64; ECHO_VALUES],
    next_echo: usize,
    selected: Option<PreparedFrame>,
    capacity: usize,
    last_ms: u64,
    last_block: u64,
    startup_until_block: u64,
    echo_guard_ms: u64,
}
impl<C> Sender<C> {
    pub fn try_new(capacity: usize) -> Result<Self, Error> {
        if capacity == 0 || capacity > ECHO_VALUES {
            return Err(Error::Limits);
        }
        Ok(Self {
            transfers: VecDeque::new(),
            events: VecDeque::new(),
            echo_until_ms: [0; ECHO_VALUES],
            next_echo: 0,
            selected: None,
            capacity,
            last_ms: 0,
            last_block: 0,
            startup_until_block: 0,
            echo_guard_ms: ECHO_GUARD_MS,
        })
    }
    pub(crate) fn after_restart(
        capacity: usize,
        policy: reuse::ReusePolicy,
    ) -> Result<Self, Error> {
        let mut sender = Self::try_new(capacity)?;
        sender.startup_until_block = policy.quiet_blocks();
        sender.echo_guard_ms = ECHO_GUARD_MS.max(policy.quiet_ms);
        Ok(sender)
    }
    pub(crate) fn can_enqueue(&self, block: u64, now_ms: u64) -> bool {
        block >= self.startup_until_block
            && self.transfers.len() < self.capacity
            && self.events.len() < self.capacity
            && self.echo_until_ms.iter().any(|until| *until <= now_ms)
    }
    fn clock(&mut self, block: u64, now_ms: u64) -> Result<(), Error> {
        if block < self.last_block || now_ms < self.last_ms {
            return Err(Error::Clock);
        }
        self.last_block = block;
        self.last_ms = now_ms;
        Ok(())
    }
    fn allocate_echo(&mut self, now_ms: u64, deadline_ms: u64) -> Result<u8, Error> {
        if self.last_block < self.startup_until_block {
            return Err(Error::EchoExhausted);
        }
        let until = deadline_ms
            .checked_add(self.echo_guard_ms)
            .ok_or(Error::Deadline)?;
        let index = (0..ECHO_VALUES)
            .map(|offset| (self.next_echo + offset) % ECHO_VALUES)
            .find(|index| self.echo_until_ms[*index] <= now_ms)
            .ok_or(Error::EchoExhausted)?;
        self.echo_until_ms[index] = until;
        self.next_echo = (index + 1) % ECHO_VALUES;
        Ok(index as u8)
    }
    pub fn enqueue(
        &mut self,
        request: Request<C>,
        block: u64,
        now_ms: u64,
    ) -> Result<TransferToken, Error> {
        let Request {
            identity,
            channel,
            message,
            completion,
            deadline_ms,
        } = request;
        self.clock(block, now_ms)?;
        if self.events.len() >= self.capacity {
            return Err(Error::Capacity);
        }
        if deadline_ms <= now_ms {
            return Err(Error::Deadline);
        }
        if message.is_empty() {
            return Err(Error::Wire(SpachError::Length));
        }
        let replacing = self.transfers.iter().position(|tx| tx.identity == identity);
        if replacing.is_none() && self.transfers.len() == self.capacity {
            return Err(Error::Capacity);
        }
        Transfer::<C>::create_frames(channel, identity, &message, 0)?;
        let echo = self.allocate_echo(now_ms, deadline_ms)?;
        let frames = Transfer::<C>::create_frames(channel, identity, &message, echo)?;
        if let Some(index) = replacing {
            let old = self.transfers.remove(index).expect("matched transaction");
            self.clear_selected(&old.generation);
            self.events.push_back(Event::Cancelled(old.completion));
        }
        let count = frames.len();
        let token = TransferToken(Arc::new(()));
        self.transfers.push_back(Transfer {
            identity,
            echo,
            owner: token.0.clone(),
            generation: token.0.clone(),
            frames,
            queue: (0..count).collect(),
            in_flight: Vec::new(),
            transmitted: vec![false; count],
            observed: vec![false; count - 1],
            received_status: false,
            last_progress: None,
            last_frame: None,
            restart_requested: false,
            waiting_since: None,
            deadline_ms,
            completion,
        });
        Ok(token)
    }
    fn clear_selected(&mut self, generation: &Arc<()>) {
        if self
            .selected
            .as_ref()
            .is_some_and(|frame| Arc::ptr_eq(&frame.token.transaction, generation))
        {
            self.selected = None;
        }
    }
    pub fn advance(&mut self, block: u64, now_ms: u64) -> Result<(), Error> {
        self.clock(block, now_ms)?;
        let count = self.transfers.len();
        for _ in 0..count {
            let mut tx = self.transfers.pop_front().expect("counted transaction");
            if now_ms >= tx.deadline_ms || tx.done() {
                self.clear_selected(&tx.generation);
                self.events.push_back(if now_ms >= tx.deadline_ms {
                    Event::Expired(tx.completion)
                } else {
                    Event::Complete(tx.completion)
                });
                continue;
            }
            tx.settle(block);
            if tx.restart_requested
                || tx
                    .last_progress
                    .is_some_and(|since| block.saturating_sub(since) >= RESTART_BLOCKS)
                || tx
                    .waiting_since
                    .is_some_and(|since| block.saturating_sub(since) >= RESTART_BLOCKS)
            {
                match self.allocate_echo(now_ms, tx.deadline_ms) {
                    Ok(echo) => {
                        self.clear_selected(&tx.generation);
                        tx.echo = echo;
                        tx.generation = Arc::new(());
                        for frame in &mut tx.frames {
                            let header = match frame {
                                ArqFrame::Begin { header, .. }
                                | ArqFrame::Continue { header, .. } => header,
                            };
                            header.partial_echo = echo;
                        }
                        tx.queue = (0..tx.frames.len()).collect();
                        tx.transmitted.fill(false);
                        tx.observed.fill(false);
                        tx.received_status = false;
                        tx.last_progress = None;
                        tx.last_frame = None;
                        tx.restart_requested = false;
                        tx.waiting_since = None;
                        tx.in_flight.clear();
                    }
                    Err(Error::EchoExhausted) => {
                        self.clear_selected(&tx.generation);
                        self.events.push_back(Event::Failed {
                            completion: tx.completion,
                            error: Error::EchoExhausted,
                        });
                        continue;
                    }
                    Err(error) => {
                        self.transfers.push_front(tx);
                        return Err(error);
                    }
                }
            }
            self.transfers.push_back(tx);
        }
        Ok(())
    }
    pub fn prepare(&mut self, block: u64, now_ms: u64) -> Result<Option<PreparedFrame>, Error> {
        self.advance(block, now_ms)?;
        if let Some(selected) = &self.selected {
            return Ok(Some(selected.clone()));
        }
        let Some(tx) = self.transfers.iter().find(|tx| !tx.queue.is_empty()) else {
            return Ok(None);
        };
        let index = *tx.queue.front().expect("nonempty queue");
        let mut frame = tx.frames[index].clone();
        let header = match &mut frame {
            ArqFrame::Begin { header, .. } | ArqFrame::Continue { header, .. } => header,
        };
        header.polling = tx.queue.len() == 1;
        let prepared = PreparedFrame {
            identity: tx.identity,
            partial_echo: tx.echo,
            information: frame.encode().map_err(Error::Wire)?,
            token: FrameToken {
                transaction: tx.generation.clone(),
                frame: index,
                submission: Arc::new(()),
            },
        };
        self.selected = Some(prepared.clone());
        Ok(Some(prepared))
    }
    pub(crate) fn has_echo(&self, echo: u8) -> bool {
        self.transfers.iter().any(|tx| tx.echo == echo)
    }
    pub(crate) fn interrupt(&mut self, echo: u8, block: u64, now_ms: u64) -> Result<bool, Error> {
        self.clock(block, now_ms)?;
        let Some(tx) = self.transfers.iter_mut().find(|tx| tx.echo == echo) else {
            return Ok(false);
        };
        tx.restart_requested = true;
        self.advance(block, now_ms)?;
        Ok(true)
    }
    pub(crate) fn prepare_prioritized(
        &mut self,
        echo: u8,
        block: u64,
        now_ms: u64,
    ) -> Result<Option<PreparedFrame>, Error> {
        self.advance(block, now_ms)?;
        let Some(index) = self.transfers.iter().position(|tx| tx.echo == echo) else {
            return Ok(None);
        };
        let mut tx = self
            .transfers
            .remove(index)
            .expect("priority transaction exists");
        if tx.queue.is_empty() && tx.in_flight.is_empty() && tx.last_frame.is_some() {
            tx.queue.push_back(tx.frames.len() - 1);
        }
        let ready = !tx.queue.is_empty();
        self.transfers.push_front(tx);
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.partial_echo != echo)
        {
            self.selected = None;
        }
        if !ready {
            return Ok(None);
        }
        self.prepare(block, now_ms)
    }
    pub fn submitted(&mut self, token: &FrameToken, block: u64, now_ms: u64) -> Result<(), Error> {
        self.clock(block, now_ms)?;
        if self
            .selected
            .as_ref()
            .is_none_or(|selected| !selected.token.matches(token))
        {
            return Err(Error::Stale);
        }
        let index = self
            .transfers
            .iter()
            .position(|tx| Arc::ptr_eq(&tx.generation, &token.transaction))
            .ok_or(Error::Stale)?;
        let mut tx = self.transfers.remove(index).expect("matched submission");
        if tx.queue.front() != Some(&token.frame) {
            self.transfers.insert(index, tx);
            return Err(Error::Stale);
        }
        tx.queue.pop_front();
        tx.in_flight.push(token.clone());
        tx.last_progress = Some(block);
        tx.last_frame = Some(block);
        self.selected = None;
        self.transfers.push_back(tx);
        Ok(())
    }
    pub(crate) fn submitted_message(&self, token: &FrameToken) -> Option<&C> {
        self.transfers
            .iter()
            .find(|tx| Arc::ptr_eq(&tx.generation, &token.transaction))
            .filter(|tx| {
                tx.transmitted.iter().enumerate().all(|(index, complete)| {
                    *complete || tx.in_flight.iter().any(|pending| pending.frame == index)
                })
            })
            .map(|tx| &tx.completion)
    }
    pub fn transmitted(
        &mut self,
        token: &FrameToken,
        block: u64,
        now_ms: u64,
    ) -> Result<bool, Error> {
        self.transmitted_with(token, block, now_ms, |_| {})
    }
    pub(crate) fn transmitted_with(
        &mut self,
        token: &FrameToken,
        block: u64,
        now_ms: u64,
        completed: impl FnOnce(&C),
    ) -> Result<bool, Error> {
        self.clock(block, now_ms)?;
        let Some(tx) = self
            .transfers
            .iter_mut()
            .find(|tx| Arc::ptr_eq(&tx.generation, &token.transaction))
        else {
            return Ok(false);
        };
        let Some(index) = tx
            .in_flight
            .iter()
            .position(|pending| pending.matches(token))
        else {
            return Ok(false);
        };
        tx.in_flight.remove(index);
        tx.transmitted[token.frame] = true;
        if tx.transmitted.iter().all(|complete| *complete) {
            completed(&tx.completion);
        }
        tx.settle(block);
        self.advance(block, now_ms)?;
        Ok(true)
    }
    pub fn status(&mut self, status: &ArqStatus, block: u64, now_ms: u64) -> Result<bool, Error> {
        self.advance(block, now_ms)?;
        let Some(tx) = self
            .transfers
            .iter_mut()
            .find(|tx| tx.echo == status.partial_echo)
        else {
            return Ok(false);
        };
        if !tx.transmitted[0] && !tx.in_flight.iter().any(|pending| pending.frame == 0) {
            return Ok(false);
        }
        for (index, received) in status
            .received_frames
            .iter()
            .take(tx.observed.len())
            .enumerate()
        {
            if *received
                && (tx.transmitted[index + 1]
                    || tx
                        .in_flight
                        .iter()
                        .any(|pending| pending.frame == index + 1))
            {
                tx.observed[index] = true;
            }
        }
        tx.received_status = true;
        tx.waiting_since = None;
        tx.last_progress = Some(block);
        tx.settle(block);
        self.advance(block, now_ms)?;
        Ok(true)
    }
    pub fn cancel(&mut self, identity: MobileIdentity) -> bool {
        let Some(index) = self.transfers.iter().position(|tx| tx.identity == identity) else {
            return false;
        };
        let tx = self.transfers.remove(index).expect("matched cancellation");
        self.clear_selected(&tx.generation);
        self.events.push_back(Event::Cancelled(tx.completion));
        true
    }
    pub fn cancel_transaction(&mut self, token: &TransferToken) -> bool {
        let Some(index) = self
            .transfers
            .iter()
            .position(|tx| Arc::ptr_eq(&tx.owner, &token.0))
        else {
            return false;
        };
        let tx = self
            .transfers
            .remove(index)
            .expect("matched transaction cancellation");
        self.clear_selected(&tx.generation);
        self.events.push_back(Event::Cancelled(tx.completion));
        true
    }
    pub fn cancel_all(&mut self) {
        self.selected = None;
        for tx in self.transfers.drain(..) {
            self.events.push_back(Event::Cancelled(tx.completion));
        }
    }
    /// Callers interleaving other messages must also enforce the 32-SPACH-frame interruption bound.
    pub fn refresh_waiting(&mut self, block: u64, now_ms: u64) -> Result<(), Error> {
        self.advance(block, now_ms)?;
        for tx in &mut self.transfers {
            if tx.queue.is_empty()
                && tx.in_flight.is_empty()
                && tx
                    .last_frame
                    .is_some_and(|since| block.saturating_sub(since) >= REFRESH_BLOCKS)
            {
                tx.queue.push_back(tx.frames.len() - 1);
            }
        }
        if self.selected.is_none() {
            let oldest = self
                .transfers
                .iter()
                .enumerate()
                .filter(|(_, tx)| !tx.queue.is_empty())
                .filter_map(|(index, tx)| {
                    tx.last_frame
                        .map(|last| (index, block.saturating_sub(last)))
                })
                .filter(|(_, age)| *age >= REFRESH_BLOCKS)
                .max_by_key(|(_, age)| *age)
                .map(|(index, _)| index);
            if let Some(index) = oldest {
                let tx = self
                    .transfers
                    .remove(index)
                    .expect("oldest ready transaction");
                self.transfers.push_front(tx);
            }
        }
        Ok(())
    }
    pub fn identity_for_echo(&self, echo: u8) -> Option<MobileIdentity> {
        self.transfers
            .iter()
            .find(|tx| tx.echo == echo)
            .map(|tx| tx.identity)
    }
    pub fn radio_active(&self) -> bool {
        self.transfers.iter().any(|tx| tx.last_progress.is_some())
    }
    pub fn ready(&self) -> bool {
        self.transfers.iter().any(|tx| !tx.queue.is_empty())
    }
    pub fn cancel_where(&mut self, mut predicate: impl FnMut(MobileIdentity) -> bool) {
        self.cancel_matching(|identity, _| predicate(identity));
    }
    pub(crate) fn contains(&self, identity: MobileIdentity) -> bool {
        self.transfers.iter().any(|tx| tx.identity == identity)
    }
    pub(crate) fn cancel_matching(
        &mut self,
        mut predicate: impl FnMut(MobileIdentity, &C) -> bool,
    ) {
        let mut index = 0;
        while index < self.transfers.len() {
            if predicate(
                self.transfers[index].identity,
                &self.transfers[index].completion,
            ) {
                let tx = self.transfers.remove(index).expect("matched transaction");
                self.clear_selected(&tx.generation);
                self.events.push_back(Event::Cancelled(tx.completion));
            } else {
                index += 1;
            }
        }
    }
    pub fn pop_event(&mut self) -> Option<Event<C>> {
        self.events.pop_front()
    }
    pub fn pending(&self) -> usize {
        self.transfers.len()
    }
}
#[cfg(test)]
mod tests;
