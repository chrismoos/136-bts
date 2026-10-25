use super::dcch::{self, CrcMode};
use std::{error::Error, fmt};

pub const MESSAGE_BITS: usize = 48;
pub const CODED_BITS: usize = 260;
const INFORMATION_BITS: usize = MESSAGE_BITS + 1;
const CRC_BITS: usize = 16;
const WORD_BITS: usize = INFORMATION_BITS + CRC_BITS;
const MEMORY_BITS: usize = 5;
const STATES: usize = 1 << MEMORY_BITS;
const INPUT_MASK: u8 = 1 << MEMORY_BITS;
const OUTPUT_BITS: usize = 4;
const GENERATORS: [u8; OUTPUT_BITS] = [0o67, 0o71, 0o75, 0o53];
const UNREACHABLE: u16 = u16::MAX;
const ROWS: usize = 26;
const COLUMNS: usize = 10;
const FIRST_ROW: [usize; COLUMNS] = [215, 256, 223, 258, 230, 219, 257, 227, 259, 189];
const COLUMN_THREE_END: [usize; 11] = [216, 217, 218, 220, 221, 222, 224, 225, 226, 228, 229];
const COLUMN_THREE_CHANGE_ROW: usize = 15;
const COLUMN_THREE_START_BASE: usize = 74;
const COLUMN_FOUR_BASE: usize = 230;
const COLUMN_FIVE_BASE: usize = 88;
const COLUMN_SIX_BASE: usize = 113;
const COLUMN_SEVEN_BASE: usize = 138;
const COLUMN_EIGHT_BASE: usize = 163;
const COLUMN_NINE_BASE: usize = 189;
const EARLY_COLUMN_STEP: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordHalf {
    Start,
    End,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacchWord {
    pub continuation: bool,
    pub message: [bool; MESSAGE_BITS],
    pub corrected_coded_bits: u16,
    pub observed_coded_bits: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacchError {
    InsufficientObservations,
    Ambiguous,
    Crc,
}
impl fmt::Display for FacchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FACCH decode failed: {self:?}")
    }
}
impl Error for FacchError {}

fn transition(state: u8, input: bool) -> (u8, [bool; OUTPUT_BITS]) {
    let register = state | if input { INPUT_MASK } else { 0 };
    let output = GENERATORS.map(|generator| (register & generator).count_ones() & 1 != 0);
    (register >> 1, output)
}

// IS-54-B §2.7.3.1.1.4 starts at C[5], wraps through C[0..4], and adds no tail bits.
pub fn encode(continuation: bool, message: &[bool; MESSAGE_BITS], dvcc: u8) -> [bool; CODED_BITS] {
    let mut word = [false; WORD_BITS];
    word[0] = continuation;
    word[1..INFORMATION_BITS].copy_from_slice(message);
    let checksum = dcch::checksum(&word[..INFORMATION_BITS], dvcc, CrcMode::Addressed);
    for (index, bit) in word[INFORMATION_BITS..].iter_mut().enumerate() {
        *bit = checksum & (1 << (CRC_BITS - 1 - index)) != 0;
    }
    let mut state = word[..MEMORY_BITS]
        .iter()
        .enumerate()
        .fold(0u8, |state, (bit, value)| state | (u8::from(*value) << bit));
    let mut coded = [false; CODED_BITS];
    for step in 0..WORD_BITS {
        let (next, output) = transition(state, word[(step + MEMORY_BITS) % WORD_BITS]);
        coded[step * OUTPUT_BITS..(step + 1) * OUTPUT_BITS].copy_from_slice(&output);
        state = next;
    }
    coded
}

pub fn decode(coded: &[bool; CODED_BITS], dvcc: u8) -> Result<FacchWord, FacchError> {
    decode_erased(&coded.map(Some), dvcc)
}

/// Missing coded bits contribute no branch metric. Ambiguous maximum-likelihood words are rejected.
pub fn decode_erased(
    coded: &[Option<bool>; CODED_BITS],
    dvcc: u8,
) -> Result<FacchWord, FacchError> {
    let observed_coded_bits = coded.iter().filter(|bit| bit.is_some()).count();
    if observed_coded_bits < WORD_BITS {
        return Err(FacchError::InsufficientObservations);
    }
    let mut best_metric = UNREACHABLE;
    let mut best_word = [false; WORD_BITS];
    let mut ambiguous = false;
    for start in 0..STATES {
        let mut metrics = [UNREACHABLE; STATES];
        metrics[start] = 0;
        let mut ties = [false; STATES];
        let mut parents = [[0u8; STATES]; WORD_BITS];
        for (step, quadruple) in coded.as_chunks::<OUTPUT_BITS>().0.iter().enumerate() {
            let mut next_metrics = [UNREACHABLE; STATES];
            let mut next_ties = [false; STATES];
            for (state, metric) in metrics.iter().copied().enumerate() {
                if metric == UNREACHABLE {
                    continue;
                }
                for input in [false, true] {
                    let (next, output) = transition(state as u8, input);
                    let next = usize::from(next);
                    let distance = output
                        .into_iter()
                        .zip(quadruple)
                        .filter(|(actual, expected)| {
                            expected.is_some_and(|expected| expected != *actual)
                        })
                        .count() as u16;
                    let candidate = metric + distance;
                    if candidate < next_metrics[next] {
                        next_metrics[next] = candidate;
                        next_ties[next] = ties[state];
                        parents[step][next] = state as u8;
                    } else if candidate == next_metrics[next] {
                        next_ties[next] = true;
                    }
                }
            }
            metrics = next_metrics;
            ties = next_ties;
        }
        if metrics[start] > best_metric {
            continue;
        }
        if metrics[start] == best_metric {
            ambiguous = true;
            continue;
        }
        best_metric = metrics[start];
        ambiguous = ties[start];
        let mut state = start;
        let mut shifted = [false; WORD_BITS];
        for step in (0..WORD_BITS).rev() {
            shifted[step] = state & (1 << (MEMORY_BITS - 1)) != 0;
            state = usize::from(parents[step][state]);
        }
        for (index, bit) in best_word.iter_mut().enumerate() {
            *bit = shifted[(index + WORD_BITS - MEMORY_BITS) % WORD_BITS];
        }
    }
    if ambiguous {
        return Err(FacchError::Ambiguous);
    }
    let received = best_word[INFORMATION_BITS..]
        .iter()
        .fold(0u16, |crc, bit| crc * 2 + u16::from(*bit));
    if received != dcch::checksum(&best_word[..INFORMATION_BITS], dvcc, CrcMode::Addressed) {
        return Err(FacchError::Crc);
    }
    Ok(FacchWord {
        continuation: best_word[0],
        message: best_word[1..INFORMATION_BITS]
            .try_into()
            .expect("FACCH message has fixed length"),
        corrected_coded_bits: best_metric,
        observed_coded_bits,
    })
}

fn index(row: usize, column: usize) -> usize {
    if row == 0 {
        return FIRST_ROW[column];
    }
    match column {
        0..=2 => row - 1 + column * EARLY_COLUMN_STEP,
        3 if row < COLUMN_THREE_CHANGE_ROW => row + COLUMN_THREE_START_BASE,
        3 => COLUMN_THREE_END[row - COLUMN_THREE_CHANGE_ROW],
        4 => row + COLUMN_FOUR_BASE,
        5 => row + COLUMN_FIVE_BASE,
        6 => row + COLUMN_SIX_BASE,
        7 => row + COLUMN_SEVEN_BASE,
        8 => row + COLUMN_EIGHT_BASE,
        9 => row + COLUMN_NINE_BASE,
        _ => unreachable!("FACCH array has ten columns"),
    }
}

// IS-54-B Table 2.7.3.1.1.2-1. Even rows finish the previous word, odd rows start the current word.
pub fn interleave(
    previous: &[bool; CODED_BITS],
    current: &[bool; CODED_BITS],
) -> [bool; CODED_BITS] {
    std::array::from_fn(|position| {
        let row = position / COLUMNS;
        let source = if row.is_multiple_of(2) {
            previous
        } else {
            current
        };
        source[index(row, position % COLUMNS)]
    })
}
pub fn observe_half(slot: &[bool; CODED_BITS], half: WordHalf) -> [Option<bool>; CODED_BITS] {
    let mut coded = [None; CODED_BITS];
    let parity = match half {
        WordHalf::Start => 1,
        WordHalf::End => 0,
    };
    for row in (parity..ROWS).step_by(2) {
        for column in 0..COLUMNS {
            coded[index(row, column)] = Some(slot[row * COLUMNS + column]);
        }
    }
    coded
}
pub fn deinterleave(first: &[bool; CODED_BITS], second: &[bool; CODED_BITS]) -> [bool; CODED_BITS] {
    let start = observe_half(first, WordHalf::Start);
    let end = observe_half(second, WordHalf::End);
    std::array::from_fn(|index| {
        start[index]
            .or(end[index])
            .expect("two halves cover every coded bit")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interleaver_is_the_standard_permutation_and_spans_two_slots() {
        let mut permutation: Vec<_> = (0..ROWS)
            .flat_map(|row| (0..COLUMNS).map(move |column| index(row, column)))
            .collect();
        let first = permutation[..COLUMNS].to_vec();
        assert_eq!(first, [215, 256, 223, 258, 230, 219, 257, 227, 259, 189]);
        assert_eq!(
            (0..COLUMNS)
                .map(|column| index(25, column))
                .collect::<Vec<_>>(),
            [24, 49, 74, 229, 255, 113, 138, 163, 188, 214]
        );
        permutation.sort();
        assert_eq!(permutation, (0..CODED_BITS).collect::<Vec<_>>());
        let word = std::array::from_fn(|index| index.is_multiple_of(3));
        let other = std::array::from_fn(|index| index.is_multiple_of(7));
        let first = interleave(&other, &word);
        let second = interleave(&word, &other);
        assert_eq!(deinterleave(&first, &second), word);
    }
    #[test]
    fn tail_biting_code_recovers_full_and_erased_half_words() {
        let message = std::array::from_fn(|index| index.is_multiple_of(3));
        let coded = encode(true, &message, 10);
        let word = decode(&coded, 10).unwrap();
        assert_eq!(word.message, message);
        assert!(word.continuation);
        assert_eq!(word.corrected_coded_bits, 0);
        assert_eq!(decode(&coded, 11), Err(FacchError::Crc));
        let slot = interleave(&coded, &coded);
        for half in [WordHalf::Start, WordHalf::End] {
            let word = decode_erased(&observe_half(&slot, half), 10).unwrap();
            assert_eq!(word.message, message);
            assert!(word.continuation);
            assert_eq!(word.observed_coded_bits, CODED_BITS / 2);
        }
        let mut corrupt = coded;
        corrupt[0] ^= true;
        corrupt[87] ^= true;
        assert_eq!(decode(&corrupt, 10).unwrap().message, message);
        assert_eq!(
            decode_erased(&[None; CODED_BITS], 10),
            Err(FacchError::InsufficientObservations)
        );
        let limited = std::array::from_fn(|index| (index < WORD_BITS).then_some(false));
        assert_eq!(decode_erased(&limited, 0), Err(FacchError::Ambiguous));
    }
    #[test]
    fn generator_equations_and_circular_bit_order_match_impulse() {
        let mut word = [false; WORD_BITS];
        word[0] = true;
        let mut state = 1;
        let mut output = Vec::new();
        for step in 0..WORD_BITS {
            let (next, quad) = transition(state, word[(step + MEMORY_BITS) % WORD_BITS]);
            output.extend(quad);
            state = next;
        }
        assert_eq!(state, 1);
        assert_eq!(&output[..OUTPUT_BITS], &[true, true, true, true]);
        assert!(
            output[OUTPUT_BITS..60 * OUTPUT_BITS]
                .iter()
                .all(|bit| !*bit)
        );
        assert_eq!(
            &output[60 * OUTPUT_BITS..],
            &[
                true, true, true, true, true, true, true, false, false, true, true, true, true,
                false, true, false, true, false, false, true
            ]
        );
    }
    #[test]
    fn codewords_match_polynomial_and_circular_equation_vectors() {
        let message = std::array::from_fn(|index| index.is_multiple_of(3));
        for (continuation, expected) in [
            (
                false,
                "78578578578578578578578578578578578578578578a9019dc88a9019dc874ea",
            ),
            (
                true,
                "88578578578578578578578578578578578578578578578a6184ea89cabc9e9dc",
            ),
        ] {
            let coded = encode(continuation, &message, 10);
            let hex: String = coded
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
            let decoded = decode(&coded, 10).unwrap();
            assert_eq!(decoded.message, message);
            assert_eq!(decoded.continuation, continuation);
        }
    }
}
