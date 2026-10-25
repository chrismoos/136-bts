use crate::phy::control::INFORMATION_BITS;
use std::{error::Error, fmt};

const EBCCH_CYCLE_BITS: usize = 8;
const EBCCH_BEGIN_BITS: usize = 1 + EBCCH_CYCLE_BITS;
// TIA-136-122-B Figure 22: an E-BCCH CONTINUE frame starts with BC and a reserved bit.
const EBCCH_CONTINUE_HEADER_BITS: usize = 2;
const SINGLE_FRAME_CYCLE: usize = 1;
const CHANGE_BITS: usize = 2;
const BASE_HEADER_BITS: usize = 1 + CHANGE_BITS;
const LENGTH_BITS: usize = 8;
const CONTINUATION_LENGTH_BITS: usize = 7;
const OCTET_BITS: usize = 8;
pub(crate) const MIN_SLOTS: usize = 3;
pub(crate) const MAX_SLOTS: usize = 10;
const MAX_MESSAGE_OCTETS: usize = u8::MAX as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastChanges {
    pub fbcch: bool,
    pub ebcch: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastSignaling {
    pub changes: BroadcastChanges,
    pub notification: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastError {
    SlotCount,
    MessageLength,
    Capacity,
    ContinuationLength,
    Filler,
    Truncated,
}
impl fmt::Display for BroadcastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SlotCount => "F-BCCH allocation requires three to ten slots",
            Self::MessageLength => "broadcast message requires one to 255 octets",
            Self::Capacity => "broadcast messages exceed the allocated F-BCCH slots",
            Self::ContinuationLength => "invalid broadcast continuation length",
            Self::Filler => "broadcast filler contains nonzero bits",
            Self::Truncated => "truncated broadcast message header",
        })
    }
}
impl Error for BroadcastError {}

struct TransmitFragment {
    message: usize,
    bit: usize,
}

// IS-136.1-A §5.2.2, L3LI is octets while CLI is bits. Table 4-1 bounds allocation.
pub fn encode_fbcch(
    messages: &[&[u8]],
    changes: BroadcastChanges,
    slots: usize,
) -> Result<Vec<[bool; INFORMATION_BITS]>, BroadcastError> {
    if !(MIN_SLOTS..=MAX_SLOTS).contains(&slots) {
        return Err(BroadcastError::SlotCount);
    }
    if messages
        .iter()
        .any(|message| message.is_empty() || message.len() > MAX_MESSAGE_OCTETS)
    {
        return Err(BroadcastError::MessageLength);
    }
    let mut frames = Vec::with_capacity(slots);
    let mut next = 0;
    let mut pending: Option<TransmitFragment> = None;
    for _ in 0..slots {
        let mut frame = [false; INFORMATION_BITS];
        frame[1] = changes.fbcch;
        frame[2] = changes.ebcch;
        let mut position = BASE_HEADER_BITS;
        if let Some(fragment) = &pending {
            frame[0] = true;
            let count = (messages[fragment.message].len() * OCTET_BITS - fragment.bit)
                .min(INFORMATION_BITS - BASE_HEADER_BITS - CONTINUATION_LENGTH_BITS);
            write_integer(&mut frame, &mut position, count, CONTINUATION_LENGTH_BITS);
            append_fragment(&mut frame, &mut position, messages, &mut pending, count);
        } else {
            start_message(&mut frame, &mut position, messages, &mut next, &mut pending);
            let available = INFORMATION_BITS - position;
            append_fragment(&mut frame, &mut position, messages, &mut pending, available);
        }
        while pending.is_none() && position < INFORMATION_BITS {
            if next >= messages.len() || INFORMATION_BITS - position < 1 + LENGTH_BITS {
                break;
            }
            frame[position] = true;
            position += 1;
            start_message(&mut frame, &mut position, messages, &mut next, &mut pending);
            let available = INFORMATION_BITS - position;
            append_fragment(&mut frame, &mut position, messages, &mut pending, available);
        }
        frames.push(frame);
    }
    if pending.is_some() || next < messages.len() {
        return Err(BroadcastError::Capacity);
    }
    Ok(frames)
}

// TIA/EIA-136-122-B §2.3 Figures 20–21. ECL counts all L2 frames in the cycle.
pub fn encode_ebcch_single_frame(
    messages: &[&[u8]],
) -> Result<[bool; INFORMATION_BITS], BroadcastError> {
    encode_ebcch_begin(messages, SINGLE_FRAME_CYCLE as u8)
}
pub(crate) fn encode_ebcch_begin(
    messages: &[&[u8]],
    cycle_frames: u8,
) -> Result<[bool; INFORMATION_BITS], BroadcastError> {
    if cycle_frames == 0 {
        return Err(BroadcastError::SlotCount);
    }
    if messages
        .iter()
        .any(|message| message.is_empty() || message.len() > MAX_MESSAGE_OCTETS)
    {
        return Err(BroadcastError::MessageLength);
    }
    if messages.len() > (INFORMATION_BITS - EBCCH_BEGIN_BITS) / (LENGTH_BITS + OCTET_BITS) {
        return Err(BroadcastError::Capacity);
    }
    let required = EBCCH_BEGIN_BITS
        + messages
            .iter()
            .map(|message| LENGTH_BITS + message.len() * OCTET_BITS)
            .sum::<usize>()
        + messages.len().saturating_sub(1);
    if required > INFORMATION_BITS {
        return Err(BroadcastError::Capacity);
    }
    let mut frame = [false; INFORMATION_BITS];
    let mut position = 1;
    write_integer(
        &mut frame,
        &mut position,
        usize::from(cycle_frames),
        EBCCH_CYCLE_BITS,
    );
    if messages.is_empty() {
        return Ok(frame);
    }
    for (index, message) in messages.iter().enumerate() {
        write_integer(&mut frame, &mut position, message.len(), LENGTH_BITS);
        for byte in *message {
            write_integer(&mut frame, &mut position, usize::from(*byte), OCTET_BITS);
        }
        if position < INFORMATION_BITS {
            frame[position] = index + 1 < messages.len();
            position += 1;
        }
    }
    Ok(frame)
}
pub fn decode_ebcch_single_frame(
    frame: &[bool; INFORMATION_BITS],
) -> Result<Vec<Vec<u8>>, BroadcastError> {
    let (cycle_frames, messages) = decode_ebcch_begin(frame)?;
    if cycle_frames != SINGLE_FRAME_CYCLE as u8 {
        return Err(BroadcastError::SlotCount);
    }
    Ok(messages)
}
pub fn decode_ebcch_begin(
    frame: &[bool; INFORMATION_BITS],
) -> Result<(u8, Vec<Vec<u8>>), BroadcastError> {
    let mut position = 1;
    if frame[0] {
        return Err(BroadcastError::SlotCount);
    }
    let cycle_frames = read_integer(frame, &mut position, EBCCH_CYCLE_BITS)? as u8;
    if cycle_frames == 0 {
        return Err(BroadcastError::SlotCount);
    }
    let mut messages = Vec::new();
    loop {
        let length = read_integer(frame, &mut position, LENGTH_BITS)?;
        if length == 0 {
            break;
        }
        let mut message = Vec::with_capacity(length);
        for _ in 0..length {
            message.push(read_integer(frame, &mut position, OCTET_BITS)? as u8);
        }
        messages.push(message);
        if position == INFORMATION_BITS {
            break;
        }
        let more = read_integer(frame, &mut position, 1)? != 0;
        if !more {
            break;
        }
    }
    if frame[position..].iter().any(|bit| *bit) {
        return Err(BroadcastError::Filler);
    }
    Ok((cycle_frames, messages))
}

fn start_message(
    frame: &mut [bool; INFORMATION_BITS],
    position: &mut usize,
    messages: &[&[u8]],
    next: &mut usize,
    pending: &mut Option<TransmitFragment>,
) {
    let length = messages.get(*next).map_or(0, |message| message.len());
    write_integer(frame, position, length, LENGTH_BITS);
    if length != 0 {
        *pending = Some(TransmitFragment {
            message: *next,
            bit: 0,
        });
        *next += 1;
    }
}

fn append_fragment(
    frame: &mut [bool; INFORMATION_BITS],
    position: &mut usize,
    messages: &[&[u8]],
    pending: &mut Option<TransmitFragment>,
    available: usize,
) {
    let Some(fragment) = pending else {
        return;
    };
    let message = messages[fragment.message];
    let count = (message.len() * OCTET_BITS - fragment.bit).min(available);
    for index in 0..count {
        let bit = fragment.bit + index;
        frame[*position + index] =
            message[bit / OCTET_BITS] & (1 << (OCTET_BITS - 1 - bit % OCTET_BITS)) != 0;
    }
    *position += count;
    fragment.bit += count;
    if fragment.bit == message.len() * OCTET_BITS {
        *pending = None;
    }
}

fn write_integer(
    frame: &mut [bool; INFORMATION_BITS],
    position: &mut usize,
    value: usize,
    width: usize,
) {
    for offset in 0..width {
        frame[*position + offset] = value & (1 << (width - 1 - offset)) != 0;
    }
    *position += width;
}

struct ReceiveFragment {
    length_bits: usize,
    bits: Vec<bool>,
}

#[derive(Default)]
pub struct FbcchReceiver {
    changes: Option<BroadcastChanges>,
    pending: Option<ReceiveFragment>,
}
impl FbcchReceiver {
    pub fn discontinuity(&mut self) {
        self.pending = None;
    }

    /// Feed CRC-verified frames in order, resetting on gaps and superframe boundaries.
    pub fn push(
        &mut self,
        frame: &[bool; INFORMATION_BITS],
    ) -> Result<Vec<Vec<u8>>, BroadcastError> {
        let changes = BroadcastChanges {
            fbcch: frame[1],
            ebcch: frame[2],
        };
        if self.changes != Some(changes) {
            self.pending = None;
        }
        self.changes = Some(changes);
        let result = self.parse(frame);
        if result.is_err() {
            self.pending = None;
        }
        result
    }

    fn parse(&mut self, frame: &[bool; INFORMATION_BITS]) -> Result<Vec<Vec<u8>>, BroadcastError> {
        self.parse_at(frame, BASE_HEADER_BITS)
    }
    fn parse_at(
        &mut self,
        frame: &[bool; INFORMATION_BITS],
        header_bits: usize,
    ) -> Result<Vec<Vec<u8>>, BroadcastError> {
        let mut messages = Vec::new();
        let mut position = header_bits;
        if frame[0] {
            let count = read_integer(frame, &mut position, CONTINUATION_LENGTH_BITS)?;
            if count > INFORMATION_BITS - position {
                return Err(BroadcastError::ContinuationLength);
            }
            if let Some(fragment) = &self.pending {
                let remaining = fragment.length_bits - fragment.bits.len();
                if count > remaining || (count < remaining && count < INFORMATION_BITS - position) {
                    return Err(BroadcastError::ContinuationLength);
                }
            }
            self.append(&frame[position..position + count], &mut messages);
            position += count;
        } else {
            self.pending = None;
            self.start(frame, &mut position)?;
            self.consume(frame, &mut position, &mut messages);
        }
        while position < INFORMATION_BITS {
            if self.pending.is_some() {
                return Err(BroadcastError::ContinuationLength);
            }
            let begin = frame[position];
            position += 1;
            if !begin {
                if frame[position..].iter().any(|bit| *bit) {
                    return Err(BroadcastError::Filler);
                }
                return Ok(messages);
            }
            self.start(frame, &mut position)?;
            self.consume(frame, &mut position, &mut messages);
        }
        Ok(messages)
    }

    fn start(
        &mut self,
        frame: &[bool; INFORMATION_BITS],
        position: &mut usize,
    ) -> Result<(), BroadcastError> {
        let length_bits = read_integer(frame, position, LENGTH_BITS)? * OCTET_BITS;
        if length_bits != 0 {
            self.pending = Some(ReceiveFragment {
                length_bits,
                bits: Vec::with_capacity(length_bits),
            });
        }
        Ok(())
    }

    fn consume(
        &mut self,
        frame: &[bool; INFORMATION_BITS],
        position: &mut usize,
        messages: &mut Vec<Vec<u8>>,
    ) {
        if let Some(fragment) = &self.pending {
            let count =
                (fragment.length_bits - fragment.bits.len()).min(INFORMATION_BITS - *position);
            self.append(&frame[*position..*position + count], messages);
            *position += count;
        }
    }

    fn append(&mut self, bits: &[bool], messages: &mut Vec<Vec<u8>>) {
        let Some(fragment) = &mut self.pending else {
            return;
        };
        fragment.bits.extend_from_slice(bits);
        if fragment.bits.len() == fragment.length_bits {
            messages.push(
                fragment
                    .bits
                    .as_chunks::<OCTET_BITS>()
                    .0
                    .iter()
                    .map(|octet| {
                        octet
                            .iter()
                            .fold(0, |value, bit| (value << 1) | u8::from(*bit))
                    })
                    .collect(),
            );
            self.pending = None;
        }
    }
}

fn read_integer(
    frame: &[bool; INFORMATION_BITS],
    position: &mut usize,
    width: usize,
) -> Result<usize, BroadcastError> {
    let bits = frame
        .get(*position..*position + width)
        .ok_or(BroadcastError::Truncated)?;
    *position += width;
    Ok(bits
        .iter()
        .fold(0, |value, bit| (value << 1) | usize::from(*bit)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn standalone_ebcch_cycle_matches_fixed_field_layout_and_rejects_bad_filler() {
        let mut expected: Vec<bool> =
            "0 00000001 00000010 00001101 00000000 1 00000010 00001111 01000000 0"
                .chars()
                .filter(|bit| *bit != ' ')
                .map(|bit| bit == '1')
                .collect();
        expected.resize(INFORMATION_BITS, false);
        let frame = encode_ebcch_single_frame(&[&[0x0d, 0x00], &[0x0f, 0x40]]).unwrap();
        assert_eq!(frame.as_slice(), expected);
        assert_eq!(
            decode_ebcch_single_frame(&frame).unwrap(),
            [vec![0x0d, 0x00], vec![0x0f, 0x40]]
        );
        let mut invalid = frame;
        invalid[INFORMATION_BITS - 1] = true;
        assert_eq!(
            decode_ebcch_single_frame(&invalid),
            Err(BroadcastError::Filler)
        );
        assert!(encode_ebcch_single_frame(&[&[0; 20]]).is_err());
        let edge: [&[u8]; 5] = [&[1], &[2], &[3], &[4], &[5, 6, 7]];
        let boundary = encode_ebcch_single_frame(&edge).unwrap();
        assert_eq!(decode_ebcch_single_frame(&boundary).unwrap(), edge);
        assert!(
            decode_ebcch_single_frame(&encode_ebcch_single_frame(&[]).unwrap())
                .unwrap()
                .is_empty()
        );
    }

    use super::*;
    const CHANGES: BroadcastChanges = BroadcastChanges {
        fbcch: false,
        ebcch: false,
    };

    #[test]
    fn begins_with_octet_length_and_msb_first_payload() {
        let frames = encode_fbcch(
            &[&[0xa5, 0x3c]],
            BroadcastChanges {
                fbcch: true,
                ebcch: false,
            },
            3,
        )
        .unwrap();
        let expected = "0100000001010100101001111000";
        let bits: String = frames[0][..expected.len()]
            .iter()
            .map(|b| if *b { '1' } else { '0' })
            .collect();
        assert_eq!(bits, expected);
        assert!(frames[0][expected.len()..].iter().all(|b| !b));
        assert_eq!(
            FbcchReceiver::default().push(&frames[0]).unwrap(),
            [vec![0xa5, 0x3c]]
        );
    }

    #[test]
    fn fragments_and_concatenates_messages_across_frames() {
        let a: Vec<u8> = (0..30).collect();
        let b: Vec<u8> = (100..140).collect();
        let c = [0x55, 0xaa];
        let frames = encode_fbcch(&[&a, &b, &c], CHANGES, 10).unwrap();
        let mut receiver = FbcchReceiver::default();
        let decoded: Vec<_> = frames
            .iter()
            .flat_map(|frame| receiver.push(frame).unwrap())
            .collect();
        assert_eq!(decoded, [a, b, c.to_vec()]);
    }

    #[test]
    fn continuation_can_start_a_new_message_after_a_lost_begin() {
        let long = [0xa5; 13];
        let following = [0x01, 0x23];
        let frames = encode_fbcch(&[&long, &following], CHANGES, 3).unwrap();
        assert!(frames[1][0]);
        assert_eq!(
            FbcchReceiver::default().push(&frames[1]).unwrap(),
            [following.to_vec()]
        );
    }

    #[test]
    fn lost_frame_and_change_flags_discard_incomplete_messages() {
        let a = [0x5a; 40];
        let frames = encode_fbcch(&[&a], CHANGES, 4).unwrap();
        let mut receiver = FbcchReceiver::default();
        assert!(receiver.push(&frames[0]).unwrap().is_empty());
        receiver.discontinuity();
        assert!(receiver.push(&frames[1]).unwrap().is_empty());
        assert!(receiver.push(&frames[2]).unwrap().is_empty());
        assert!(receiver.push(&frames[3]).unwrap().is_empty());
        receiver.push(&frames[0]).unwrap();
        let mut changed = frames[1];
        changed[1] = true;
        assert!(receiver.push(&changed).unwrap().is_empty());
    }

    #[test]
    fn rejects_overflow_and_malformed_frames() {
        assert_eq!(
            encode_fbcch(&[&[0; 256]], CHANGES, 10),
            Err(BroadcastError::MessageLength)
        );
        assert_eq!(
            encode_fbcch(&[&[]], CHANGES, 10),
            Err(BroadcastError::MessageLength)
        );
        assert_eq!(
            encode_fbcch(&[], CHANGES, 2),
            Err(BroadcastError::SlotCount)
        );
        assert_eq!(
            encode_fbcch(&[&[0; 255]], CHANGES, 10),
            Err(BroadcastError::Capacity)
        );
        let mut idle = [false; INFORMATION_BITS];
        idle[20] = true;
        assert_eq!(
            FbcchReceiver::default().push(&idle),
            Err(BroadcastError::Filler)
        );
        let mut invalid = [false; INFORMATION_BITS];
        invalid[0] = true;
        invalid[3..10].fill(true);
        assert_eq!(
            FbcchReceiver::default().push(&invalid),
            Err(BroadcastError::ContinuationLength)
        );
    }
}

#[derive(Default)]
pub struct EbcchReceiver {
    receiver: FbcchReceiver,
}
impl EbcchReceiver {
    pub fn discontinuity(&mut self) {
        self.receiver.discontinuity();
    }
    pub fn push(
        &mut self,
        frame: &[bool; INFORMATION_BITS],
    ) -> Result<Vec<Vec<u8>>, BroadcastError> {
        let header = if frame[0] {
            EBCCH_CONTINUE_HEADER_BITS
        } else {
            let mut position = 1;
            if read_integer(frame, &mut position, EBCCH_CYCLE_BITS)? == 0 {
                self.discontinuity();
                return Err(BroadcastError::SlotCount);
            }
            EBCCH_BEGIN_BITS
        };
        let result = self.receiver.parse_at(frame, header);
        if result.is_err() {
            self.discontinuity();
        }
        result
    }
}
pub(crate) fn encode_ebcch_cycle(
    messages: &[&[u8]],
    frames: usize,
) -> Result<Vec<[bool; INFORMATION_BITS]>, BroadcastError> {
    if frames == 0 || frames > u8::MAX as usize {
        return Err(BroadcastError::SlotCount);
    }
    if messages
        .iter()
        .any(|message| message.is_empty() || message.len() > MAX_MESSAGE_OCTETS)
    {
        return Err(BroadcastError::MessageLength);
    }
    let mut output = Vec::with_capacity(frames);
    let mut next = 0;
    let mut pending: Option<TransmitFragment> = None;
    for _ in 0..frames {
        let mut frame = [false; INFORMATION_BITS];
        let mut position = 1;
        if let Some(fragment) = &pending {
            frame[0] = true;
            position = EBCCH_CONTINUE_HEADER_BITS;
            let count = (messages[fragment.message].len() * OCTET_BITS - fragment.bit)
                .min(INFORMATION_BITS - EBCCH_CONTINUE_HEADER_BITS - CONTINUATION_LENGTH_BITS);
            write_integer(&mut frame, &mut position, count, CONTINUATION_LENGTH_BITS);
            append_fragment(&mut frame, &mut position, messages, &mut pending, count);
        } else {
            write_integer(&mut frame, &mut position, frames, EBCCH_CYCLE_BITS);
            start_message(&mut frame, &mut position, messages, &mut next, &mut pending);
            let available = INFORMATION_BITS - position;
            append_fragment(&mut frame, &mut position, messages, &mut pending, available);
        }
        while pending.is_none()
            && next < messages.len()
            && INFORMATION_BITS - position > LENGTH_BITS
        {
            frame[position] = true;
            position += 1;
            start_message(&mut frame, &mut position, messages, &mut next, &mut pending);
            let available = INFORMATION_BITS - position;
            append_fragment(&mut frame, &mut position, messages, &mut pending, available);
        }
        output.push(frame);
    }
    if pending.is_some() || next < messages.len() {
        return Err(BroadcastError::Capacity);
    }
    Ok(output)
}

#[cfg(test)]
mod ebcch_tests {
    use super::*;

    #[test]
    fn continue_frame_reserves_the_bit_after_bc() {
        let long = vec![0xA5u8; 40];
        let frames = encode_ebcch_cycle(&[&long], 5).unwrap();
        let continuation = frames
            .iter()
            .find(|frame| frame[0])
            .expect("a 40-octet message needs a CONTINUE frame");
        assert!(!continuation[1], "RSVD must be zero");
        let mut position = EBCCH_CONTINUE_HEADER_BITS;
        let count = read_integer(continuation, &mut position, CONTINUATION_LENGTH_BITS).unwrap();
        assert!(
            count > 0
                && count
                    <= INFORMATION_BITS - EBCCH_CONTINUE_HEADER_BITS - CONTINUATION_LENGTH_BITS
        );

        let mut receiver = EbcchReceiver::default();
        let mut decoded = Vec::new();
        for frame in &frames {
            decoded.extend(receiver.push(frame).unwrap());
        }
        assert_eq!(decoded, vec![long]);
    }
}
