use super::receiver::{self, Block, ReadOutcome, Receiver};
use num_complex::Complex32;
const EMPTY_READ_BACKOFF: std::time::Duration = std::time::Duration::from_micros(100);
// Buffers the device thread may fill before the consumer returns one. Simulated radios in unit
// tests derive their clock from reads, so a single buffer keeps that clock behind RX decoding.
pub(crate) const WORKER_BUFFERS: usize = if cfg!(test) { 1 } else { 16 };
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
struct Packet {
    outcome: ReadOutcome,
    samples: Vec<Complex32>,
}
struct Worker {
    feed: Option<Feed>,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<(Receiver, Result<(), receiver::Error>)>,
}
/// Blocks read by the device thread. Return every buffer with `recycle`, or the device thread stalls.
/// Drop the feed before stopping the receiver.
pub(crate) struct Feed {
    data: mpsc::Receiver<Packet>,
    errors: mpsc::Receiver<receiver::Error>,
    recycle: mpsc::SyncSender<Vec<Complex32>>,
}
pub(crate) struct FeedBlock {
    pub block: Block,
    pub samples: Vec<Complex32>,
}
impl Feed {
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<FeedBlock>, receiver::Error> {
        if let Ok(error) = self.errors.try_recv() {
            return Err(error);
        }
        let packet = match self.data.recv_timeout(timeout) {
            Ok(packet) => packet,
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(self
                    .errors
                    .try_recv()
                    .unwrap_or_else(|_| local(receiver::Failure::WorkerStopped)));
            }
        };
        let ReadOutcome::Block(block) = packet.outcome else {
            return Err(local(receiver::Failure::WorkerStopped));
        };
        Ok(Some(FeedBlock {
            block,
            samples: packet.samples,
        }))
    }
    pub fn recycle(&self, samples: Vec<Complex32>) -> Result<(), receiver::Error> {
        self.recycle
            .try_send(samples)
            .map_err(|_| local(receiver::Failure::WorkerStopped))
    }
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
        let (data_tx, data) = mpsc::sync_channel(WORKER_BUFFERS);
        let (error_tx, errors) = mpsc::channel();
        let (recycle_tx, recycle) = mpsc::sync_channel(WORKER_BUFFERS);
        for _ in 0..WORKER_BUFFERS {
            recycle_tx
                .send(vec![Complex32::new(0.0, 0.0); receiver.max_block_samples()])
                .map_err(|_| local(receiver::Failure::WorkerStopped))?;
        }
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
            feed: Some(Feed {
                data,
                errors,
                recycle: recycle_tx,
            }),
            stop,
            thread,
        });
        Ok(())
    }
    /// Hands the device thread's blocks to another consumer. `read` fails afterward.
    pub(crate) fn take_feed(&mut self) -> Option<Feed> {
        self.worker.as_mut().and_then(|worker| worker.feed.take())
    }
    pub fn read(&mut self, buffer: &mut [Complex32]) -> Result<ReadOutcome, receiver::Error> {
        let Some(worker) = self.worker.as_ref() else {
            return self
                .receiver
                .as_mut()
                .ok_or_else(|| local(receiver::Failure::WorkerStopped))?
                .read(buffer);
        };
        let Some(worker) = worker.feed.as_ref() else {
            return Err(local(receiver::Failure::State));
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
            .try_send(packet.samples)
            .map_err(|_| local(receiver::Failure::WorkerStopped))?;
        Ok(packet.outcome)
    }
    pub fn stop(&mut self) -> Result<(), receiver::Error> {
        if let Some(mut worker) = self.worker.take() {
            worker.stop.store(true, Ordering::Release);
            worker.feed.take();
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
