use super::*;
use sdr_hal::{RadioRx, RxReadResult, error::Error as HalError};
use std::{
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};
const TEST_DEADLINE: Duration = Duration::from_secs(1);
const BLOCKED_IO_LIMIT: Duration = Duration::from_secs(2);
struct Device {
    active: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    fail: bool,
}
impl RadioRx for Device {
    fn rx_sample_rate_hz(&self) -> f64 {
        100.0
    }
    fn tick_rate(&self) -> u64 {
        1000
    }
    fn get_hardware_time(&self) -> Result<u64, HalError> {
        Ok(100)
    }
    fn set_rx_frequency(&mut self, _: f64) -> Result<(), HalError> {
        Ok(())
    }
    fn rx_activate(&mut self, _: Option<u64>) -> Result<(), HalError> {
        self.active.store(true, Ordering::Release);
        Ok(())
    }
    fn stop(&mut self) -> Result<(), HalError> {
        self.active.store(false, Ordering::Release);
        Ok(())
    }
    fn rx_read(&mut self, buffer: &mut [Complex32], _: i64) -> Result<RxReadResult, HalError> {
        let read = self.reads.fetch_add(1, Ordering::Relaxed);
        if let Some((entered, release)) = self.gate.take() {
            entered.send(()).unwrap();
            release
                .recv_timeout(BLOCKED_IO_LIMIT)
                .map_err(|_| HalError::from("test RX gate timed out"))?;
        }
        if self.fail {
            return Err(HalError::from("injected RX failure"));
        }
        buffer.fill(Complex32::new(0.25, -0.5));
        Ok(RxReadResult {
            samples_read: buffer.len(),
            time_ticks: 100 + read as u64 * 80,
            overflow: false,
        })
    }
}
fn receiver(device: Device) -> BufferedReceiver {
    let receiver = Receiver::try_new(
        Box::new(device),
        100,
        receiver::Limits {
            max_block_samples: 8,
            timeout_us: 1000,
        },
    )
    .unwrap();
    let mut receiver = BufferedReceiver::new(receiver);
    receiver.enable_buffering(crate::realtime::Config {
        enabled: false,
        ..Default::default()
    });
    receiver.start(None).unwrap();
    receiver
}
#[test]
fn blocked_backend_read_cannot_block_serving_and_stop_joins_the_reader() {
    let (entered, entry) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let active = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let mut receiver = receiver(Device {
        active: active.clone(),
        reads: reads.clone(),
        gate: Some((entered, gate)),
        fail: false,
    });
    entry.recv_timeout(TEST_DEADLINE).unwrap();
    let mut buffer = [Complex32::new(0.0, 0.0); 8];
    assert!(matches!(
        receiver.read(&mut buffer).unwrap(),
        ReadOutcome::Timeout { .. }
    ));
    release.send(()).unwrap();
    let deadline = Instant::now() + TEST_DEADLINE;
    let block = loop {
        if let ReadOutcome::Block(block) = receiver.read(&mut buffer).unwrap() {
            break block;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    };
    assert_eq!(block.time_ticks, 100);
    assert_eq!(block.first_sample, 0);
    assert_eq!(block.samples_read, 8);
    assert!(
        buffer
            .iter()
            .all(|sample| *sample == Complex32::new(0.25, -0.5))
    );
    thread::sleep(Duration::from_millis(10));
    assert!(
        reads.load(Ordering::Relaxed) <= WORKER_BUFFERS + 1,
        "RX prefetch grew beyond the reusable buffers"
    );
    receiver.stop().unwrap();
    assert!(receiver.stop_acknowledged());
    assert!(!active.load(Ordering::Acquire));
}
#[test]
fn worker_failure_is_reported_and_hardware_stop_is_acknowledged() {
    let active = Arc::new(AtomicBool::new(false));
    let mut receiver = receiver(Device {
        active: active.clone(),
        reads: Arc::new(AtomicUsize::new(0)),
        gate: None,
        fail: true,
    });
    let deadline = Instant::now() + TEST_DEADLINE;
    let mut buffer = [Complex32::new(0.0, 0.0); 8];
    loop {
        if receiver.read(&mut buffer).is_err() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(receiver.state(), receiver::State::Failed);
    receiver.stop().unwrap();
    assert!(receiver.stop_acknowledged());
    assert!(!active.load(Ordering::Acquire));
}
#[test]
fn taken_feed_delivers_contiguous_blocks_and_recycles_buffers() {
    let active = Arc::new(AtomicBool::new(false));
    let mut receiver = receiver(Device {
        active: active.clone(),
        reads: Arc::new(AtomicUsize::new(0)),
        gate: None,
        fail: false,
    });
    let feed = receiver.take_feed().unwrap();
    assert!(receiver.take_feed().is_none());
    let mut buffer = [Complex32::new(0.0, 0.0); 8];
    assert!(receiver.read(&mut buffer).is_err());
    for expected in 0..WORKER_BUFFERS as u64 * 2 {
        let received = feed.recv_timeout(TEST_DEADLINE).unwrap().unwrap();
        assert_eq!(received.block.first_sample, expected * 8);
        assert_eq!(received.block.samples_read, 8);
        feed.recycle(received.samples).unwrap();
    }
    drop(feed);
    receiver.stop().unwrap();
    assert!(receiver.stop_acknowledged());
    assert!(!active.load(Ordering::Acquire));
}
