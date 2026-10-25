use super::receiver::{self, ReadOutcome, Receiver};
use num_complex::Complex32;
const EMPTY_READ_BACKOFF: std::time::Duration = std::time::Duration::from_micros(100);
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};
struct Packet {
    outcome: ReadOutcome,
    samples: Vec<Complex32>,
}
struct Worker {
    data: mpsc::Receiver<Packet>,
    errors: mpsc::Receiver<receiver::Error>,
    recycle: Option<mpsc::SyncSender<Vec<Complex32>>>,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<(Receiver, Result<(), receiver::Error>)>,
}
pub(super) struct BufferedReceiver {
    receiver: Option<Receiver>,
    worker: Option<Worker>,
    realtime: crate::realtime::Config,
    buffered: bool,
    failed: bool,
}
impl BufferedReceiver {
    pub fn new(receiver: Receiver) -> Self {
        Self {
            receiver: Some(receiver),
            worker: None,
            realtime: crate::realtime::Config {
                enabled: false,
                ..Default::default()
            },
            buffered: false,
            failed: false,
        }
    }
    pub fn enable_buffering(&mut self, realtime: crate::realtime::Config) {
        self.realtime = realtime;
        self.buffered = true;
    }
    pub fn state(&self) -> receiver::State {
        if self.failed {
            return receiver::State::Failed;
        }
        self.receiver
            .as_ref()
            .map_or(receiver::State::Active, Receiver::state)
    }
    pub fn stop_acknowledged(&self) -> bool {
        self.receiver
            .as_ref()
            .is_some_and(Receiver::stop_acknowledged)
    }
    pub fn start(&mut self, tick: Option<u64>) -> Result<(), receiver::Error> {
        let receiver = self
            .receiver
            .as_mut()
            .ok_or_else(|| local(receiver::Failure::State))?;
        if receiver.state() != receiver::State::Active {
            receiver.start(tick)?;
        }
        if !self.buffered {
            return Ok(());
        }
        let samples = vec![Complex32::new(0.0, 0.0); receiver.max_block_samples()];
        let (data_tx, data) = mpsc::sync_channel(1);
        let (error_tx, errors) = mpsc::channel();
        let (recycle_tx, recycle) = mpsc::sync_channel(1);
        recycle_tx
            .send(samples)
            .map_err(|_| local(receiver::Failure::WorkerStopped))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let mut receiver = self.receiver.take().expect("prepared receiver exists");
        let realtime = self.realtime;
        let thread = thread::Builder::new()
            .name("136bts-rx".into())
            .spawn(move || {
                let _scheduling = crate::realtime::apply(realtime, crate::realtime::Role::Rx);
                while !stopping.load(Ordering::Acquire) {
                    let Ok(mut samples) = recycle.recv() else {
                        break;
                    };
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    loop {
                        if stopping.load(Ordering::Acquire) {
                            break;
                        }
                        match receiver.read(&mut samples) {
                            Ok(outcome @ ReadOutcome::Block(_)) => {
                                if data_tx.try_send(Packet { outcome, samples }).is_err() {
                                    stopping.store(true, Ordering::Release);
                                }
                                break;
                            }
                            Ok(_) => thread::sleep(EMPTY_READ_BACKOFF),
                            Err(error) => {
                                let _ = error_tx.send(error);
                                stopping.store(true, Ordering::Release);
                                break;
                            }
                        }
                    }
                }
                let stopped = receiver.stop();
                (receiver, stopped)
            })
            .map_err(|error| local(receiver::Failure::Worker(error)))?;
        self.worker = Some(Worker {
            data,
            errors,
            recycle: Some(recycle_tx),
            stop,
            thread,
        });
        Ok(())
    }
    pub fn read(&mut self, buffer: &mut [Complex32]) -> Result<ReadOutcome, receiver::Error> {
        let Some(worker) = self.worker.as_ref() else {
            return self
                .receiver
                .as_mut()
                .ok_or_else(|| local(receiver::Failure::WorkerStopped))?
                .read(buffer);
        };
        if let Ok(error) = worker.errors.try_recv() {
            self.failed = true;
            return Err(error);
        }
        // TX scheduling requires a nonblocking poll of the device thread.
        let packet = match worker.data.try_recv() {
            Ok(packet) => packet,
            Err(mpsc::TryRecvError::Empty) => {
                return Ok(ReadOutcome::Timeout {
                    hardware_overflow: false,
                });
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.failed = true;
                return Err(worker
                    .errors
                    .try_recv()
                    .unwrap_or_else(|_| local(receiver::Failure::WorkerStopped)));
            }
        };
        let ReadOutcome::Block(block) = packet.outcome else {
            return Err(local(receiver::Failure::WorkerStopped));
        };
        if block.samples_read > buffer.len() {
            self.failed = true;
            return Err(local(receiver::Failure::Buffer(buffer.len())));
        }
        buffer[..block.samples_read].copy_from_slice(&packet.samples[..block.samples_read]);
        worker
            .recycle
            .as_ref()
            .ok_or_else(|| local(receiver::Failure::WorkerStopped))?
            .try_send(packet.samples)
            .map_err(|_| local(receiver::Failure::WorkerStopped))?;
        Ok(packet.outcome)
    }
    pub fn stop(&mut self) -> Result<(), receiver::Error> {
        if let Some(mut worker) = self.worker.take() {
            worker.stop.store(true, Ordering::Release);
            worker.recycle.take();
            match worker.thread.join() {
                Ok((receiver, stopped)) => {
                    self.receiver = Some(receiver);
                    stopped?;
                }
                Err(_) => {
                    self.failed = true;
                    return Err(local(receiver::Failure::WorkerPanicked));
                }
            }
        }
        if let Some(receiver) = self.receiver.as_mut()
            && (receiver.state() != receiver::State::Stopped || !receiver.stop_acknowledged())
        {
            receiver.stop()?;
        }
        Ok(())
    }
}
impl Drop for BufferedReceiver {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn local(failure: receiver::Failure) -> receiver::Error {
    receiver::Error {
        failure,
        stop_error: None,
    }
}
#[cfg(test)]
mod tests;
