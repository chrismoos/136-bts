use super::{
    RadioStreams,
    forward_tx::{self, ForwardTx},
    receiver::{self, ReadOutcome},
    timed_tx::{Observation, Receipt},
};
use crate::phy::traffic::SLOT_BITS;
use num_complex::Complex32;
use std::{error::Error as StdError, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Prepared,
    Running,
    Stopped,
    Failed,
}
#[derive(Debug)]
pub enum Failure {
    State,
    Tx(Box<forward_tx::Error>),
    Rx(Box<receiver::Error>),
    Shutdown,
}
#[derive(Debug)]
pub struct Error {
    pub failure: Failure,
    pub tx_stop_error: Option<Box<forward_tx::Error>>,
    pub rx_stop_error: Option<Box<receiver::Error>>,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "duplex radio failed: {self:?}")
    }
}
impl StdError for Error {}
pub struct Duplex {
    tx: ForwardTx,
    rx: super::buffered_receiver::BufferedReceiver,
    state: State,
}
impl Duplex {
    pub fn try_new(streams: RadioStreams, samples_per_symbol: usize) -> Result<Self, Error> {
        let tx = ForwardTx::try_new(streams.tx, samples_per_symbol)
            .map_err(|error| Self::local(Failure::Tx(Box::new(error))))?;
        if !matches!(
            streams.rx.state(),
            receiver::State::Prepared | receiver::State::Active
        ) {
            return Err(Self::local(Failure::State));
        }
        Ok(Self {
            tx,
            rx: super::buffered_receiver::BufferedReceiver::new(streams.rx),
            state: State::Prepared,
        })
    }
    pub(crate) fn buffer_receive(&mut self, realtime: crate::realtime::Config) {
        self.rx.enable_buffering(realtime);
    }
    pub(crate) fn timeline(&self) -> Option<&super::tx_timeline::TxTimeline> {
        self.tx.timeline()
    }
    pub fn completed_slots(&self) -> u64 {
        self.tx.completed_slots()
    }
    pub fn state(&self) -> State {
        self.state
    }
    /// Both acknowledgments describe stop requests, not an independent RF-state query.
    pub fn stop_acknowledged(&self) -> bool {
        self.tx.stop_acknowledged() && self.rx.stop_acknowledged()
    }
    pub fn start(
        &mut self,
        nominal_epoch_tick: u64,
        slots: u64,
        rx_tick: Option<u64>,
    ) -> Result<(), Error> {
        if self.state != State::Prepared {
            return Err(Self::local(Failure::State));
        }
        if let Err(error) = self.rx.start(rx_tick) {
            return Err(self.failed(Failure::Rx(Box::new(error))));
        }
        if let Err(error) = self.tx.start(nominal_epoch_tick, slots) {
            return Err(self.failed(Failure::Tx(Box::new(error))));
        }
        self.state = State::Running;
        Ok(())
    }
    pub fn start_continuous(
        &mut self,
        nominal_epoch_tick: u64,
        rx_tick: Option<u64>,
    ) -> Result<(), Error> {
        if self.state != State::Prepared {
            return Err(Self::local(Failure::State));
        }
        if let Err(error) = self.rx.start(rx_tick) {
            return Err(self.failed(Failure::Rx(Box::new(error))));
        }
        if let Err(error) = self.tx.start_continuous(nominal_epoch_tick) {
            return Err(self.failed(Failure::Tx(Box::new(error))));
        }
        self.state = State::Running;
        Ok(())
    }
    pub fn submit_slot(&mut self, index: u64, bits: &[bool; SLOT_BITS]) -> Result<Receipt, Error> {
        if self.state != State::Running {
            return Err(Self::local(Failure::State));
        }
        let result = self.tx.submit_slot(index, bits);
        self.tx_result(result)
    }
    pub(crate) fn skip_expired_slots(
        &mut self,
        index: u64,
        lead_ticks: u64,
        maximum_slots: u64,
    ) -> Result<u64, Error> {
        if self.state != State::Running {
            return Err(Self::local(Failure::State));
        }
        let result = self.tx.skip_expired_slots(index, lead_ticks, maximum_slots);
        self.tx_result(result)
    }
    pub fn finish(&mut self) -> Result<Receipt, Error> {
        if self.state != State::Running {
            return Err(Self::local(Failure::State));
        }
        let result = self.tx.finish();
        self.tx_result(result)
    }
    fn tx_result<T>(&mut self, result: Result<T, forward_tx::Error>) -> Result<T, Error> {
        match result {
            Ok(value) => Ok(value),
            Err(error) if self.tx.state() == forward_tx::State::Failed => {
                Err(self.failed(Failure::Tx(Box::new(error))))
            }
            Err(error) => Err(Self::local(Failure::Tx(Box::new(error)))),
        }
    }
    pub(crate) fn take_rx_feed(&mut self) -> Option<super::buffered_receiver::Feed> {
        self.rx.take_feed()
    }
    pub fn read(&mut self, buffer: &mut [Complex32]) -> Result<ReadOutcome, Error> {
        if self.state != State::Running {
            return Err(Self::local(Failure::State));
        }
        match self.rx.read(buffer) {
            Ok(outcome) => Ok(outcome),
            Err(error) if self.rx.state() == receiver::State::Failed => {
                Err(self.failed(Failure::Rx(Box::new(error))))
            }
            Err(error) => Err(Self::local(Failure::Rx(Box::new(error)))),
        }
    }
    /// Poll regularly to enforce TX deadlines. RX reads must use bounded timeouts that leave enough time to feed TX.
    pub fn poll(&mut self, elapsed: &mut Vec<Receipt>) -> Result<Observation, Error> {
        if self.state != State::Running {
            return Err(Self::local(Failure::State));
        }
        let result = self.tx.poll(elapsed);
        match result {
            Ok(observation) => {
                if self.tx.state() == forward_tx::State::Stopped {
                    self.stop()?;
                }
                Ok(observation)
            }
            Err(error) if self.tx.state() == forward_tx::State::Stopped => {
                Err(self.failed(Failure::Tx(Box::new(error))))
            }
            other => self.tx_result(other),
        }
    }
    pub fn stop(&mut self) -> Result<(), Error> {
        let tx_stop_error = self.tx.stop().err().map(Box::new);
        let rx_stop_error = self.rx.stop().err().map(Box::new);
        if tx_stop_error.is_none() && rx_stop_error.is_none() {
            self.state = State::Stopped;
            Ok(())
        } else {
            self.state = State::Failed;
            Err(Error {
                failure: Failure::Shutdown,
                tx_stop_error,
                rx_stop_error,
            })
        }
    }
    fn failed(&mut self, failure: Failure) -> Error {
        let tx_stop_error = self.tx.stop().err().map(Box::new);
        let rx_stop_error = self.rx.stop().err().map(Box::new);
        self.state = State::Failed;
        Error {
            failure,
            tx_stop_error,
            rx_stop_error,
        }
    }
    fn local(failure: Failure) -> Error {
        Error {
            failure,
            tx_stop_error: None,
            rx_stop_error: None,
        }
    }
}
impl Drop for Duplex {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
