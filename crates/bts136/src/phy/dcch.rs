use std::{error::Error, fmt};

const TAIL_BITS: usize = 5;
const CRC_BITS: usize = 16;
const DVCC_BITS: usize = 8;
const CRC_POLYNOMIAL: u16 = 0x1021;
const STATES: usize = 1 << TAIL_BITS;
const G0: u8 = 0o65;
const G1: u8 = 0o57;
const INPUT_MASK: u8 = 1 << TAIL_BITS;
const UNREACHABLE_METRIC: u16 = u16::MAX;
const FORWARD_ROWS: usize = 13;
const REVERSE_ROWS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockFormat {
    Forward,
    ReverseNormal,
    ReverseAbbreviated,
}

impl BlockFormat {
    pub fn encoded_bits(self) -> usize {
        match self {
            Self::Forward => 260,
            Self::ReverseNormal => 244,
            Self::ReverseAbbreviated => 200,
        }
    }
    pub fn information_bits(self) -> usize {
        self.encoded_bits() / 2 - CRC_BITS - TAIL_BITS
    }
    fn rows(self) -> usize {
        match self {
            Self::Forward => FORWARD_ROWS,
            Self::ReverseNormal | Self::ReverseAbbreviated => REVERSE_ROWS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrcMode {
    Broadcast,
    Addressed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    Length { expected: usize, actual: usize },
    Crc,
}
impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length { expected, actual } => {
                write!(f, "DCCH block requires {expected} bits, received {actual}")
            }
            Self::Crc => f.write_str("DCCH CRC mismatch"),
        }
    }
}
impl Error for BlockError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedBlock {
    pub information: Vec<bool>,
    pub corrected_coded_bits: u16,
}

// IS-136.1-A §§4.4.3 and 5.2.6. F-BCCH uses DVCC zero and complemented CRC.
pub fn encode(
    format: BlockFormat,
    information: &[bool],
    dvcc: u8,
    mode: CrcMode,
) -> Result<Vec<bool>, BlockError> {
    check_length(information, format.information_bits())?;
    let checksum = checksum(information, dvcc, mode);
    let mut input = Vec::with_capacity(format.encoded_bits() / 2);
    input.extend_from_slice(information);
    input.extend((0..CRC_BITS).rev().map(|bit| checksum & (1 << bit) != 0));
    input.extend([false; TAIL_BITS]);
    Ok(interleave(format, &convolutional_encode(&input)))
}

pub fn decode(
    format: BlockFormat,
    encoded: &[bool],
    dvcc: u8,
    mode: CrcMode,
) -> Result<DecodedBlock, BlockError> {
    check_length(encoded, format.encoded_bits())?;
    let (decoded, corrected_coded_bits) = viterbi(&deinterleave(format, encoded));
    finish_decode(format, &decoded, corrected_coded_bits, dvcc, mode)
}
fn finish_decode(
    format: BlockFormat,
    decoded: &[bool],
    corrected_coded_bits: u16,
    dvcc: u8,
    mode: CrcMode,
) -> Result<DecodedBlock, BlockError> {
    let information = decoded[..format.information_bits()].to_vec();
    let received_crc = decoded[format.information_bits()..format.information_bits() + CRC_BITS]
        .iter()
        .fold(0, |crc, bit| (crc << 1) | u16::from(*bit));
    if received_crc != checksum(&information, dvcc, mode) {
        return Err(BlockError::Crc);
    }
    Ok(DecodedBlock {
        information,
        corrected_coded_bits,
    })
}

// Positive reliability favors zero. Negative reliability favors one. Zero is an erasure.
pub(crate) fn decode_soft(
    format: BlockFormat,
    encoded: &[f64],
    dvcc: u8,
    mode: CrcMode,
) -> Result<DecodedBlock, BlockError> {
    if encoded.len() != format.encoded_bits() {
        return Err(BlockError::Length {
            expected: format.encoded_bits(),
            actual: encoded.len(),
        });
    }
    if encoded.iter().any(|value| !value.is_finite()) {
        return Err(BlockError::Crc);
    }
    let values = deinterleave(format, encoded);
    let decoded = viterbi_soft(&values);
    let corrected = convolutional_encode(&decoded)
        .iter()
        .zip(&values)
        .filter(|(bit, value)| **bit != (**value < 0.0))
        .count() as u16;
    finish_decode(format, &decoded, corrected, dvcc, mode)
}
fn viterbi_soft(encoded: &[f64]) -> Vec<bool> {
    let steps = encoded.len() / 2;
    let mut metrics = [f64::INFINITY; STATES];
    metrics[0] = 0.0;
    let mut predecessors = vec![[0u8; STATES]; steps];
    for (step, pair) in encoded.as_chunks::<2>().0.iter().enumerate() {
        let mut next_metrics = [f64::INFINITY; STATES];
        for (state, metric) in metrics.iter().copied().enumerate() {
            if !metric.is_finite() {
                continue;
            }
            for input in [false, true] {
                let (next, output) = transition(state as u8, input);
                let distance: f64 = output
                    .iter()
                    .zip(pair)
                    .map(|(bit, reliability)| {
                        if *bit != (*reliability < 0.0) {
                            reliability.abs()
                        } else {
                            0.0
                        }
                    })
                    .sum();
                let candidate = metric + distance;
                if candidate < next_metrics[usize::from(next)] {
                    next_metrics[usize::from(next)] = candidate;
                    predecessors[step][usize::from(next)] = state as u8;
                }
            }
        }
        metrics = next_metrics;
    }
    let mut state = 0usize;
    let mut bits = vec![false; steps];
    for step in (0..steps).rev() {
        bits[step] = state & (1 << (TAIL_BITS - 1)) != 0;
        state = usize::from(predecessors[step][state]);
    }
    bits
}

fn check_length(bits: &[bool], expected: usize) -> Result<(), BlockError> {
    if bits.len() != expected {
        return Err(BlockError::Length {
            expected,
            actual: bits.len(),
        });
    }
    Ok(())
}

pub(crate) fn checksum(information: &[bool], dvcc: u8, mode: CrcMode) -> u16 {
    let color = (0..DVCC_BITS).rev().map(|bit| dvcc & (1 << bit) != 0);
    let mut crc = 0u16;
    for bit in color.chain(information.iter().copied()) {
        let feedback = (crc >> (CRC_BITS - 1) != 0) ^ bit;
        crc <<= 1;
        if feedback {
            crc ^= CRC_POLYNOMIAL;
        }
    }
    match mode {
        CrcMode::Broadcast => !crc,
        CrcMode::Addressed => crc,
    }
}

// IS-136.2-A §2.1.3.3.3.4, free coefficient is the MSB, g0 precedes g1.
pub(crate) fn transition(state: u8, input: bool) -> (u8, [bool; 2]) {
    let register = state | if input { INPUT_MASK } else { 0 };
    let output = [G0, G1].map(|generator| (register & generator).count_ones() & 1 != 0);
    (register >> 1, output)
}

fn convolutional_encode(input: &[bool]) -> Vec<bool> {
    let mut state = 0;
    let mut coded = Vec::with_capacity(input.len() * 2);
    for bit in input {
        let (next, output) = transition(state, *bit);
        coded.extend(output);
        state = next;
    }
    coded
}

fn viterbi(encoded: &[bool]) -> (Vec<bool>, u16) {
    let steps = encoded.len() / 2;
    let mut metrics = [UNREACHABLE_METRIC; STATES];
    metrics[0] = 0;
    let mut predecessors = vec![[0u8; STATES]; steps];
    for (step, pair) in encoded.as_chunks::<2>().0.iter().enumerate() {
        let mut next_metrics = [UNREACHABLE_METRIC; STATES];
        for (state, metric) in metrics.iter().copied().enumerate() {
            if metric == UNREACHABLE_METRIC {
                continue;
            }
            for input in [false, true] {
                let (next, output) = transition(state as u8, input);
                let distance = u16::from(output[0] != pair[0]) + u16::from(output[1] != pair[1]);
                let candidate = metric + distance;
                if candidate < next_metrics[usize::from(next)] {
                    next_metrics[usize::from(next)] = candidate;
                    predecessors[step][usize::from(next)] = state as u8;
                }
            }
        }
        metrics = next_metrics;
    }
    let mut state = 0usize;
    let mut bits = vec![false; steps];
    for step in (0..steps).rev() {
        bits[step] = state & (1 << (TAIL_BITS - 1)) != 0;
        state = usize::from(predecessors[step][state]);
    }
    (bits, metrics[0])
}

// IS-136.1-A §4.4.3.2, column entry, even rows first, omit absent cells.
fn permutation(format: BlockFormat) -> Vec<usize> {
    let rows = format.rows();
    let columns = format.encoded_bits().div_ceil(rows);
    (0..rows)
        .step_by(2)
        .chain((1..rows).step_by(2))
        .flat_map(|row| (0..columns).map(move |column| row + column * rows))
        .filter(|index| *index < format.encoded_bits())
        .collect()
}
fn interleave(format: BlockFormat, coded: &[bool]) -> Vec<bool> {
    permutation(format)
        .into_iter()
        .map(|index| coded[index])
        .collect()
}
fn deinterleave<T: Copy + Default>(format: BlockFormat, encoded: &[T]) -> Vec<T> {
    let mut coded = vec![T::default(); encoded.len()];
    for (bit, index) in encoded.iter().zip(permutation(format)) {
        coded[index] = *bit;
    }
    coded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_decoding_recovers_low_confidence_errors_rejected_by_hard_decoding() {
        const WEAK_ERRORS: usize = 40;
        const WEAK_RELIABILITY: f64 = 0.001;
        for format in [
            BlockFormat::Forward,
            BlockFormat::ReverseNormal,
            BlockFormat::ReverseAbbreviated,
        ] {
            let information: Vec<_> = (0..format.information_bits())
                .map(|index| index % 3 == 0)
                .collect();
            let encoded = encode(format, &information, 23, CrcMode::Addressed).unwrap();
            let mut soft: Vec<_> = encoded
                .iter()
                .map(|bit| if *bit { -1.0 } else { 1.0 })
                .collect();
            for index in 0..WEAK_ERRORS {
                let position = index * 7 % encoded.len();
                soft[position] *= -WEAK_RELIABILITY;
            }
            let hard: Vec<_> = soft.iter().map(|value| *value < 0.0).collect();
            assert!(decode(format, &hard, 23, CrcMode::Addressed).is_err());
            let decoded = decode_soft(format, &soft, 23, CrcMode::Addressed).unwrap();
            assert_eq!(decoded.information, information);
            assert_eq!(decoded.corrected_coded_bits, WEAK_ERRORS as u16);
            assert!(decode_soft(format, &soft, 24, CrcMode::Addressed).is_err());
        }
    }

    #[test]
    fn convolutional_impulse_response_from_generator_equations() {
        assert_eq!(
            convolutional_encode(&[true, false, false, false, false, false]),
            [
                true, true, true, false, false, true, true, true, false, true, true, true
            ]
        );
    }

    #[test]
    fn crc_matches_ccitt_zero_initial_check_vector() {
        let information: Vec<bool> = b"123456789"
            .iter()
            .flat_map(|byte| (0..8).rev().map(move |bit| byte & (1 << bit) != 0))
            .collect();
        assert_eq!(checksum(&information, 0, CrcMode::Addressed), 0x31c3);
        assert_eq!(checksum(&information, 0, CrcMode::Broadcast), !0x31c3);
    }

    #[test]
    fn interleaver_orders_from_standard_tables() {
        let forward = permutation(BlockFormat::Forward);
        assert_eq!(&forward[..4], &[0, 13, 26, 39]);
        assert_eq!(forward[19], 247);
        assert_eq!(forward[20], 2);
        assert_eq!(forward[140], 1);
        let normal = permutation(BlockFormat::ReverseNormal);
        assert_eq!(normal[20], 240);
        assert_eq!(normal[21], 2);
        assert_eq!(normal[122], 1);
        let abbreviated = permutation(BlockFormat::ReverseAbbreviated);
        assert_eq!(abbreviated[16], 192);
        assert_eq!(abbreviated[17], 2);
        assert_eq!(abbreviated[100], 1);
        for format in [
            BlockFormat::Forward,
            BlockFormat::ReverseNormal,
            BlockFormat::ReverseAbbreviated,
        ] {
            let mut indexes = permutation(format);
            indexes.sort_unstable();
            assert_eq!(indexes, (0..format.encoded_bits()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn terminated_decoder_recovers_errors_and_checks_color() {
        for format in [
            BlockFormat::Forward,
            BlockFormat::ReverseNormal,
            BlockFormat::ReverseAbbreviated,
        ] {
            let information: Vec<bool> =
                (0..format.information_bits()).map(|i| i % 3 == 0).collect();
            let mut encoded = encode(format, &information, 27, CrcMode::Addressed).unwrap();
            encoded[17] = !encoded[17];
            let decoded = decode(format, &encoded, 27, CrcMode::Addressed).unwrap();
            assert_eq!(decoded.information, information);
            assert_eq!(decoded.corrected_coded_bits, 1);
            assert_eq!(
                decode(format, &encoded, 28, CrcMode::Addressed),
                Err(BlockError::Crc)
            );
            assert_eq!(
                decode(format, &encoded, 27, CrcMode::Broadcast),
                Err(BlockError::Crc)
            );
            assert!(
                decode(
                    format,
                    &encoded[..encoded.len() - 1],
                    27,
                    CrcMode::Addressed
                )
                .is_err()
            );
        }
    }
}
