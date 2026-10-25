use super::*;
use crate::l3::legacy_traffic::{DtmfDigit, DtmfMessage};
const CONTINUOUS_HOLD_INTERVALS: u64 = 60;
const AFTER_STOP_INTERVALS: u64 = 12;
const EXPECTED_ACKS: usize = 5;
const ROWS: [f64; 4] = [697.0, 770.0, 852.0, 941.0];
const COLUMNS: [f64; 3] = [1209.0, 1336.0, 1477.0];
#[derive(Default)]
pub(super) struct Probe {
    queued: usize,
    acknowledged: usize,
    command: Option<crate::link::traffic_tx::CommandHandle>,
    completed: Option<u64>,
    last_ack: Option<u64>,
    pcm: Vec<f64>,
    pairs: Vec<(usize, usize)>,
    restored_speech_frames: usize,
}
impl Probe {
    fn request(stage: usize, index: usize) -> DtmfMessage {
        match stage {
            0 | 1 => DtmfMessage::Burst {
                request_number: 15,
                digits: Some(if index == CALLER {
                    vec![DtmfDigit::One, DtmfDigit::Two]
                } else {
                    vec![DtmfDigit::Eight, DtmfDigit::Nine]
                }),
            },
            2 => DtmfMessage::Burst {
                request_number: 0,
                digits: Some(vec![if index == CALLER {
                    DtmfDigit::Zero
                } else {
                    DtmfDigit::Hash
                }]),
            },
            3 => DtmfMessage::Continuous {
                request_number: 15,
                digit: if index == CALLER {
                    DtmfDigit::Five
                } else {
                    DtmfDigit::Seven
                },
            },
            4 => DtmfMessage::Continuous {
                request_number: 0,
                digit: DtmfDigit::Null,
            },
            _ => unreachable!("bounded DTMF script"),
        }
    }
    pub fn drive(
        &mut self,
        index: usize,
        interval: u64,
        tx: &mut TrafficTx,
    ) -> Result<(), HalError> {
        if self.queued >= EXPECTED_ACKS || self.queued != self.acknowledged {
            return Ok(());
        }
        if self.queued == 4
            && self
                .last_ack
                .is_none_or(|at| interval < at + CONTINUOUS_HOLD_INTERVALS)
        {
            return Ok(());
        }
        self.command = Some(
            tx.enqueue(&TrafficMessage::Dtmf(Self::request(self.queued, index)))
                .map_err(hal)?,
        );
        self.queued += 1;
        Ok(())
    }
    pub fn submitted(
        &mut self,
        completed: &Option<crate::link::traffic_tx::CommandHandle>,
        interval: u64,
    ) {
        if completed.is_some() && completed == &self.command {
            self.completed = Some(interval);
            self.command = None;
        }
    }
    pub fn acknowledgment(
        &mut self,
        index: usize,
        interval: u64,
        ack: &DtmfMessage,
    ) -> Result<(), HalError> {
        let request = Self::request(self.acknowledged, index);
        if request.acknowledgment().as_ref() != Some(ack) {
            return Err("DTMF acknowledgment does not match pending request".into());
        }
        let sent = self
            .completed
            .take()
            .ok_or("DTMF acknowledged before final request transmission")?;
        let symbols = interval
            .checked_sub(sent)
            .ok_or("DTMF response predates request")?
            * CONTROL_SLOT_PERIOD
            * SYMBOLS_PER_SLOT as u64
            + REVERSE_LEAD_SYMBOLS as u64;
        if symbols as f64 * 1000.0 > SYMBOL_RATE_HZ * FACCH_RESPONSE_TIMEOUT_MS as f64 {
            return Err("DTMF acknowledgment exceeded 200 ms FACCH response timeout".into());
        }
        self.acknowledged += 1;
        self.last_ack = Some(interval);
        Ok(())
    }
    pub fn restored_speech(&mut self, interval: u64) {
        if self.acknowledged == EXPECTED_ACKS && self.last_ack.is_some_and(|at| interval > at + 4) {
            self.restored_speech_frames += 1;
        }
    }
    pub fn audio(&mut self, pcm: &[f64]) {
        self.pcm.extend_from_slice(pcm);
        const WINDOW: usize = crate::codec::vselp::FRAME_SAMPLES * 2;
        if self.pcm.len() < WINDOW {
            return;
        }
        let samples = &self.pcm[self.pcm.len() - WINDOW..];
        let row = (0..ROWS.len())
            .max_by(|a, b| power(samples, ROWS[*a]).total_cmp(&power(samples, ROWS[*b])))
            .unwrap();
        let column = (0..COLUMNS.len())
            .max_by(|a, b| power(samples, COLUMNS[*a]).total_cmp(&power(samples, COLUMNS[*b])))
            .unwrap();
        let row_peak = power(samples, ROWS[row]);
        let column_peak = power(samples, COLUMNS[column]);
        if ROWS
            .iter()
            .enumerate()
            .all(|(i, hz)| i == row || row_peak > power(samples, *hz) * 2.5)
            && COLUMNS
                .iter()
                .enumerate()
                .all(|(i, hz)| i == column || column_peak > power(samples, *hz) * 2.5)
        {
            self.pairs.push((row, column));
        }
    }
    pub fn done(&self, interval: u64) -> bool {
        self.acknowledged == EXPECTED_ACKS
            && self
                .last_ack
                .is_some_and(|at| interval > at + AFTER_STOP_INTERVALS)
    }
    pub fn progress(&self) -> String {
        format!(
            "queued={}, acknowledged={}, completed={:?}, last_ack={:?}, restored_speech={}",
            self.queued,
            self.acknowledged,
            self.completed,
            self.last_ack,
            self.restored_speech_frames
        )
    }
    pub fn verify(&self, destination: usize) {
        assert_eq!(self.acknowledged, EXPECTED_ACKS);
        assert!(
            self.restored_speech_frames >= REQUIRED_SPEECH_FRAMES,
            "destination {destination}: no speech restoration after continuous stop"
        );
        let expected = if destination == CALLEE {
            [(0, 0), (0, 1), (3, 1), (1, 1)]
        } else {
            [(2, 1), (2, 2), (3, 2), (2, 0)]
        };
        for pair in expected {
            assert!(
                self.pairs.contains(&pair),
                "destination {destination}: missing {pair:?}, observed {:?}",
                self.pairs
            );
        }
    }
}
fn power(samples: &[f64], hz: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (index, value) in samples.iter().enumerate() {
        let phase = std::f64::consts::TAU * hz * index as f64
            / crate::codec::vselp::PCM_SAMPLE_RATE_HZ as f64;
        re += value * phase.cos();
        im += value * phase.sin();
    }
    re * re + im * im
}
