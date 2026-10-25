use crate::{
    codec::vselp::{
        FRAME_SAMPLES, PCM_SAMPLE_RATE_HZ, VselpParameters,
        encoder::{EncodeError, Encoder},
    },
    l3::legacy_traffic::{DtmfDigit, DtmfMessage},
};
use std::{collections::VecDeque, f64::consts::TAU};

const MAX_PENDING_DIGITS: usize = 252;
const BURST_TONE_FRAMES: usize = 5;
const BURST_GAP_FRAMES: usize = 5;
const FLUSH_FRAMES: usize = 2;
const TONE_AMPLITUDE: f64 = 0.2;
const ROW_HZ: [f64; 4] = [697.0, 770.0, 852.0, 941.0];
const COLUMN_HZ: [f64; 3] = [1209.0, 1336.0, 1477.0];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdmissionError {
    NotRequest,
    ChangedRetransmission,
    Capacity,
}
#[derive(Default)]
pub(super) struct Sender {
    last_burst: Option<DtmfMessage>,
    last_continuous: Option<DtmfMessage>,
    burst: VecDeque<DtmfDigit>,
    burst_frame: usize,
    continuous: Option<DtmfDigit>,
    phases: [f64; 2],
    preceding_digit: Option<DtmfDigit>,
    encoder: Option<Encoder>,
    flush: usize,
    last_interval: Option<u64>,
    cached: Option<VselpParameters>,
    cached_pcm: Option<[f64; FRAME_SAMPLES]>,
}
impl Sender {
    // TIA-136-133-E §5.1.2.3.108 keeps retransmission numbers independent per message type.
    pub fn is_new(&self, request: &DtmfMessage) -> Result<bool, AdmissionError> {
        let last = match request {
            DtmfMessage::Burst { .. } => &self.last_burst,
            DtmfMessage::Continuous { .. } => &self.last_continuous,
            _ => return Err(AdmissionError::NotRequest),
        };
        if let Some(last) = last
            && last.request_number() == request.request_number()
        {
            return if last == request {
                Ok(false)
            } else {
                Err(AdmissionError::ChangedRetransmission)
            };
        }
        if let DtmfMessage::Burst {
            digits: Some(digits),
            ..
        } = request
            && self.burst.len()
                + digits
                    .iter()
                    .filter(|digit| **digit != DtmfDigit::Null)
                    .count()
                > MAX_PENDING_DIGITS
        {
            return Err(AdmissionError::Capacity);
        }
        Ok(true)
    }
    /// Call only after is_new admits the request and its acknowledgment is retained.
    pub fn accept(&mut self, request: &DtmfMessage) {
        match request {
            DtmfMessage::Burst { digits, .. } => {
                if let Some(digits) = digits {
                    self.burst.extend(
                        digits
                            .iter()
                            .copied()
                            .filter(|digit| *digit != DtmfDigit::Null),
                    );
                }
                self.last_burst = Some(request.clone());
            }
            DtmfMessage::Continuous { digit, .. } => {
                self.continuous = (*digit != DtmfDigit::Null).then_some(*digit);
                self.last_continuous = Some(request.clone());
            }
            _ => unreachable!("only admitted requests are accepted"),
        }
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn pause(&mut self, interval: u64) {
        self.last_interval = Some(interval);
        self.cached = None;
        self.cached_pcm = None;
    }
    pub fn skip(&mut self, interval: u64) {
        if self.last_interval != Some(interval) {
            self.next_digit();
        }
        self.last_interval = Some(interval);
        self.encoder = None;
        self.cached = None;
        self.cached_pcm = None;
    }
    pub fn frame(
        &mut self,
        interval: u64,
        preceding: VselpParameters,
    ) -> Result<Option<VselpParameters>, EncodeError> {
        if self.last_interval == Some(interval) {
            return Ok(self.cached.clone());
        }
        if self
            .last_interval
            .is_some_and(|last| last.checked_add(1) != Some(interval))
        {
            self.encoder = None;
        }
        self.last_interval = Some(interval);
        let active = !self.burst.is_empty() || self.continuous.is_some();
        if !active && (self.encoder.is_none() || self.flush == 0) {
            self.encoder = None;
            self.cached = None;
            self.cached_pcm = None;
            return Ok(None);
        }
        let digit = self.next_digit();
        if active {
            self.flush = FLUSH_FRAMES;
        } else {
            self.flush -= 1;
        }
        let pcm = self.pcm(digit);
        let encoded = self
            .encoder
            .get_or_insert_with(|| Encoder::new(preceding))
            .push(&pcm)?;
        self.cached = encoded.clone();
        Ok(encoded)
    }
    pub fn frame_pcm(&mut self, interval: u64) -> Option<[f64; FRAME_SAMPLES]> {
        if self.last_interval == Some(interval) {
            return self.cached_pcm;
        }
        self.last_interval = Some(interval);
        let active = !self.burst.is_empty() || self.continuous.is_some();
        if !active && self.flush == 0 {
            self.cached_pcm = None;
            return None;
        }
        let digit = self.next_digit();
        if active {
            self.flush = FLUSH_FRAMES;
        } else {
            self.flush -= 1;
        }
        let pcm = self.pcm(digit);
        self.cached_pcm = Some(pcm);
        Some(pcm)
    }
    fn next_digit(&mut self) -> Option<DtmfDigit> {
        let Some(digit) = self.burst.front().copied() else {
            return self.continuous;
        };
        let output = (self.burst_frame < BURST_TONE_FRAMES).then_some(digit);
        self.burst_frame += 1;
        if self.burst_frame == BURST_TONE_FRAMES + BURST_GAP_FRAMES {
            self.burst.pop_front();
            self.burst_frame = 0;
        }
        output
    }
    fn pcm(&mut self, digit: Option<DtmfDigit>) -> [f64; FRAME_SAMPLES] {
        if digit != self.preceding_digit {
            self.phases = [0.0; 2];
            self.preceding_digit = digit;
        }
        let Some((row, column)) = digit.and_then(frequencies) else {
            return [0.0; FRAME_SAMPLES];
        };
        std::array::from_fn(|_| {
            let output = TONE_AMPLITUDE * (self.phases[0].sin() + self.phases[1].sin());
            self.phases[0] = (self.phases[0] + TAU * row / PCM_SAMPLE_RATE_HZ as f64) % TAU;
            self.phases[1] = (self.phases[1] + TAU * column / PCM_SAMPLE_RATE_HZ as f64) % TAU;
            output
        })
    }
}
fn frequencies(digit: DtmfDigit) -> Option<(f64, f64)> {
    let (row, column) = match digit {
        DtmfDigit::Null => return None,
        DtmfDigit::One => (0, 0),
        DtmfDigit::Two => (0, 1),
        DtmfDigit::Three => (0, 2),
        DtmfDigit::Four => (1, 0),
        DtmfDigit::Five => (1, 1),
        DtmfDigit::Six => (1, 2),
        DtmfDigit::Seven => (2, 0),
        DtmfDigit::Eight => (2, 1),
        DtmfDigit::Nine => (2, 2),
        DtmfDigit::Star => (3, 0),
        DtmfDigit::Zero => (3, 1),
        DtmfDigit::Hash => (3, 2),
    };
    Some((ROW_HZ[row], COLUMN_HZ[column]))
}
#[cfg(test)]
mod tests;
