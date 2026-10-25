use super::{
    dcch::{self, CrcMode},
    traffic::SACCH_BITS,
};
use std::{error::Error, fmt};

pub const MESSAGE_BITS: usize = 48;
pub const WORD_BITS: usize = MESSAGE_BITS + 2 + 16;
pub const CODED_WORD_BITS: usize = WORD_BITS * 2;
pub const WORD_SLOTS: usize = WORD_BITS / DATA_BITS_PER_SLOT;
const DATA_BITS_PER_SLOT: usize = SACCH_BITS / 2;
const HEADER_BITS: usize = 2;
const CRC_BITS: usize = 16;
const INFORMATION_BITS: usize = HEADER_BITS + MESSAGE_BITS;
const MEMORY_BITS: usize = 5;
const STATES: usize = 1 << MEMORY_BITS;
const UNREACHABLE: u16 = u16::MAX;
const OUTPUT_BITS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SacchWord {
    pub continuation: bool,
    pub message: [bool; MESSAGE_BITS],
    pub corrected_coded_bits: u16,
    pub final_encoder_state: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SacchError {
    Ambiguous,
    Reserved,
    Crc,
}
impl fmt::Display for SacchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SACCH decode failed: {self:?}")
    }
}
impl Error for SacchError {}

#[derive(Clone, Default)]
pub struct SacchEncoder {
    state: u8,
    history: [[bool; SACCH_BITS]; SACCH_BITS],
    cursor: usize,
}
impl SacchEncoder {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    // IS-54-B §2.7.3.1.2 uses continuous convolutional coding without tail bits.
    pub fn encode_word(
        &mut self,
        continuation: bool,
        message: &[bool; MESSAGE_BITS],
        dvcc: u8,
    ) -> [[bool; SACCH_BITS]; WORD_SLOTS] {
        let mut word = [false; WORD_BITS];
        word[0] = continuation;
        word[HEADER_BITS..INFORMATION_BITS].copy_from_slice(message);
        let crc = dcch::checksum(&word[..INFORMATION_BITS], dvcc, CrcMode::Addressed);
        for (index, bit) in word[INFORMATION_BITS..].iter_mut().enumerate() {
            *bit = crc & (1 << (CRC_BITS - 1 - index)) != 0;
        }
        std::array::from_fn(|slot| {
            self.encode_bits(
                word[slot * DATA_BITS_PER_SLOT..(slot + 1) * DATA_BITS_PER_SLOT]
                    .try_into()
                    .expect("six SACCH input bits per slot"),
            )
        })
    }

    pub fn encode_bits(&mut self, bits: [bool; DATA_BITS_PER_SLOT]) -> [bool; SACCH_BITS] {
        let mut coded = [false; SACCH_BITS];
        for (index, bit) in bits.into_iter().enumerate() {
            let (next, pair) = dcch::transition(self.state, bit);
            coded[index * OUTPUT_BITS..(index + 1) * OUTPUT_BITS].copy_from_slice(&pair);
            self.state = next;
        }
        self.history[self.cursor] = coded;
        let output = std::array::from_fn(|bit| {
            self.history[(self.cursor + SACCH_BITS - bit) % SACCH_BITS][bit]
        });
        self.cursor = (self.cursor + 1) % SACCH_BITS;
        output
    }
}

#[derive(Default)]
pub struct SacchDeinterleaver {
    history: [[bool; SACCH_BITS]; SACCH_BITS],
    cursor: usize,
    received: usize,
}
impl SacchDeinterleaver {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    // IS-54-B §2.7.3.1.2.1 sends coded bit j in slot m+j.
    pub fn push(&mut self, slot: [bool; SACCH_BITS]) -> Option<[bool; SACCH_BITS]> {
        self.history[self.cursor] = slot;
        self.received = self.received.saturating_add(1);
        let output = (self.received >= SACCH_BITS).then(|| {
            std::array::from_fn(|bit| self.history[(self.cursor + 1 + bit) % SACCH_BITS][bit])
        });
        self.cursor = (self.cursor + 1) % SACCH_BITS;
        output
    }
}

/// Decode a complete word with unknown starting memory and no forced ending state.
pub fn decode(coded: &[bool; CODED_WORD_BITS], dvcc: u8) -> Result<SacchWord, SacchError> {
    let mut metrics = [0u16; STATES];
    let mut ties = [false; STATES];
    let mut paths = [0u128; STATES];
    for pair in coded.as_chunks::<OUTPUT_BITS>().0 {
        let mut next_metrics = [UNREACHABLE; STATES];
        let mut next_ties = [false; STATES];
        let mut next_paths = [0u128; STATES];
        for (state, metric) in metrics.iter().copied().enumerate() {
            for input in [false, true] {
                let (next, output) = dcch::transition(state as u8, input);
                let next = usize::from(next);
                let candidate =
                    metric + output.iter().zip(pair).filter(|(a, b)| a != b).count() as u16;
                let path = (paths[state] << 1) | u128::from(input);
                if candidate < next_metrics[next] {
                    next_metrics[next] = candidate;
                    next_ties[next] = ties[state];
                    next_paths[next] = path;
                } else if candidate == next_metrics[next] {
                    next_ties[next] |= ties[state] || next_paths[next] != path;
                }
            }
        }
        metrics = next_metrics;
        ties = next_ties;
        paths = next_paths;
    }
    let best = *metrics.iter().min().expect("32 ending states");
    let mut ending = None;
    for (state, metric) in metrics.iter().enumerate() {
        if *metric != best {
            continue;
        }
        if ending.is_some() {
            return Err(SacchError::Ambiguous);
        }
        ending = Some(state);
    }
    let ending = ending.expect("one best ending state");
    if ties[ending] {
        return Err(SacchError::Ambiguous);
    }
    let final_encoder_state = ending as u8;
    let word: [bool; WORD_BITS] =
        std::array::from_fn(|index| paths[ending] & (1 << (WORD_BITS - 1 - index)) != 0);
    if word[1] {
        return Err(SacchError::Reserved);
    }
    let received = word[INFORMATION_BITS..]
        .iter()
        .fold(0u16, |crc, bit| crc * 2 + u16::from(*bit));
    if received != dcch::checksum(&word[..INFORMATION_BITS], dvcc, CrcMode::Addressed) {
        return Err(SacchError::Crc);
    }
    Ok(SacchWord {
        continuation: word[0],
        message: word[HEADER_BITS..INFORMATION_BITS]
            .try_into()
            .expect("SACCH message has fixed length"),
        corrected_coded_bits: best,
        final_encoder_state,
    })
}

#[derive(Debug)]
pub enum ReceiveOutcome {
    Pending,
    Word(SacchWord),
    Rejected(SacchError),
}

#[derive(Default)]
pub struct SacchReceiver {
    deinterleaver: SacchDeinterleaver,
    rows: [[bool; SACCH_BITS]; WORD_SLOTS],
    cursor: usize,
    available: usize,
    phase: usize,
    locked_phase: Option<usize>,
}
impl SacchReceiver {
    pub fn discontinuity(&mut self) {
        *self = Self::default();
    }

    /// Supply consecutive SACCH fields for one assigned traffic channel. Reset after gaps or handoff.
    pub fn push(&mut self, slot: [bool; SACCH_BITS], dvcc: u8) -> Option<SacchWord> {
        match self.push_outcome(slot, dvcc) {
            ReceiveOutcome::Word(word) => Some(word),
            ReceiveOutcome::Pending | ReceiveOutcome::Rejected(_) => None,
        }
    }
    pub fn push_outcome(&mut self, slot: [bool; SACCH_BITS], dvcc: u8) -> ReceiveOutcome {
        let Some(row) = self.deinterleaver.push(slot) else {
            return ReceiveOutcome::Pending;
        };
        self.rows[self.cursor] = row;
        self.cursor = (self.cursor + 1) % WORD_SLOTS;
        self.available = (self.available + 1).min(WORD_SLOTS);
        self.phase = (self.phase + 1) % WORD_SLOTS;
        if self.available < WORD_SLOTS || self.locked_phase.is_some_and(|phase| phase != self.phase)
        {
            return ReceiveOutcome::Pending;
        }
        let coded = std::array::from_fn(|index| {
            self.rows[(self.cursor + index / SACCH_BITS) % WORD_SLOTS][index % SACCH_BITS]
        });
        match decode(&coded, dvcc) {
            Ok(word) => {
                self.locked_phase = Some(self.phase);
                ReceiveOutcome::Word(word)
            }
            Err(error) => {
                self.locked_phase = None;
                ReceiveOutcome::Rejected(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagonal_delay_matches_transmission_equation() {
        let mut encoder = SacchEncoder::default();
        let mut deinterleaver = SacchDeinterleaver::default();
        let mut source = Vec::new();
        let mut state = 0;
        for slot in 0..40 {
            let bits =
                std::array::from_fn(|bit| (slot * DATA_BITS_PER_SLOT + bit).is_multiple_of(7));
            let mut coded = [false; SACCH_BITS];
            for (index, input) in bits.into_iter().enumerate() {
                let (next, pair) = dcch::transition(state, input);
                state = next;
                coded[index * 2..index * 2 + 2].copy_from_slice(&pair);
            }
            source.push(coded);
            let transmitted = encoder.encode_bits(bits);
            for bit in 0..SACCH_BITS {
                let expected = if slot >= bit {
                    source[slot - bit][bit]
                } else {
                    false
                };
                assert_eq!(transmitted[bit], expected);
            }
            let recovered = deinterleaver.push(transmitted);
            if slot >= SACCH_BITS - 1 {
                assert_eq!(recovered, Some(source[slot - (SACCH_BITS - 1)]));
            } else {
                assert_eq!(recovered, None);
            }
        }
    }
    #[test]
    fn continuous_words_recover_without_tail_bits_and_with_unknown_initial_memory() {
        let a = std::array::from_fn(|index| index.is_multiple_of(3));
        let b = std::array::from_fn(|index| index.is_multiple_of(7));
        let mut encoder = SacchEncoder::default();
        let mut deinterleaver = SacchDeinterleaver::default();
        let stream: Vec<_> = encoder
            .encode_word(false, &a, 10)
            .into_iter()
            .chain(encoder.encode_word(true, &b, 10))
            .chain(encoder.encode_word(false, &[false; MESSAGE_BITS], 10))
            .collect();
        let coded: Vec<_> = stream
            .into_iter()
            .filter_map(|slot| deinterleaver.push(slot))
            .flatten()
            .collect();
        for (index, (message, continuation)) in [(a, false), (b, true)].into_iter().enumerate() {
            let block = coded[index * CODED_WORD_BITS..(index + 1) * CODED_WORD_BITS]
                .try_into()
                .unwrap();
            let word = decode(block, 10).unwrap();
            assert_eq!(word.message, message);
            assert_eq!(word.continuation, continuation);
            assert_eq!(word.corrected_coded_bits, 0);
            assert_eq!(decode(block, 11), Err(SacchError::Crc));
        }
        encoder.reset();
        deinterleaver.reset();
        assert!(
            deinterleaver
                .push(encoder.encode_bits([false; DATA_BITS_PER_SLOT]))
                .is_none()
        );
    }
    #[test]
    fn continuous_code_matches_independent_lag_equation_vectors() {
        let message = std::array::from_fn(|index| index.is_multiple_of(3));
        for (prefix, expected) in [
            (false, "e93e38e38e38e38e38e38e38ee6a52e83"),
            (true, "823e38e38e38e38e38e38e38ee6a52e83"),
        ] {
            let mut encoder = SacchEncoder::default();
            let mut deinterleaver = SacchDeinterleaver::default();
            if prefix {
                assert!(
                    deinterleaver
                        .push(encoder.encode_bits([false, true, false, true, true, false]))
                        .is_none()
                );
            }
            let stream: Vec<_> = encoder
                .encode_word(true, &message, 10)
                .into_iter()
                .chain(encoder.encode_word(false, &[false; MESSAGE_BITS], 10))
                .collect();
            let coded: Vec<_> = stream
                .into_iter()
                .filter_map(|slot| deinterleaver.push(slot))
                .flatten()
                .collect();
            let start = if prefix { SACCH_BITS } else { 0 };
            let block: [bool; CODED_WORD_BITS] =
                coded[start..start + CODED_WORD_BITS].try_into().unwrap();
            let hex: String = block
                .as_chunks::<4>()
                .0
                .iter()
                .map(|nibble| {
                    char::from_digit(
                        nibble
                            .iter()
                            .fold(0, |value, bit| value * 2 + u32::from(*bit)),
                        16,
                    )
                    .unwrap()
                })
                .collect();
            assert_eq!(hex, expected);
            let word = decode(&block, 10).unwrap();
            assert_eq!(word.message, message);
            assert!(word.continuation);
            let mut bad = block;
            bad[0] ^= true;
            let corrected = decode(&bad, 10).unwrap();
            assert_eq!(corrected.message, message);
            assert_eq!(corrected.corrected_coded_bits, 1);
        }
    }
    #[test]
    fn reserved_header_is_rejected_even_with_matching_crc() {
        let mut word = [false; WORD_BITS];
        word[1] = true;
        let crc = dcch::checksum(&word[..INFORMATION_BITS], 10, CrcMode::Addressed);
        for (index, bit) in word[INFORMATION_BITS..].iter_mut().enumerate() {
            *bit = crc & (1 << (CRC_BITS - 1 - index)) != 0;
        }
        let mut state = 0;
        let mut coded = [false; CODED_WORD_BITS];
        for (index, input) in word.into_iter().enumerate() {
            let (next, pair) = dcch::transition(state, input);
            state = next;
            coded[index * OUTPUT_BITS..(index + 1) * OUTPUT_BITS].copy_from_slice(&pair);
        }
        assert_eq!(decode(&coded, 10), Err(SacchError::Reserved));
    }
}
