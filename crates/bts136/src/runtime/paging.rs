use crate::{
    core::{
        call_paging::{CallPaging, PagingError, PagingOutcome},
        calls::CallHandle,
    },
    link::spach::paging,
    phy::{
        control::{self, LogicalChannel},
        traffic::SLOT_BITS,
    },
    radio::{
        carrier::{self, Carrier},
        duplex::Duplex,
        receiver::ReadOutcome,
        timed_tx::{Observation, Receipt},
    },
    scheduler::{broadcast::PHYSICAL_SLOTS, paging::PagingAllocation},
};
use num_complex::Complex32;
use std::{error::Error as StdError, fmt};
const CONTROL_SLOT_PERIOD: u64 = 3;
#[derive(Debug)]
pub enum Error {
    Carrier(carrier::Error),
    Page,
    UnknownAttempt,
    OutputCapacity,
    Paging(PagingError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "paging radio failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug)]
pub struct PageEvent {
    pub handle: CallHandle,
    pub result: Result<Option<PagingOutcome>, PagingError>,
}
pub struct PagingRadio {
    carrier: Carrier<Option<CallHandle>>,
    calls: CallPaging,
    allocation: PagingAllocation,
    dvcc: u8,
    completed: Vec<Option<CallHandle>>,
    max_pending_slots: usize,
}
impl PagingRadio {
    pub fn try_new(
        radio: Duplex,
        calls: CallPaging,
        allocation: PagingAllocation,
        dvcc: u8,
        max_pending_slots: usize,
    ) -> Result<Self, Error> {
        let carrier = Carrier::try_new(radio, max_pending_slots).map_err(Error::Carrier)?;
        let mut completed = Vec::new();
        completed
            .try_reserve_exact(max_pending_slots)
            .map_err(|error| Error::Carrier(carrier::Error::Allocation(error)))?;
        Ok(Self {
            carrier,
            calls,
            allocation,
            dvcc,
            completed,
            max_pending_slots,
        })
    }
    pub fn calls_mut(&mut self) -> &mut CallPaging {
        &mut self.calls
    }
    pub fn start(
        &mut self,
        nominal_epoch_tick: u64,
        slots: u64,
        rx_tick: Option<u64>,
    ) -> Result<(), Error> {
        self.carrier
            .start(nominal_epoch_tick, slots, rx_tick)
            .map_err(Error::Carrier)
    }
    pub fn submit_slot(
        &mut self,
        index: u64,
        bits: &[bool; SLOT_BITS],
        page: Option<&CallHandle>,
    ) -> Result<Receipt, Error> {
        if let Some(handle) = page {
            let request = self
                .calls
                .requests()
                .find(|request| request.handle.matches(handle))
                .ok_or(Error::UnknownAttempt)?;
            let phase = self
                .allocation
                .phase(request.recipient)
                .map_err(|_| Error::Page)?;
            if !index.is_multiple_of(CONTROL_SLOT_PERIOD)
                || (index % PHYSICAL_SLOTS) / CONTROL_SLOT_PERIOD != u64::from(phase)
            {
                return Err(Error::Page);
            }
            let decoded = control::decode_forward(bits, LogicalChannel::Spach, self.dvcc)
                .map_err(|_| Error::Page)?;
            let frame = paging::decode(&decoded.block.information).map_err(|_| Error::Page)?;
            if decoded.superframe_phase != phase
                || !frame.addresses(request.recipient)
                || frame.flags.paging_frame_modifier
            {
                return Err(Error::Page);
            }
        }
        self.carrier
            .submit_slot(index, bits, page.cloned())
            .map_err(|rejected| Error::Carrier(rejected.error))
    }
    pub fn finish(&mut self) -> Result<Receipt, Error> {
        self.carrier.finish().map_err(Error::Carrier)
    }
    pub fn poll(
        &mut self,
        elapsed_iq: &mut Vec<Receipt>,
        monotonic_ms: u64,
        events: &mut Vec<PageEvent>,
    ) -> Result<Observation, Error> {
        if events.capacity() < self.max_pending_slots {
            return Err(Error::OutputCapacity);
        }
        events.clear();
        elapsed_iq.clear();
        let observation = self
            .carrier
            .poll(elapsed_iq, &mut self.completed)
            .map_err(Error::Carrier)?;
        for handle in self.completed.drain(..).flatten() {
            let result = self.calls.transmitted(&handle, monotonic_ms);
            events.push(PageEvent { handle, result });
        }
        Ok(observation)
    }
    pub fn advance_deadlines(
        &mut self,
        monotonic_ms: u64,
        expired: &mut Vec<PagingOutcome>,
    ) -> Result<(), Error> {
        self.calls
            .advance_into(monotonic_ms, expired)
            .map_err(Error::Paging)
    }
    pub fn read(&mut self, buffer: &mut [Complex32]) -> Result<ReadOutcome, Error> {
        self.carrier.read(buffer).map_err(Error::Carrier)
    }
    pub fn stop(&mut self) -> Result<(), Error> {
        self.carrier.stop().map_err(Error::Carrier)
    }
    pub fn take_cancelled(&mut self, cancelled: &mut Vec<CallHandle>) -> Result<(), Error> {
        if cancelled.capacity() < self.max_pending_slots {
            return Err(Error::OutputCapacity);
        }
        self.carrier
            .take_cancelled(&mut self.completed)
            .map_err(Error::Carrier)?;
        cancelled.clear();
        cancelled.extend(self.completed.drain(..).flatten());
        Ok(())
    }
}
