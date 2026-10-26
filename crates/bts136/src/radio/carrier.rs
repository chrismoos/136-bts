use super::{
    duplex::{self, Duplex, State},
    receiver::ReadOutcome,
    timed_tx::{Observation, Receipt},
};
use crate::phy::traffic::SLOT_BITS;
use num_complex::Complex32;
use std::{
    collections::{TryReserveError, VecDeque},
    error::Error as StdError,
    fmt,
    sync::Arc,
};

#[derive(Debug)]
pub enum Error {
    Limits,
    State,
    Timeline(super::tx_timeline::TimelineError),
    PendingCapacity,
    OutputCapacity,
    ForeignFence,
    Allocation(TryReserveError),
    Radio(duplex::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "carrier execution failed: {self:?}")
    }
}
impl Error {
    pub(crate) fn is_late(&self) -> bool {
        matches!(self, Self::Radio(duplex::Error { failure: duplex::Failure::Tx(error), .. })
            if matches!(error.as_ref(), super::forward_tx::Error::Tx(super::timed_tx::Error { failure: super::timed_tx::Failure::Late { .. }, .. })))
    }
}
impl StdError for Error {}
#[derive(Debug)]
pub struct Rejected<T> {
    pub error: Error,
    pub completion: T,
}
pub(crate) trait SlotObserver: Send {
    fn submitted(&mut self, index: u64, bits: &[bool; SLOT_BITS], receipt: &Receipt);
    fn completed(&mut self, receipt: &Receipt);
}
#[derive(Debug)]
pub struct Completed<T> {
    pub slot_index: u64,
    /// Scheduled completion includes the full pulse tail. Clock coverage is not independent RF-delivery proof.
    pub scheduled_end_tick: u64,
    pub completion: T,
}
#[derive(Debug, Clone, Copy)]
struct FenceBoundary {
    slot_index: u64,
    scheduled_end_tick: u64,
}
#[derive(Debug, Clone)]
pub struct QueueFence {
    owner: Arc<()>,
    boundary: Option<FenceBoundary>,
}
impl QueueFence {
    pub fn scheduled_end_tick(&self) -> Option<u64> {
        self.boundary.map(|boundary| boundary.scheduled_end_tick)
    }
}
struct Pending<T> {
    index: u64,
    scheduled_end_tick: u64,
    completion: T,
}
struct CommitGuard<'a> {
    radio: &'a mut Duplex,
    armed: bool,
}
impl Drop for CommitGuard<'_> {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = self.radio.stop()
        {
            tracing::error!(%error, "Radio stop failed after interrupted carrier commit");
        }
    }
}
pub struct Carrier<T> {
    owner: Arc<()>,
    covered_tick: Option<u64>,
    observer: Option<Box<dyn SlotObserver>>,
    radio: Duplex,
    pending: VecDeque<Pending<T>>,
    max_pending_slots: usize,
}
impl<T> Carrier<T> {
    pub fn try_new(radio: Duplex, max_pending_slots: usize) -> Result<Self, Error> {
        if radio.state() != State::Prepared {
            return Err(Error::State);
        }
        if max_pending_slots == 0 {
            return Err(Error::Limits);
        }
        let mut pending = VecDeque::new();
        pending
            .try_reserve_exact(max_pending_slots)
            .map_err(Error::Allocation)?;
        Ok(Self {
            owner: Arc::new(()),
            covered_tick: None,
            radio,
            observer: None,
            pending,
            max_pending_slots,
        })
    }
    pub(crate) fn set_observer(&mut self, observer: Box<dyn SlotObserver>) {
        self.observer = Some(observer);
    }
    pub fn state(&self) -> State {
        self.radio.state()
    }
    /// Withdraw the canceled source before capturing this bound so no later slot can contain more of its data.
    pub fn queue_fence(&self) -> QueueFence {
        QueueFence {
            owner: self.owner.clone(),
            boundary: self.pending.back().map(|pending| FenceBoundary {
                slot_index: pending.index,
                scheduled_end_tick: pending.scheduled_end_tick,
            }),
        }
    }
    pub fn fence_complete(&self, fence: &QueueFence) -> Result<bool, Error> {
        if !Arc::ptr_eq(&self.owner, &fence.owner) {
            return Err(Error::ForeignFence);
        }
        if self.stop_acknowledged() {
            return Ok(true);
        }
        if !matches!(self.state(), State::Running) {
            return Err(Error::State);
        }
        Ok(fence.boundary.is_none_or(|boundary| {
            self.covered_tick
                .is_some_and(|covered| covered >= boundary.scheduled_end_tick)
                && self.radio.completed_slots() > boundary.slot_index
        }))
    }
    pub fn pending_end_tick(&self) -> Option<u64> {
        self.pending
            .back()
            .map(|pending| pending.scheduled_end_tick)
    }
    pub fn pending_slots(&self) -> usize {
        self.pending.len()
    }
    pub fn start(
        &mut self,
        nominal_epoch_tick: u64,
        slots: u64,
        rx_tick: Option<u64>,
    ) -> Result<(), Error> {
        self.radio
            .start(nominal_epoch_tick, slots, rx_tick)
            .map_err(Error::Radio)
    }
    pub fn start_continuous(
        &mut self,
        nominal_epoch_tick: u64,
        rx_tick: Option<u64>,
    ) -> Result<(), Error> {
        self.radio
            .start_continuous(nominal_epoch_tick, rx_tick)
            .map_err(Error::Radio)
    }
    pub fn stop_acknowledged(&self) -> bool {
        self.radio.stop_acknowledged()
    }
    pub fn submit_slot(
        &mut self,
        index: u64,
        bits: &[bool; SLOT_BITS],
        completion: T,
    ) -> Result<Receipt, Rejected<T>> {
        if self.pending.len() == self.max_pending_slots {
            return Err(Rejected {
                error: Error::PendingCapacity,
                completion,
            });
        }
        let scheduled_end_tick = match self.scheduled_end_tick(index) {
            Ok(tick) => tick,
            Err(error) => return Err(Rejected { error, completion }),
        };
        match self.radio.submit_slot(index, bits) {
            Ok(receipt) => {
                let mut guard = CommitGuard {
                    radio: &mut self.radio,
                    armed: true,
                };
                if let Some(observer) = self.observer.as_mut() {
                    observer.submitted(index, bits, &receipt);
                }
                self.pending.push_back(Pending {
                    index,
                    scheduled_end_tick,
                    completion,
                });
                guard.armed = false;
                Ok(receipt)
            }
            Err(error) => Err(Rejected {
                error: Error::Radio(error),
                completion,
            }),
        }
    }
    pub fn submit_slot_with(
        &mut self,
        index: u64,
        bits: &[bool; SLOT_BITS],
        completion: impl FnOnce() -> T,
    ) -> Result<Receipt, Error> {
        if self.pending.len() == self.max_pending_slots {
            return Err(Error::PendingCapacity);
        }
        let scheduled_end_tick = self.scheduled_end_tick(index)?;
        let receipt = self.radio.submit_slot(index, bits).map_err(Error::Radio)?;
        let mut guard = CommitGuard {
            radio: &mut self.radio,
            armed: true,
        };
        if let Some(observer) = self.observer.as_mut() {
            observer.submitted(index, bits, &receipt);
        }
        self.pending.push_back(Pending {
            index,
            scheduled_end_tick,
            completion: completion(),
        });
        guard.armed = false;
        Ok(receipt)
    }
    pub(crate) fn skip_expired_slots(
        &mut self,
        index: u64,
        lead_ticks: u64,
        maximum_slots: u64,
    ) -> Result<u64, Error> {
        self.radio
            .skip_expired_slots(index, lead_ticks, maximum_slots)
            .map_err(Error::Radio)
    }
    pub fn finish(&mut self) -> Result<Receipt, Error> {
        self.radio.finish().map_err(Error::Radio)
    }
    /// Tokens return only after healthy clock observations cover their complete pulse tails. RF delivery is not independently measured.
    pub fn poll(
        &mut self,
        elapsed_iq: &mut Vec<Receipt>,
        completed: &mut Vec<T>,
    ) -> Result<Observation, Error> {
        if completed.capacity() < self.max_pending_slots {
            return Err(Error::OutputCapacity);
        }
        completed.clear();
        self.poll_with(elapsed_iq, |pending| completed.push(pending.completion))
    }
    pub fn poll_completed(
        &mut self,
        elapsed_iq: &mut Vec<Receipt>,
        completed: &mut Vec<Completed<T>>,
    ) -> Result<Observation, Error> {
        if completed.capacity() < self.max_pending_slots {
            return Err(Error::OutputCapacity);
        }
        completed.clear();
        self.poll_with(elapsed_iq, |pending| {
            completed.push(Completed {
                slot_index: pending.index,
                scheduled_end_tick: pending.scheduled_end_tick,
                completion: pending.completion,
            })
        })
    }
    fn scheduled_end_tick(&self, index: u64) -> Result<u64, Error> {
        let slots = index
            .checked_add(1)
            .ok_or(Error::Timeline(super::tx_timeline::TimelineError::Range))?;
        self.radio
            .timeline()
            .ok_or(Error::State)?
            .end_tick_with_tail(slots)
            .map_err(Error::Timeline)
    }
    fn poll_with(
        &mut self,
        elapsed_iq: &mut Vec<Receipt>,
        mut complete: impl FnMut(Pending<T>),
    ) -> Result<Observation, Error> {
        let observation = self.radio.poll(elapsed_iq).map_err(Error::Radio)?;
        self.covered_tick = Some(observation.hardware_tick);
        if let Some(observer) = self.observer.as_mut() {
            for receipt in elapsed_iq.iter() {
                observer.completed(receipt);
            }
        }
        let watermark = self.radio.completed_slots();
        while self
            .pending
            .front()
            .is_some_and(|pending| pending.index < watermark)
        {
            complete(self.pending.pop_front().expect("pending front exists"));
        }
        Ok(observation)
    }
    pub(crate) fn take_rx_feed(&mut self) -> Option<super::buffered_receiver::Feed> {
        self.radio.take_rx_feed()
    }
    pub fn read(&mut self, buffer: &mut [Complex32]) -> Result<ReadOutcome, Error> {
        self.radio.read(buffer).map_err(Error::Radio)
    }
    pub fn stop(&mut self) -> Result<(), Error> {
        self.radio.stop().map_err(Error::Radio)
    }
    pub fn take_cancelled(&mut self, cancelled: &mut Vec<T>) -> Result<(), Error> {
        if !matches!(self.radio.state(), State::Stopped | State::Failed) {
            return Err(Error::State);
        }
        if cancelled.capacity() < self.max_pending_slots {
            return Err(Error::OutputCapacity);
        }
        cancelled.clear();
        cancelled.extend(self.pending.drain(..).map(|pending| pending.completion));
        Ok(())
    }
}
