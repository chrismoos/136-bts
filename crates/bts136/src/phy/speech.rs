use super::dcch;
use std::{error::Error, fmt};

pub const CLASS_ONE_BITS: usize = 77;
pub const CLASS_TWO_BITS: usize = 82;
pub const CHANNEL_BITS: usize = 260;
const CLASS_ONE_OFFSET: usize = 4;
const CRC_BITS: usize = 7;
const TAIL_BITS: usize = 5;
const INPUT_BITS: usize = CLASS_ONE_BITS + CRC_BITS + TAIL_BITS;
const OUTPUT_BITS: usize = 2;
pub const PROTECTED_BITS: usize = INPUT_BITS * OUTPUT_BITS;
const STATES: usize = 1 << TAIL_BITS;
const UNREACHABLE: u16 = u16::MAX;
const CRC_POLYNOMIAL: u8 = 0xb7;
const CRC_MASK: u8 = (1 << CRC_BITS) - 1;
const ROWS: usize = 26;
const COLUMNS: usize = 10;
// IS-54-B §2.1.3.3.3.3, input and parity coefficients in descending polynomial order.
const SIGNIFICANT_POSITIONS: [usize; 12] = [80, 4, 79, 5, 78, 6, 77, 7, 76, 8, 75, 9];
const PARITY_POSITIONS: [usize; CRC_BITS] = [0, 83, 1, 82, 2, 81, 3];
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechBits {
    /// CL1[4..=80] in Table 2.1.3.3.3.4-2 order, excluding CRC and tail bits.
    pub class_one: [bool; CLASS_ONE_BITS],
    /// CL2[0..=81] in Table 2.1.3.3.4-1 order, without error protection.
    pub class_two: [bool; CLASS_TWO_BITS],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechFrame {
    pub bits: SpeechBits,
    pub corrected_coded_bits: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrcStatus {
    Valid,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechCandidate {
    pub frame: SpeechFrame,
    pub crc: CrcStatus,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechError {
    Ambiguous,
    Crc,
}
impl fmt::Display for SpeechError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP channel decode failed: {self:?}")
    }
}
impl Error for SpeechError {}
fn checksum(input: &[bool; INPUT_BITS]) -> u8 {
    let mut remainder = 0u8;
    for position in SIGNIFICANT_POSITIONS {
        let feedback = (remainder >> (CRC_BITS - 1) != 0) ^ input[position];
        remainder = (remainder << 1) & CRC_MASK;
        if feedback {
            remainder ^= CRC_POLYNOMIAL & CRC_MASK;
        }
    }
    remainder & CRC_MASK
}
// IS-54-B §2.1.3.3.4 specifies these class-2 locations before column-to-row permutation.
const CLASS_TWO_SINGLE_POSITIONS: [usize; 8] = [0, 26, 52, 78, 130, 156, 182, 208];
const CLASS_TWO_RANGES: [std::ops::RangeInclusive<usize>; 2] = [93..=129, 223..=259];
fn is_class_two(index: usize) -> bool {
    CLASS_TWO_SINGLE_POSITIONS.contains(&index)
        || CLASS_TWO_RANGES.iter().any(|range| range.contains(&index))
}
pub fn encode(bits: &SpeechBits) -> [bool; CHANNEL_BITS] {
    let mut input = [false; INPUT_BITS];
    input[CLASS_ONE_OFFSET..CLASS_ONE_OFFSET + CLASS_ONE_BITS].copy_from_slice(&bits.class_one);
    let crc = checksum(&input);
    for (coefficient, position) in PARITY_POSITIONS.into_iter().enumerate() {
        input[position] = crc & (1 << (CRC_BITS - 1 - coefficient)) != 0;
    }
    let mut protected = [false; PROTECTED_BITS];
    let mut state = 0;
    for (step, bit) in input.into_iter().enumerate() {
        let (next, output) = dcch::transition(state, bit);
        protected[step * OUTPUT_BITS..(step + 1) * OUTPUT_BITS].copy_from_slice(&output);
        state = next;
    }
    let mut channel = [false; CHANNEL_BITS];
    let mut protected_index = 0;
    let mut class_two_index = 0;
    for (index, bit) in channel.iter_mut().enumerate() {
        if is_class_two(index) {
            *bit = bits.class_two[class_two_index];
            class_two_index += 1;
        } else {
            *bit = protected[protected_index];
            protected_index += 1;
        }
    }
    channel
}
/// A passing CRC covers only the twelve significant class-1 bits, not all 159 speech bits.
pub fn decode(channel: &[bool; CHANNEL_BITS]) -> Result<SpeechFrame, SpeechError> {
    let candidate = decode_candidate(channel)?;
    match candidate.crc {
        CrcStatus::Valid => Ok(candidate.frame),
        CrcStatus::Failed => Err(SpeechError::Crc),
    }
}
/// CRC-failed parameters are retained for IS-54-B §2.2.2.2.3.2 masking, not trusted synthesis.
pub fn decode_candidate(channel: &[bool; CHANNEL_BITS]) -> Result<SpeechCandidate, SpeechError> {
    let mut protected = [false; PROTECTED_BITS];
    let mut class_two = [false; CLASS_TWO_BITS];
    let mut protected_index = 0;
    let mut class_two_index = 0;
    for (index, bit) in channel.iter().copied().enumerate() {
        if is_class_two(index) {
            class_two[class_two_index] = bit;
            class_two_index += 1;
        } else {
            protected[protected_index] = bit;
            protected_index += 1;
        }
    }
    let mut metrics = [UNREACHABLE; STATES];
    metrics[0] = 0;
    let mut ties = [false; STATES];
    let mut parents = [[0u8; STATES]; INPUT_BITS];
    for (step, pair) in protected.as_chunks::<OUTPUT_BITS>().0.iter().enumerate() {
        let mut next_metrics = [UNREACHABLE; STATES];
        let mut next_ties = [false; STATES];
        for (state, metric) in metrics.iter().copied().enumerate() {
            if metric == UNREACHABLE {
                continue;
            }
            for bit in [false, true] {
                let (next, output) = dcch::transition(state as u8, bit);
                let next = usize::from(next);
                let distance = u16::from(output[0] != pair[0]) + u16::from(output[1] != pair[1]);
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
    if ties[0] {
        return Err(SpeechError::Ambiguous);
    }
    let mut input = [false; INPUT_BITS];
    let mut state = 0;
    for step in (0..INPUT_BITS).rev() {
        input[step] = state & (1 << (TAIL_BITS - 1)) != 0;
        state = usize::from(parents[step][state]);
    }
    let received = PARITY_POSITIONS
        .into_iter()
        .fold(0u8, |crc, position| crc * 2 + u8::from(input[position]));
    let crc = if received == checksum(&input) {
        CrcStatus::Valid
    } else {
        CrcStatus::Failed
    };
    Ok(SpeechCandidate {
        crc,
        frame: SpeechFrame {
            bits: SpeechBits {
                class_one: input[CLASS_ONE_OFFSET..CLASS_ONE_OFFSET + CLASS_ONE_BITS]
                    .try_into()
                    .expect("fixed class-1 interval"),
                class_two,
            },
            corrected_coded_bits: metrics[0],
        },
    })
}
// IS-54-B Figure 2.1.3.3.4-1 assigns previous-frame bits to even rows and current-frame bits to odd rows.
pub fn interleave(
    previous: &[bool; CHANNEL_BITS],
    current: &[bool; CHANNEL_BITS],
) -> [bool; CHANNEL_BITS] {
    std::array::from_fn(|wire| {
        let row = wire / COLUMNS;
        let column = wire % COLUMNS;
        let index = column * ROWS + row;
        if row.is_multiple_of(2) {
            previous[index]
        } else {
            current[index]
        }
    })
}
pub fn deinterleave(
    first: &[bool; CHANNEL_BITS],
    second: &[bool; CHANNEL_BITS],
) -> [bool; CHANNEL_BITS] {
    std::array::from_fn(|index| {
        let row = index % ROWS;
        let column = index / ROWS;
        let wire = row * COLUMNS + column;
        if row.is_multiple_of(2) {
            second[wire]
        } else {
            first[wire]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bits() -> SpeechBits {
        SpeechBits {
            class_one: std::array::from_fn(|i| (i * 7 + 3) % 11 < 5),
            class_two: std::array::from_fn(|i| (i * 5 + 2) % 13 < 6),
        }
    }
    #[test]
    fn class_allocation_has_exact_178_protected_and_82_unprotected_positions() {
        assert_eq!(
            (0..CHANNEL_BITS).filter(|i| is_class_two(*i)).count(),
            CLASS_TWO_BITS
        );
        assert_eq!(CHANNEL_BITS - CLASS_TWO_BITS, PROTECTED_BITS);
    }
    #[test]
    fn two_slot_interleaving_preserves_every_bit_of_the_middle_frame() {
        let zero = [false; CHANNEL_BITS];
        for index in 0..CHANNEL_BITS {
            let mut middle = zero;
            middle[index] = true;
            let first = interleave(&zero, &middle);
            let second = interleave(&middle, &zero);
            assert_eq!(first.iter().chain(&second).filter(|bit| **bit).count(), 1);
            assert_eq!(deinterleave(&first, &second), middle);
        }
        let previous = [true; CHANNEL_BITS];
        let slot = interleave(&previous, &zero);
        assert_eq!(&slot[..10], &[true; 10]);
        assert_eq!(&slot[10..20], &[false; 10]);
    }
    #[test]
    fn protected_errors_correct_but_class_two_errors_remain_unprotected() {
        let expected = bits();
        let mut channel = encode(&expected);
        channel[1] = !channel[1];
        let decoded = decode(&channel).unwrap();
        assert_eq!(decoded.bits, expected);
        assert_eq!(decoded.corrected_coded_bits, 1);
        channel = encode(&expected);
        channel[0] = !channel[0];
        let decoded = decode(&channel).unwrap();
        assert_eq!(decoded.bits.class_one, expected.class_one);
        assert_ne!(decoded.bits.class_two, expected.class_two);
        assert_eq!(decoded.corrected_coded_bits, 0);
    }
    #[test]
    fn channel_word_matches_independent_polynomial_and_delay_equations() {
        let expected = bits();
        let golden = "10000001101111001001000110011010100011111110001101100101000111111100011011010110001111111000101001011010010100101101001010010110100101101010001111111000110111010100011111110001101101001000111111100011011010011101011000000000010110100101001011010010100101101001";
        let channel: [bool; CHANNEL_BITS] = golden
            .bytes()
            .map(|bit| bit == b'1')
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        assert_eq!(encode(&expected), channel);
        assert_eq!(decode(&channel).unwrap().bits, expected);
        let mut input = [false; INPUT_BITS];
        input[4..81].copy_from_slice(&expected.class_one);
        assert_eq!(checksum(&input), 1);
    }
    #[test]
    fn crc_basis_vectors_match_polynomial_division() {
        let expected = [121, 103, 104, 52, 26, 13, 93, 117, 97, 107, 110, 55];
        for (position, remainder) in SIGNIFICANT_POSITIONS.into_iter().zip(expected) {
            let mut input = [false; INPUT_BITS];
            input[position] = true;
            assert_eq!(checksum(&input), remainder);
        }
    }
    #[test]
    fn valid_convolutional_word_with_wrong_crc_is_rejected() {
        let invalid = "10000000011100100111000110011010100011111110001101100101000111111100011011010110001111111000101001011010010100101101001010010110100101101010001111111000110111010100011111110001101101001000111111100011011010011101011000000000010110100101001011010010100101101001";
        let channel: [bool; CHANNEL_BITS] = invalid
            .bytes()
            .map(|bit| bit == b'1')
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        assert_eq!(decode(&channel), Err(SpeechError::Crc));
        let candidate = decode_candidate(&channel).unwrap();
        assert_eq!(candidate.crc, CrcStatus::Failed);
        assert_eq!(candidate.frame.corrected_coded_bits, 0);
    }
}
