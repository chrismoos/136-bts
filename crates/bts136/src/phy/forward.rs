use super::{
    modulation::{Dibit, DifferentialEncoder, Symbol},
    pulse::{InvalidPulseRate, RootRaisedCosine, SYMBOL_RATE_HZ},
    traffic::SLOT_BITS,
};

use num_complex::Complex32;
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLength {
    pub expected: usize,
    pub actual: usize,
}
impl fmt::Display for OutputLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "forward IQ buffer requires {} samples, received {}",
            self.expected, self.actual
        )
    }
}
impl Error for OutputLength {}

const MAX_SAMPLES_PER_SYMBOL: usize = 32;
const FILTER_HALF_SPAN_SYMBOLS: usize = 8;

pub struct ForwardModulator {
    encoder: DifferentialEncoder,
    taps: Vec<f64>,
    history: Vec<Symbol>,
    cursor: usize,
    samples_per_symbol: usize,
}
impl ForwardModulator {
    pub fn try_new(samples_per_symbol: usize) -> Result<Self, InvalidPulseRate> {
        if !(2..=MAX_SAMPLES_PER_SYMBOL).contains(&samples_per_symbol) {
            return Err(InvalidPulseRate);
        }
        let filter = RootRaisedCosine::try_new(
            SYMBOL_RATE_HZ * samples_per_symbol as f64,
            FILTER_HALF_SPAN_SYMBOLS,
        )?;
        let history =
            vec![Symbol { i: 0.0, q: 0.0 }; filter.taps().len().div_ceil(samples_per_symbol)];
        Ok(Self {
            encoder: DifferentialEncoder::default(),
            taps: filter.taps().to_vec(),
            history,
            cursor: 0,
            samples_per_symbol,
        })
    }
    pub fn sample_rate_hz(&self) -> f64 {
        SYMBOL_RATE_HZ * self.samples_per_symbol as f64
    }
    pub fn delay_samples(&self) -> usize {
        self.taps.len() / 2
    }

    pub fn slot_samples(&self) -> usize {
        SLOT_BITS / 2 * self.samples_per_symbol
    }
    pub fn tail_samples(&self) -> usize {
        (self.history.len() - 1) * self.samples_per_symbol
    }
    pub fn encode_slot_into(
        &mut self,
        bits: &[bool; SLOT_BITS],
        output: &mut [Complex32],
    ) -> Result<(), OutputLength> {
        self.check_output(output.len(), self.slot_samples())?;
        for (dibit, samples) in bits
            .as_chunks::<2>()
            .0
            .iter()
            .zip(output.chunks_exact_mut(self.samples_per_symbol))
        {
            let symbol = self.encoder.encode(Dibit::from_bits(*dibit));
            let mut samples = samples.iter_mut();
            self.push_symbol(symbol, |sample| {
                *samples.next().expect("one output per filter phase") =
                    Complex32::new(sample.i as f32, sample.q as f32);
            });
        }
        Ok(())
    }
    pub(crate) fn skip_zero_slots_into(
        &mut self,
        slots: u64,
        output: &mut [Complex32],
    ) -> Result<(), OutputLength> {
        const ZERO_SLOT_PHASE_PERIOD: u64 = 4;
        if slots == 0 {
            return Ok(());
        }
        // Each zero slot advances phase by pi/2 and replaces the complete FIR history.
        for _ in 0..1 + (slots - 1) % ZERO_SLOT_PHASE_PERIOD {
            self.encode_slot_into(&[false; SLOT_BITS], output)?;
        }
        Ok(())
    }
    pub fn finish_into(&mut self, output: &mut [Complex32]) -> Result<(), OutputLength> {
        self.check_output(output.len(), self.tail_samples())?;
        for samples in output.chunks_exact_mut(self.samples_per_symbol) {
            let mut samples = samples.iter_mut();
            self.push_symbol(Symbol { i: 0.0, q: 0.0 }, |sample| {
                *samples.next().expect("one output per filter phase") =
                    Complex32::new(sample.i as f32, sample.q as f32);
            });
        }
        Ok(())
    }
    fn check_output(&self, actual: usize, expected: usize) -> Result<(), OutputLength> {
        if actual != expected {
            return Err(OutputLength { expected, actual });
        }
        Ok(())
    }

    pub fn encode_slot(&mut self, bits: &[bool; SLOT_BITS]) -> Vec<Symbol> {
        let mut output = Vec::with_capacity(SLOT_BITS / 2 * self.samples_per_symbol);
        for dibit in bits.as_chunks::<2>().0 {
            let symbol = self.encoder.encode(Dibit::from_bits(*dibit));
            self.push_symbol(symbol, |sample| output.push(sample));
        }
        output
    }

    pub fn finish(&mut self) -> Vec<Symbol> {
        let mut output = Vec::with_capacity((self.history.len() - 1) * self.samples_per_symbol);
        for _ in 1..self.history.len() {
            self.push_symbol(Symbol { i: 0.0, q: 0.0 }, |sample| output.push(sample));
        }
        output
    }

    fn push_symbol(&mut self, symbol: Symbol, mut emit: impl FnMut(Symbol)) {
        self.history[self.cursor] = symbol;
        for phase in 0..self.samples_per_symbol {
            let mut sample = Symbol { i: 0.0, q: 0.0 };
            for (delay, tap) in self
                .taps
                .iter()
                .skip(phase)
                .step_by(self.samples_per_symbol)
                .enumerate()
            {
                let symbol =
                    self.history[(self.cursor + self.history.len() - delay) % self.history.len()];
                sample.i += symbol.i * tap;
                sample.q += symbol.q * tap;
            }
            emit(sample);
        }
        self.cursor = (self.cursor + 1) % self.history.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phy::{
        control::{self, INFORMATION_BITS, LogicalChannel, SharedFeedback},
        modulation,
        sync::SyncWord,
    };

    #[test]
    fn reusable_iq_buffers_preserve_phase_filter_history_and_tail() {
        for oversampling in [2, 8, MAX_SAMPLES_PER_SYMBOL] {
            let mut reference = ForwardModulator::try_new(oversampling).unwrap();
            let mut stream = ForwardModulator::try_new(oversampling).unwrap();
            let mut buffer = vec![Complex32::new(0.0, 0.0); stream.slot_samples()];
            for slot in 0..7 {
                let bits = std::array::from_fn(|index| (index + slot) % 3 == 0);
                let expected = reference.encode_slot(&bits);
                stream.encode_slot_into(&bits, &mut buffer).unwrap();
                for (actual, expected) in buffer.iter().zip(expected) {
                    assert_eq!(
                        *actual,
                        Complex32::new(expected.i as f32, expected.q as f32)
                    );
                }
            }
            let expected = reference.finish();
            buffer.resize(stream.tail_samples(), Complex32::new(0.0, 0.0));
            stream.finish_into(&mut buffer).unwrap();
            assert_eq!(
                buffer,
                expected
                    .into_iter()
                    .map(|sample| Complex32::new(sample.i as f32, sample.q as f32))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn rejected_output_does_not_change_phase_history_or_buffer() {
        let mut stream = ForwardModulator::try_new(8).unwrap();
        let mut reference = ForwardModulator::try_new(8).unwrap();
        let bits = [true; SLOT_BITS];
        let sentinel = Complex32::new(7.0, -9.0);
        let mut wrong = vec![sentinel; stream.slot_samples() - 1];
        assert_eq!(
            stream.encode_slot_into(&bits, &mut wrong),
            Err(OutputLength {
                expected: stream.slot_samples(),
                actual: wrong.len()
            })
        );
        assert!(wrong.iter().all(|sample| *sample == sentinel));
        assert!(stream.finish_into(&mut wrong).is_err());
        let mut actual = vec![sentinel; stream.slot_samples()];
        stream.encode_slot_into(&bits, &mut actual).unwrap();
        let expected = reference.encode_slot(&bits);
        assert_eq!(
            actual,
            expected
                .into_iter()
                .map(|sample| Complex32::new(sample.i as f32, sample.q as f32))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn filtered_forward_control_recovers_at_the_matched_filter_boundary() {
        let information = std::array::from_fn::<_, INFORMATION_BITS, _>(|i| i % 3 == 0);
        let bits = control::encode_forward(
            SyncWord::One,
            7,
            &SharedFeedback {
                leading: [false; 12],
                trailing: [false; 10],
            },
            &information,
            LogicalChannel::Fbcch,
            0,
        )
        .unwrap();
        let mut transmitter = ForwardModulator::try_new(8).unwrap();
        let mut waveform = transmitter.encode_slot(&bits);
        waveform.extend(transmitter.encode_slot(&bits));
        waveform.extend(transmitter.finish());
        let receive_filter =
            RootRaisedCosine::try_new(transmitter.sample_rate_hz(), FILTER_HALF_SPAN_SYMBOLS)
                .unwrap();
        let filtered = receive_filter.filter(&waveform);
        let delay = transmitter.delay_samples() + receive_filter.delay_samples();
        let start = delay + SLOT_BITS / 2 * 8;
        let mut decoded = [false; SLOT_BITS];
        for (index, dibit) in decoded.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let position = start + index * 8;
            let previous = filtered[position - 8];
            let current = filtered[position];
            *dibit = modulation::decode(previous, current).unwrap().bits();
        }
        assert_eq!(decoded, bits);
        assert_eq!(
            control::decode_forward(&decoded, LogicalChannel::Fbcch, 0)
                .unwrap()
                .block
                .information,
            information
        );
    }
}

#[cfg(test)]
mod skipped_slot_tests {
    use super::*;
    #[test]
    fn bulk_zero_slot_skip_preserves_next_iq_after_different_phase_cycles() {
        for oversampling in [2, 16, 32] {
            for slots in [0, 1, 2, 3, 4, 5, 8, 9, 47, 400] {
                let mut reference = ForwardModulator::try_new(oversampling).unwrap();
                let mut fast = ForwardModulator::try_new(oversampling).unwrap();
                let bits = std::array::from_fn(|index| index % 3 == 0);
                let mut expected = vec![Complex32::default(); reference.slot_samples()];
                let mut actual = expected.clone();
                reference.encode_slot_into(&bits, &mut expected).unwrap();
                fast.encode_slot_into(&bits, &mut actual).unwrap();
                for _ in 0..slots {
                    reference
                        .encode_slot_into(&[false; SLOT_BITS], &mut expected)
                        .unwrap();
                }
                fast.skip_zero_slots_into(slots, &mut actual).unwrap();
                reference.encode_slot_into(&bits, &mut expected).unwrap();
                fast.encode_slot_into(&bits, &mut actual).unwrap();
                assert_eq!(
                    actual, expected,
                    "oversampling={oversampling} slots={slots}"
                );
            }
        }
    }
}
