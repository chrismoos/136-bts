use super::{Outcome, RachLinkError, RachReceiver};
use crate::phy::rach::BurstFormat;
use std::{collections::TryReserveError, error::Error as StdError, fmt, sync::Arc};
#[derive(Debug)]
pub enum Error {
    Limits,
    Allocation(TryReserveError),
    Capacity,
    Deadline,
    ClockRegression,
    Handle,
    Range,
    Link(RachLinkError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "access attempt failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug, Clone)]
pub struct Handle {
    owner: Arc<()>,
    index: usize,
    sequence: u64,
}
struct Attempt {
    sequence: u64,
    deadline_us: u64,
    receiver: RachReceiver,
}
pub struct Attempts {
    owner: Arc<()>,
    entries: Vec<Option<Attempt>>,
    next_sequence: u64,
    last_time_us: Option<u64>,
}
impl Attempts {
    pub fn try_new(max_attempts: usize) -> Result<Self, Error> {
        if max_attempts == 0 {
            return Err(Error::Limits);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(max_attempts)
            .map_err(Error::Allocation)?;
        entries.resize_with(max_attempts, || None);
        Ok(Self {
            owner: Arc::new(()),
            entries,
            next_sequence: 0,
            last_time_us: None,
        })
    }
    pub fn active_attempts(&self) -> usize {
        self.entries.iter().filter(|entry| entry.is_some()).count()
    }
    /// Bind this handle to one granted access resource. Continuation frames lack identity and cannot select their own context.
    pub fn open(&mut self, now_us: u64, deadline_us: u64) -> Result<Handle, Error> {
        if deadline_us <= now_us {
            return Err(Error::Deadline);
        }
        let next_sequence = self.next_sequence.checked_add(1).ok_or(Error::Range)?;
        self.expire(now_us)?;
        let index = self
            .entries
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        let sequence = self.next_sequence;
        self.entries[index] = Some(Attempt {
            sequence,
            deadline_us,
            receiver: RachReceiver::default(),
        });
        self.next_sequence = next_sequence;
        Ok(Handle {
            owner: self.owner.clone(),
            index,
            sequence,
        })
    }
    /// Times use one monotonic microsecond clock. Progress and duplicates never extend the original deadline.
    pub fn push(
        &mut self,
        handle: &Handle,
        now_us: u64,
        format: BurstFormat,
        bits: &[bool],
    ) -> Result<Outcome, Error> {
        self.attempt(handle)?;
        self.expire(now_us)?;
        self.attempt(handle)?
            .receiver
            .push(format, bits)
            .map_err(Error::Link)
    }
    pub fn retire(&mut self, handle: &Handle, now_us: u64) -> Result<(), Error> {
        self.attempt(handle)?;
        self.expire(now_us)?;
        self.attempt(handle)?;
        self.entries[handle.index] = None;
        Ok(())
    }
    pub fn expire(&mut self, now_us: u64) -> Result<usize, Error> {
        if self.last_time_us.is_some_and(|previous| now_us < previous) {
            return Err(Error::ClockRegression);
        }
        self.last_time_us = Some(now_us);
        let mut expired = 0;
        for entry in &mut self.entries {
            if entry
                .as_ref()
                .is_some_and(|attempt| attempt.deadline_us <= now_us)
            {
                *entry = None;
                expired += 1;
            }
        }
        Ok(expired)
    }
    pub fn discontinuity(&mut self) {
        self.entries.iter_mut().for_each(|entry| *entry = None);
    }
    fn attempt(&mut self, handle: &Handle) -> Result<&mut Attempt, Error> {
        if !Arc::ptr_eq(&self.owner, &handle.owner) {
            return Err(Error::Handle);
        }
        self.entries
            .get_mut(handle.index)
            .and_then(Option::as_mut)
            .filter(|attempt| attempt.sequence == handle.sequence)
            .ok_or(Error::Handle)
    }
}
#[cfg(test)]
mod tests;
