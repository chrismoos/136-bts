use crate::codec::mr74::{FRAME_BITS, Parameters};
use std::{error::Error, fmt};

pub const CHANNEL_BITS: usize = 260;
const CLASS_ONE_BITS: usize = 96;
const CLASS_ONE_A_BITS: usize = 48;
pub const PROTECTED_BITS: usize = 208;
const CRC_BITS: usize = 7;
const CRC_POLYNOMIAL: u8 = 0xb7;
const CRC_MASK: u8 = (1 << CRC_BITS) - 1;
const TAIL_BITS: usize = 5;
const INPUT_BITS: usize = CLASS_ONE_BITS + CRC_BITS + TAIL_BITS;
const STATES: usize = 1 << TAIL_BITS;
const PUNCTURE_PERIOD: usize = 27;
const COLUMNS: usize = 10;
const CRC_POSITIONS: [usize; CRC_BITS] = [24, 25, 26, 27, 78, 77, 76];
// TIA/EIA-136-410 Table 4.2 maps d[] to the MSB-first speech parameter stream s[].
const SPEECH_ORDER: [usize; FRAME_BITS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 26, 87, 27, 88, 28, 89, 29, 90, 30,
    91, 51, 80, 112, 141, 52, 81, 113, 142, 53, 82, 114, 143, 54, 83, 115, 144, 55, 84, 116, 145,
    58, 119, 59, 120, 17, 18, 19, 20, 21, 22, 23, 24, 25, 31, 92, 60, 121, 32, 93, 33, 94, 61, 122,
    62, 123, 56, 85, 117, 146, 57, 86, 118, 147, 50, 79, 111, 140, 49, 78, 110, 139, 48, 77, 109,
    138, 47, 76, 108, 137, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 63, 64, 65, 66, 67,
    68, 69, 70, 71, 72, 73, 74, 75, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107,
    124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136,
];
// TIA/EIA-136-410 Table 4.3, row-major wire order to c[].
const INTERLEAVER: [[usize; COLUMNS]; CHANNEL_BITS / COLUMNS] = [
    [220, 219, 218, 233, 232, 231, 211, 210, 209, 208],
    [0, 25, 50, 75, 100, 125, 150, 175, 200, 237],
    [1, 26, 51, 76, 101, 126, 151, 176, 201, 224],
    [2, 27, 52, 77, 102, 127, 152, 177, 202, 236],
    [3, 28, 53, 78, 103, 128, 153, 178, 203, 223],
    [4, 29, 54, 79, 104, 129, 154, 179, 204, 235],
    [5, 30, 55, 80, 105, 130, 155, 180, 205, 222],
    [6, 31, 56, 81, 106, 131, 156, 181, 206, 234],
    [7, 32, 57, 82, 107, 132, 157, 182, 207, 221],
    [8, 33, 58, 83, 108, 133, 158, 183, 250, 249],
    [9, 34, 59, 84, 109, 134, 159, 184, 217, 216],
    [10, 35, 60, 85, 110, 135, 160, 185, 248, 247],
    [11, 36, 61, 86, 111, 136, 161, 186, 215, 230],
    [12, 37, 62, 87, 112, 137, 162, 187, 246, 245],
    [13, 38, 63, 88, 113, 138, 163, 188, 229, 228],
    [14, 39, 64, 89, 114, 139, 164, 189, 244, 259],
    [15, 40, 65, 90, 115, 140, 165, 190, 214, 213],
    [16, 41, 66, 91, 116, 141, 166, 191, 258, 257],
    [17, 42, 67, 92, 117, 142, 167, 192, 212, 227],
    [18, 43, 68, 93, 118, 143, 168, 193, 243, 242],
    [19, 44, 69, 94, 119, 144, 169, 194, 226, 225],
    [20, 45, 70, 95, 120, 145, 170, 195, 241, 256],
    [21, 46, 71, 96, 121, 146, 171, 196, 240, 239],
    [22, 47, 72, 97, 122, 147, 172, 197, 255, 254],
    [23, 48, 73, 98, 123, 148, 173, 198, 238, 253],
    [24, 49, 74, 99, 124, 149, 174, 199, 252, 251],
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub parameters: Parameters,
    pub crc_valid: bool,
    pub corrected_coded_bits: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Ambiguous,
}
impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ACELP CC1 decode failed: {self:?}")
    }
}
impl Error for DecodeError {}

fn checksum(bits: &[bool]) -> u8 {
    let mut remainder = 0u8;
    for &bit in bits {
        let feedback = (remainder >> (CRC_BITS - 1) != 0) ^ bit;
        remainder = (remainder << 1) & CRC_MASK;
        if feedback {
            remainder ^= CRC_POLYNOMIAL & CRC_MASK;
        }
    }
    remainder
}
fn information_position(index: usize) -> usize {
    let pair = index / 2;
    match (index.is_multiple_of(2), index < CLASS_ONE_A_BITS) {
        (true, true) => pair,
        (false, true) => CLASS_ONE_BITS + CRC_BITS - 1 - pair,
        (true, false) => pair + 4,
        (false, false) => CLASS_ONE_BITS + CRC_BITS - 4 - pair,
    }
}
fn punctured(step: usize) -> bool {
    (step + 1).is_multiple_of(PUNCTURE_PERIOD)
}
// TIA/EIA-136-410 §4.3, D^0 is the newest input and g0 precedes g1.
fn transition(state: usize, bit: bool) -> (usize, [bool; 2]) {
    const G0: usize = 0b101011;
    const G1: usize = 0b111101;
    const STATE_MASK: usize = STATES - 1;
    let register = (state << 1) | usize::from(bit);
    (
        register & STATE_MASK,
        [G0, G1].map(|g| !(register & g).count_ones().is_multiple_of(2)),
    )
}
pub fn encode(parameters: &Parameters) -> [bool; CHANNEL_BITS] {
    let speech = parameters.codec_bits();
    let ordered: [bool; FRAME_BITS] = std::array::from_fn(|i| speech[SPEECH_ORDER[i]]);
    let mut input = [false; INPUT_BITS];
    for (i, &bit) in ordered[..CLASS_ONE_BITS].iter().enumerate() {
        input[information_position(i)] = bit;
    }
    let crc = checksum(&ordered[..CLASS_ONE_A_BITS]);
    for (i, position) in CRC_POSITIONS.into_iter().enumerate() {
        input[position] = crc & (1 << (CRC_BITS - 1 - i)) != 0;
    }
    let mut channel = [false; CHANNEL_BITS];
    let mut state = 0;
    let mut offset = 0;
    for (step, bit) in input.into_iter().enumerate() {
        let (next, pair) = transition(state, bit);
        state = next;
        if !punctured(step) {
            channel[offset..offset + 2].copy_from_slice(&pair);
            offset += 2;
        }
    }
    channel[PROTECTED_BITS..].copy_from_slice(&ordered[CLASS_ONE_BITS..]);
    channel
}
pub fn decode(channel: &[bool; CHANNEL_BITS]) -> Result<DecodedFrame, DecodeError> {
    let mut metrics = [u16::MAX; STATES];
    metrics[0] = 0;
    let mut ambiguous = [false; STATES];
    let mut parents = [[0u8; STATES]; INPUT_BITS];
    let mut offset = 0;
    for (step, parent) in parents.iter_mut().enumerate() {
        let mut next_metrics = [u16::MAX; STATES];
        let mut next_ambiguous = [false; STATES];
        for (state, metric) in metrics.into_iter().enumerate() {
            if metric == u16::MAX {
                continue;
            }
            for bit in [false, true] {
                let (next, pair) = transition(state, bit);
                let distance = if punctured(step) {
                    0
                } else {
                    u16::from(pair[0] != channel[offset])
                        + u16::from(pair[1] != channel[offset + 1])
                };
                let candidate = metric + distance;
                if candidate < next_metrics[next] {
                    next_metrics[next] = candidate;
                    next_ambiguous[next] = ambiguous[state];
                    parent[next] = state as u8;
                } else if candidate == next_metrics[next] {
                    next_ambiguous[next] = true;
                }
            }
        }
        metrics = next_metrics;
        ambiguous = next_ambiguous;
        if !punctured(step) {
            offset += 2;
        }
    }
    if ambiguous[0] {
        return Err(DecodeError::Ambiguous);
    }
    let mut input = [false; INPUT_BITS];
    let mut state = 0;
    for step in (0..INPUT_BITS).rev() {
        input[step] = state & 1 != 0;
        state = usize::from(parents[step][state]);
    }
    let mut ordered = [false; FRAME_BITS];
    for (i, bit) in ordered[..CLASS_ONE_BITS].iter_mut().enumerate() {
        *bit = input[information_position(i)];
    }
    ordered[CLASS_ONE_BITS..].copy_from_slice(&channel[PROTECTED_BITS..]);
    let received_crc = CRC_POSITIONS
        .into_iter()
        .fold(0u8, |value, i| value * 2 + u8::from(input[i]));
    let mut speech = [false; FRAME_BITS];
    for (i, &bit) in ordered.iter().enumerate() {
        speech[SPEECH_ORDER[i]] = bit;
    }
    Ok(DecodedFrame {
        parameters: Parameters::from_codec_bits(&speech),
        crc_valid: received_crc == checksum(&ordered[..CLASS_ONE_A_BITS]),
        corrected_coded_bits: metrics[0],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interleaving {
    One,
    Two,
    Three,
}
impl Interleaving {
    fn slots(self) -> usize {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 3,
        }
    }
}
// TIA/EIA-136-410 §4.4.1, history and received slots run oldest to newest.
pub fn interleave(history: &[[bool; CHANNEL_BITS]; 3], mode: Interleaving) -> [bool; CHANNEL_BITS] {
    std::array::from_fn(|wire| {
        let row = wire / COLUMNS;
        let column = wire % COLUMNS;
        let age = mode.slots() - 1 - row % mode.slots();
        history[2 - age][INTERLEAVER[row][column]]
    })
}
pub fn deinterleave(slots: &[[bool; CHANNEL_BITS]; 3], mode: Interleaving) -> [bool; CHANNEL_BITS] {
    let mut channel = [false; CHANNEL_BITS];
    for (row, indices) in INTERLEAVER.iter().enumerate() {
        let source = mode.slots() - 1 - row % mode.slots();
        for (column, &index) in indices.iter().enumerate() {
            channel[index] = slots[source][row * COLUMNS + column];
        }
    }
    channel
}

#[cfg(test)]
mod tests;
