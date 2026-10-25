use super::{
    timed_tx::{self, Observation, Receipt, TimedTx},
    tx_timeline::{TimelineError, TxTimeline},
};
use crate::phy::{forward::ForwardModulator, pulse::InvalidPulseRate, traffic::SLOT_BITS};
use num_complex::Complex32;
use std::{error::Error as StdError, fmt};

#[derive(Debug)]
pub enum Error {
    Modulator(InvalidPulseRate),
    Rate { applied_hz: u64, generated_hz: u64 },
    Buffer,
    State,
    Slot { expected: u64, actual: u64 },
    QueueFull,
    Timeline(TimelineError),
    Tx(timed_tx::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "forward TX failed: {self:?}")
    }
}
impl StdError for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Prepared,
    Running,
    Draining,
    Stopped,
    Failed,
}
pub struct ForwardTx {
    tx: TimedTx,
    modulator: ForwardModulator,
    buffer: Vec<Complex32>,
    timeline: Option<TxTimeline>,
    slots: u64,
    next_slot: u64,
    completed_slots: u64,
    submitted_end_tick: u64,
    state: State,
    // A rejected late slot has advanced modulation and must not be encoded twice.
    encoded_slot: bool,
}
impl ForwardTx {
    pub fn try_new(tx: TimedTx, samples_per_symbol: usize) -> Result<Self, Error> {
        if tx.state() != timed_tx::State::Prepared {
            return Err(Error::State);
        }
        let modulator = ForwardModulator::try_new(samples_per_symbol).map_err(Error::Modulator)?;
        let (applied_hz, _) = tx.rates();
        let generated_hz = modulator.sample_rate_hz() as u64;
        if applied_hz != generated_hz {
            return Err(Error::Rate {
                applied_hz,
                generated_hz,
            });
        }
        if tx.max_block_samples() < modulator.slot_samples() {
            return Err(Error::Buffer);
        }
        let buffer = vec![Complex32::new(0.0, 0.0); modulator.slot_samples()];
        Ok(Self {
            tx,
            modulator,
            buffer,
            timeline: None,
            slots: 0,
            next_slot: 0,
            completed_slots: 0,
            submitted_end_tick: 0,
            state: State::Prepared,
            encoded_slot: false,
        })
    }
    pub fn state(&self) -> State {
        self.state
    }
    /// Healthy clock observations and submitted IQ cover each counted slot's full filter tail. This is not an independent RF-state query.
    pub fn completed_slots(&self) -> u64 {
        self.completed_slots
    }
    pub fn timeline(&self) -> Option<&TxTimeline> {
        self.timeline.as_ref()
    }
    pub fn stop_acknowledged(&self) -> bool {
        self.tx.stop_acknowledged()
    }
    pub fn start(&mut self, nominal_epoch_tick: u64, slots: u64) -> Result<(), Error> {
        if slots == 0 {
            return Err(Error::State);
        }
        self.start_window(nominal_epoch_tick, Some(slots))
    }
    pub fn start_continuous(&mut self, nominal_epoch_tick: u64) -> Result<(), Error> {
        self.start_window(nominal_epoch_tick, None)
    }
    fn start_window(&mut self, nominal_epoch_tick: u64, slots: Option<u64>) -> Result<(), Error> {
        if self.state != State::Prepared {
            return Err(Error::State);
        }
        let timeline = TxTimeline::try_new(nominal_epoch_tick, self.tx.rates().1, &self.modulator)
            .map_err(Error::Timeline)?;
        let end = match slots {
            Some(slots) => timeline
                .end_tick_with_tail(slots)
                .map_err(Error::Timeline)?,
            None => u64::MAX,
        };
        if let Err(error) = self.tx.start(timeline.iq_epoch_tick(), end) {
            if self.tx.state() == timed_tx::State::Failed {
                self.state = State::Failed;
            }
            return Err(Error::Tx(error));
        }
        self.timeline = Some(timeline);
        self.slots = slots.unwrap_or(u64::MAX);
        self.state = State::Running;
        Ok(())
    }
    pub fn submit_slot(&mut self, index: u64, bits: &[bool; SLOT_BITS]) -> Result<Receipt, Error> {
        if self.state != State::Running || self.next_slot == self.slots {
            return Err(Error::State);
        }
        if index != self.next_slot {
            return Err(Error::Slot {
                expected: self.next_slot,
                actual: index,
            });
        }
        if self.tx.queue_full() {
            return Err(Error::QueueFull);
        }
        if self.encoded_slot {
            return Err(Error::State);
        }
        self.modulator
            .encode_slot_into(bits, &mut self.buffer)
            .expect("buffer sized for one slot at construction");
        self.encoded_slot = true;
        let result = self.tx.submit(&self.buffer);
        let receipt = self.submitted(result)?;
        self.encoded_slot = false;
        self.submitted_end_tick = receipt.end_tick();
        self.next_slot += 1;
        Ok(receipt)
    }
    pub(crate) fn skip_expired_slots(
        &mut self,
        index: u64,
        lead_ticks: u64,
        maximum_slots: u64,
    ) -> Result<u64, Error> {
        if self.state != State::Running || self.next_slot == self.slots {
            return Err(Error::State);
        }
        if index != self.next_slot {
            return Err(Error::Slot {
                expected: self.next_slot,
                actual: index,
            });
        }
        let skipped = match self.tx.skip_expired_blocks(
            self.modulator.slot_samples(),
            lead_ticks,
            maximum_slots.min(self.slots - self.next_slot),
        ) {
            Ok(skipped) => skipped,
            Err(error) => {
                if self.tx.state() == timed_tx::State::Failed {
                    self.state = State::Failed;
                }
                return Err(Error::Tx(error));
            }
        };
        if skipped != 0 {
            let zero_slots = skipped - u64::from(self.encoded_slot);
            self.modulator
                .skip_zero_slots_into(zero_slots, &mut self.buffer)
                .expect("one-slot buffer sized at construction");
            self.encoded_slot = false;
            self.next_slot += skipped;
        }
        Ok(skipped)
    }
    pub fn finish(&mut self) -> Result<Receipt, Error> {
        if self.state != State::Running || self.next_slot != self.slots {
            return Err(Error::State);
        }
        if self.tx.queue_full() {
            return Err(Error::QueueFull);
        }
        let tail = self.modulator.tail_samples();
        self.modulator
            .finish_into(&mut self.buffer[..tail])
            .expect("filter tail fits one slot buffer");
        let result = self.tx.submit(&self.buffer[..tail]);
        let receipt = self.submitted(result)?;
        self.submitted_end_tick = receipt.end_tick();
        self.state = State::Draining;
        Ok(receipt)
    }
    fn submitted(&mut self, result: Result<Receipt, timed_tx::Error>) -> Result<Receipt, Error> {
        match result {
            Ok(receipt) => Ok(receipt),
            Err(error) if matches!(error.failure, timed_tx::Failure::Late { .. }) => {
                Err(Error::Tx(error))
            }
            Err(mut error) => {
                self.state = State::Failed;
                if let Err(stop) = self.tx.stop()
                    && error.stop_error.is_none()
                {
                    error.stop_error = stop.stop_error.or_else(|| match stop.failure {
                        timed_tx::Failure::Hal(error) => Some(error),
                        _ => None,
                    });
                }
                Err(Error::Tx(error))
            }
        }
    }
    pub fn poll(&mut self, elapsed: &mut Vec<Receipt>) -> Result<Observation, Error> {
        if !matches!(self.state, State::Running | State::Draining) {
            return Err(Error::State);
        }
        match self.tx.poll(elapsed) {
            Ok(observation) => {
                self.completed_slots = self
                    .timeline
                    .as_ref()
                    .expect("running TX has a timeline")
                    .elapsed_slots_with_tail(
                        observation.hardware_tick,
                        self.submitted_end_tick,
                        self.next_slot,
                    )
                    .map_err(Error::Timeline)?;
                if observation.state == timed_tx::State::Stopped {
                    self.state = State::Stopped;
                }
                Ok(observation)
            }
            Err(error) => {
                if self.tx.state() == timed_tx::State::Failed {
                    self.state = State::Failed;
                } else if self.tx.state() == timed_tx::State::Stopped {
                    self.state = State::Stopped;
                }
                Err(Error::Tx(error))
            }
        }
    }
    pub fn stop(&mut self) -> Result<(), Error> {
        match self.tx.stop() {
            Ok(()) => {
                self.state = State::Stopped;
                Ok(())
            }
            Err(error) => {
                self.state = State::Failed;
                Err(Error::Tx(error))
            }
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use sdr_hal::{RadioTx, error::Error as HalError};
    use std::sync::{Arc, Mutex};
    const CLOCK_HZ: u64 = 1_000_000_000;
    const EPOCH: u64 = CLOCK_HZ;
    const STALLED_TICK: u64 = EPOCH + 1_000_000;
    #[derive(Default)]
    struct Device {
        tick: u64,
        enabled: bool,
        writes: Vec<(u64, Vec<Complex32>)>,
    }
    struct Tx(Arc<Mutex<Device>>);
    impl RadioTx for Tx {
        fn tx_sample_rate_hz(&self) -> f64 {
            crate::config::SAMPLE_RATE_HZ as f64
        }
        fn supports_timed_tx(&self) -> bool {
            true
        }
        fn tick_rate(&self) -> u64 {
            CLOCK_HZ
        }
        fn get_hardware_time(&self) -> Result<u64, HalError> {
            Ok(self.0.lock().unwrap().tick)
        }
        fn enable_transmit(&mut self, enable: bool) -> Result<(), HalError> {
            self.0.lock().unwrap().enabled = enable;
            Ok(())
        }
        fn transmit(&mut self, _: &[Complex32]) -> Result<(), HalError> {
            Err("timed writes required".into())
        }
        fn transmit_at(
            &mut self,
            samples: &[Complex32],
            tick: Option<u64>,
        ) -> Result<(), HalError> {
            self.0
                .lock()
                .unwrap()
                .writes
                .push((tick.unwrap(), samples.to_vec()));
            Ok(())
        }
    }
    #[test]
    fn clock_advancing_after_preflight_drops_old_iq_without_double_advancing_modulation() {
        let device = Arc::new(Mutex::new(Device::default()));
        let mut reference = ForwardModulator::try_new(crate::config::SAMPLES_PER_SYMBOL).unwrap();
        let timed = TimedTx::try_new(
            Box::new(Tx(device.clone())),
            crate::config::SAMPLE_RATE_HZ as u64,
            timed_tx::Limits {
                max_queued_blocks: 4,
                max_block_samples: reference.slot_samples(),
            },
        )
        .unwrap();
        let mut forward = ForwardTx::try_new(timed, crate::config::SAMPLES_PER_SYMBOL).unwrap();
        forward.start_continuous(EPOCH).unwrap();
        assert_eq!(forward.skip_expired_slots(0, 0, 1).unwrap(), 0);
        device.lock().unwrap().tick = STALLED_TICK;
        let first = std::array::from_fn(|index| index % 3 == 0);
        assert!(matches!(
            forward.submit_slot(0, &first),
            Err(Error::Tx(timed_tx::Error {
                failure: timed_tx::Failure::Late { .. },
                ..
            }))
        ));
        assert_eq!(forward.state(), State::Running);
        assert_eq!(forward.skip_expired_slots(0, 0, 1).unwrap(), 1);
        assert_eq!(forward.skip_expired_slots(1, 0, 1).unwrap(), 0);
        assert!(device.lock().unwrap().writes.is_empty());
        let second = std::array::from_fn(|index| index % 5 == 0);
        let receipt = forward.submit_slot(1, &second).unwrap();
        assert_eq!(
            receipt.start_tick(),
            forward
                .timeline()
                .unwrap()
                .sample_tick(reference.slot_samples() as u64)
                .unwrap()
        );
        reference.encode_slot(&first);
        let expected: Vec<_> = reference
            .encode_slot(&second)
            .iter()
            .map(|symbol| Complex32::new(symbol.i as f32, symbol.q as f32))
            .collect();
        let device = device.lock().unwrap();
        assert_eq!(device.writes.len(), 1);
        assert_eq!(device.writes[0].1, expected);
        assert!(device.enabled);
    }
}
