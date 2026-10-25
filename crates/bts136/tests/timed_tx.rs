use bts136::{
    codec::vselp::{SUBFRAMES, VselpParameters, VselpSubframe},
    link::voice::{FrameKind, TrafficFrameReceiver, TransmitFrame},
    phy::{
        acquisition::decode_forward_traffic, forward::ForwardModulator, modulation::Symbol,
        speech::CrcStatus, sync::SyncWord, traffic,
    },
    radio::{
        timed_tx::{Limits, State, TimedTx},
        tx_timeline::TxTimeline,
    },
};
use num_complex::Complex32;
use sdr_hal::{RadioTx, error::Error as HalError};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Capture {
    tick: u64,
    enabled: bool,
    fail_write: bool,
    start_tick: Option<u64>,
    samples: Vec<Complex32>,
}
struct Sink(Arc<Mutex<Capture>>);
impl RadioTx for Sink {
    fn tx_sample_rate_hz(&self) -> f64 {
        194_400.0
    }
    fn supports_timed_tx(&self) -> bool {
        true
    }
    fn tick_rate(&self) -> u64 {
        1000000
    }
    fn get_hardware_time(&self) -> Result<u64, HalError> {
        Ok(self.0.lock().unwrap().tick)
    }
    fn transmit(&mut self, _samples: &[Complex32]) -> Result<(), HalError> {
        Err("timestamp required".into())
    }
    fn transmit_at(&mut self, samples: &[Complex32], tick: Option<u64>) -> Result<(), HalError> {
        let mut capture = self.0.lock().unwrap();
        capture.start_tick = tick;
        if capture.fail_write {
            capture
                .samples
                .extend_from_slice(&samples[..samples.len() / 2]);
            return Err("incomplete native write".into());
        }
        capture.samples.extend_from_slice(samples);
        Ok(())
    }
    fn enable_transmit(&mut self, enable: bool) -> Result<(), HalError> {
        self.0.lock().unwrap().enabled = enable;
        Ok(())
    }
}
#[test]
fn generated_forward_iq_through_timed_hal_submission_decodes_exact_speech_without_early_elapsed_event()
 {
    const DVCC: u8 = 42;
    let parameters = VselpParameters::try_new(
        20,
        [32, 16, 16, 8, 8, 4, 4, 4, 4, 2],
        [VselpSubframe::try_new(21, 37, 54, 170).unwrap(); SUBFRAMES],
    )
    .unwrap();
    let frame = TransmitFrame::speech(&parameters);
    let data = TransmitFrame::interleave(&frame, &frame);
    let slot =
        traffic::encode_forward(SyncWord::One, &[false; traffic::SACCH_BITS], &data, DVCC, 0)
            .unwrap();
    let mut modulator = ForwardModulator::try_new(8).unwrap();
    let slot_samples = modulator.slot_samples();
    let mut iq = vec![Complex32::new(0.0, 0.0); slot_samples * 2 + modulator.tail_samples()];
    modulator
        .encode_slot_into(&slot, &mut iq[..slot_samples])
        .unwrap();
    modulator
        .encode_slot_into(&slot, &mut iq[slot_samples..slot_samples * 2])
        .unwrap();
    modulator.finish_into(&mut iq[slot_samples * 2..]).unwrap();
    let capture = Arc::new(Mutex::new(Capture::default()));
    let mut tx = TimedTx::try_new(
        Box::new(Sink(capture.clone())),
        modulator.sample_rate_hz() as u64,
        Limits {
            max_queued_blocks: 1,
            max_block_samples: iq.len(),
        },
    )
    .unwrap();
    let timeline = TxTimeline::try_new(2000, 1_000_000, &modulator).unwrap();
    let end_tick = timeline.end_tick_with_tail(2).unwrap();
    tx.start(timeline.iq_epoch_tick(), end_tick + 1).unwrap();
    let receipt = tx.submit(&iq).unwrap();
    assert_eq!(receipt.end_tick(), end_tick);
    let mut elapsed = Vec::with_capacity(1);
    capture.lock().unwrap().tick = end_tick - 1;
    tx.poll(&mut elapsed).unwrap();
    assert!(elapsed.is_empty());
    capture.lock().unwrap().tick = end_tick;
    tx.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed, [receipt]);
    capture.lock().unwrap().tick = end_tick + 1;
    assert_eq!(tx.poll(&mut elapsed).unwrap().state, State::Stopped);
    let capture = capture.lock().unwrap();
    assert!(!capture.enabled);
    assert_eq!(capture.start_tick, Some(timeline.iq_epoch_tick()));
    assert_eq!(timeline.symbol_center_tick(0).unwrap(), 2000);
    let samples: Vec<_> = capture
        .samples
        .iter()
        .map(|sample| Symbol {
            i: f64::from(sample.re),
            q: f64::from(sample.im),
        })
        .collect();
    let acquired = decode_forward_traffic(&samples, modulator.sample_rate_hz()).unwrap();
    assert_eq!(acquired.len(), 2);
    let mut receiver = TrafficFrameReceiver::new(DVCC, VselpParameters::default());
    assert!(
        receiver
            .push(Some(&acquired[0].fields.fields.data))
            .unwrap()
            .is_none()
    );
    let frame = receiver
        .push(Some(&acquired[1].fields.fields.data))
        .unwrap()
        .unwrap();
    assert!(matches!(
        frame.kind,
        FrameKind::Speech {
            crc: CrcStatus::Valid,
            ..
        }
    ));
    assert_eq!(frame.parameters, Some(parameters));
}

#[test]
fn sequential_carrier_backpressure_preserves_waveform_and_drains_tail_at_deadline() {
    use bts136::radio::forward_tx::{Error, ForwardTx, State as ForwardState};
    let capture = Arc::new(Mutex::new(Capture::default()));
    let mut reference = ForwardModulator::try_new(8).unwrap();
    let tx = TimedTx::try_new(
        Box::new(Sink(capture.clone())),
        reference.sample_rate_hz() as u64,
        Limits {
            max_queued_blocks: 2,
            max_block_samples: reference.slot_samples(),
        },
    )
    .unwrap();
    let mut carrier = ForwardTx::try_new(tx, 8).unwrap();
    assert_eq!(carrier.state(), ForwardState::Prepared);
    carrier.start(2000, 3).unwrap();
    let bits = std::array::from_fn(|index| index % 3 == 0);
    assert!(matches!(carrier.finish(), Err(Error::State)));
    assert!(matches!(
        carrier.submit_slot(1, &bits),
        Err(Error::Slot {
            expected: 0,
            actual: 1
        })
    ));
    let first = carrier.submit_slot(0, &bits).unwrap();
    let second = carrier.submit_slot(1, &bits).unwrap();
    assert_eq!(second.start_tick(), first.end_tick());
    assert!(matches!(
        carrier.submit_slot(2, &bits),
        Err(Error::QueueFull)
    ));
    let mut elapsed = Vec::with_capacity(2);
    capture.lock().unwrap().tick = first.end_tick();
    carrier.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed, [first]);
    assert_eq!(carrier.completed_slots(), 0);
    elapsed.clear();
    let third = carrier.submit_slot(2, &bits).unwrap();
    assert_eq!(third.start_tick(), second.end_tick());
    assert!(matches!(carrier.finish(), Err(Error::QueueFull)));
    capture.lock().unwrap().tick = second.end_tick();
    carrier.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed, [second]);
    assert_eq!(carrier.completed_slots(), 1);
    elapsed.clear();
    let tail = carrier.finish().unwrap();
    assert_eq!(tail.start_tick(), third.end_tick());
    assert_eq!(carrier.state(), ForwardState::Draining);
    assert!(matches!(carrier.finish(), Err(Error::State)));
    assert!(matches!(carrier.submit_slot(3, &bits), Err(Error::State)));
    capture.lock().unwrap().tick = tail.end_tick();
    carrier.poll(&mut elapsed).unwrap();
    assert_eq!(elapsed, [third, tail]);
    assert_eq!(carrier.completed_slots(), 3);
    assert_eq!(carrier.state(), ForwardState::Stopped);
    assert!(carrier.stop_acknowledged());
    assert!(!capture.lock().unwrap().enabled);
    let mut expected = Vec::new();
    for _ in 0..3 {
        expected.extend(reference.encode_slot(&bits));
    }
    expected.extend(reference.finish());
    assert_eq!(
        capture.lock().unwrap().samples,
        expected
            .into_iter()
            .map(|sample| Complex32::new(sample.i as f32, sample.q as f32))
            .collect::<Vec<_>>()
    );
}

#[test]
fn late_carrier_submission_is_rejected_without_stopping_or_replaying_iq() {
    use bts136::radio::forward_tx::{Error, ForwardTx, State as ForwardState};
    let capture = Arc::new(Mutex::new(Capture::default()));
    let tx = TimedTx::try_new(
        Box::new(Sink(capture.clone())),
        194400,
        Limits {
            max_queued_blocks: 2,
            max_block_samples: 1296,
        },
    )
    .unwrap();
    let mut carrier = ForwardTx::try_new(tx, 8).unwrap();
    carrier.start(2000, 2).unwrap();
    capture.lock().unwrap().tick = carrier.timeline().unwrap().iq_epoch_tick();
    assert!(matches!(
        carrier.submit_slot(0, &[false; traffic::SLOT_BITS]),
        Err(Error::Tx(_))
    ));
    assert_eq!(carrier.state(), ForwardState::Running);
    assert!(!carrier.stop_acknowledged());
    assert!(capture.lock().unwrap().enabled);
    assert!(capture.lock().unwrap().samples.is_empty());
    assert!(matches!(
        carrier.submit_slot(0, &[false; traffic::SLOT_BITS]),
        Err(Error::State)
    ));
    carrier.stop().unwrap();
    assert!(carrier.stop_acknowledged());
    assert!(!capture.lock().unwrap().enabled);
}

#[test]
fn carrier_rejects_applied_rate_mismatch_and_small_hal_buffer_before_activation() {
    use bts136::radio::forward_tx::{Error, ForwardTx};
    for (rate, capacity) in [(194401, 1296), (194400, 1295)] {
        let capture = Arc::new(Mutex::new(Capture::default()));
        let tx = TimedTx::try_new(
            Box::new(Sink(capture.clone())),
            rate,
            Limits {
                max_queued_blocks: 2,
                max_block_samples: capacity,
            },
        )
        .unwrap();
        let result = ForwardTx::try_new(tx, 8);
        if rate == 194401 {
            assert!(matches!(result, Err(Error::Rate { .. })));
        } else {
            assert!(matches!(result, Err(Error::Buffer)));
        }
        assert!(!capture.lock().unwrap().enabled);
        assert!(capture.lock().unwrap().samples.is_empty());
    }
}

#[test]
fn incomplete_carrier_write_is_stopped_and_never_retried() {
    use bts136::radio::forward_tx::{Error, ForwardTx, State as ForwardState};
    let capture = Arc::new(Mutex::new(Capture {
        fail_write: true,
        ..Capture::default()
    }));
    let modulator = ForwardModulator::try_new(8).unwrap();
    let tx = TimedTx::try_new(
        Box::new(Sink(capture.clone())),
        modulator.sample_rate_hz() as u64,
        Limits {
            max_queued_blocks: 2,
            max_block_samples: modulator.slot_samples(),
        },
    )
    .unwrap();
    let mut carrier = ForwardTx::try_new(tx, 8).unwrap();
    carrier.start(2000, 2).unwrap();
    assert!(matches!(
        carrier.submit_slot(0, &[true; traffic::SLOT_BITS]),
        Err(Error::Tx(_))
    ));
    assert_eq!(carrier.state(), ForwardState::Failed);
    assert!(carrier.stop_acknowledged());
    assert!(!capture.lock().unwrap().enabled);
    let partial = capture.lock().unwrap().samples.len();
    assert_eq!(partial, modulator.slot_samples() / 2);
    assert!(matches!(
        carrier.submit_slot(0, &[true; traffic::SLOT_BITS]),
        Err(Error::State)
    ));
    assert_eq!(capture.lock().unwrap().samples.len(), partial);
}
