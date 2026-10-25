pub mod attempts;

use crate::phy::{dcch::BlockFormat, rach::BurstFormat};
use std::{error::Error, fmt};

const TMSI20_TYPE: u8 = 0;
const TMSI24_TYPE: u8 = 1;
const MIN_TYPE: u8 = 2;
const IMSI_TYPE: u8 = 3;
const TMSI20_BITS: usize = 20;
const TMSI24_BITS: usize = 24;
const MIN_BITS: usize = 34;
const IMSI_BITS: usize = 50;
const FLAG_BITS: usize = 1;
const TYPE_BITS: usize = 3;
const IDENTITY_TYPE_BITS: usize = 2;
const MESSAGE_COUNT_BITS: usize = 3;
const LENGTH_BITS: usize = 8;
const MAX_MESSAGES: usize = 8;
const MAX_MESSAGE_OCTETS: usize = 255;
const OCTET_BITS: usize = 8;
const ECHO_BITS: usize = 7;
const ECHO_MASK: u64 = 0x7f;
const ARQ_RESERVED_BITS: usize = 2;
pub const ARQ_MAP_BITS: usize = 26;
const BEGIN: u8 = 0;
const CONTINUE: u8 = 1;
const END: u8 = 2;
const BEGIN_END: u8 = 3;
const ARQ_STATUS: u8 = 4;

/// Packed air-interface identities. MIN and IMSI values are not decimal phone numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MobileIdentity {
    Tmsi20(u32),
    Tmsi24(u32),
    Min34(u64),
    Imsi50(u64),
}
impl MobileIdentity {
    pub(crate) fn parts(self) -> (u8, usize, u64) {
        match self {
            Self::Tmsi20(value) => (TMSI20_TYPE, TMSI20_BITS, u64::from(value)),
            Self::Tmsi24(value) => (TMSI24_TYPE, TMSI24_BITS, u64::from(value)),
            Self::Min34(value) => (MIN_TYPE, MIN_BITS, value),
            Self::Imsi50(value) => (IMSI_TYPE, IMSI_BITS, value),
        }
    }
    pub(crate) fn min_digits(self) -> Option<String> {
        match self {
            Self::Min34(value) => decoded_min(value),
            _ => None,
        }
    }
    pub fn partial_echo(self) -> u8 {
        (self.parts().2 & ECHO_MASK) as u8
    }
}

impl fmt::Display for MobileIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")?;
        if let Self::Min34(value) = *self
            && let Some(digits) = decoded_min(value)
        {
            write!(f, " MIN={digits}")?;
        }
        Ok(())
    }
}

// IS-136.2 §2.3.1 encodes three-digit groups with each decimal digit decremented modulo ten.
fn decoded_min(value: u64) -> Option<String> {
    const GROUP_BITS: usize = 10;
    const DIGIT_BITS: usize = 4;
    const LOCAL_BITS: usize = 24;
    const GROUP_MASK: u64 = (1 << GROUP_BITS) - 1;
    const DIGIT_MASK: u64 = (1 << DIGIT_BITS) - 1;
    const DECIMAL_GROUP_LIMIT: u64 = 1000;
    fn group(value: u64) -> Option<String> {
        (value < DECIMAL_GROUP_LIMIT).then(|| {
            format!("{value:03}")
                .bytes()
                .map(|digit| char::from(b'0' + (digit - b'0' + 1) % 10))
                .collect()
        })
    }
    if value >= 1 << MIN_BITS {
        return None;
    }
    let area = group(value >> LOCAL_BITS)?;
    let exchange = group((value >> (GROUP_BITS + DIGIT_BITS)) & GROUP_MASK)?;
    let digit = (value >> GROUP_BITS) & DIGIT_MASK;
    if !(1..=10).contains(&digit) {
        return None;
    }
    let tail = group(value & GROUP_MASK)?;
    Some(format!("{area}{exchange}{}{tail}", digit % 10))
}

#[cfg(test)]
mod identity_display_tests {
    use super::*;
    #[test]
    fn min_display_preserves_wire_identity_and_decodes_digits() {
        assert_eq!(
            MobileIdentity::Min34(3410151054).to_string(),
            "Min34(3410151054) MIN=3143781765"
        );
        assert_eq!(
            decoded_min((210 << 24) | 0x565f15),
            Some("3214567890".into())
        );
        assert_eq!(
            decoded_min((999 << 24) | (999 << 14) | (10 << 10) | 999),
            Some("0000000000".into())
        );
        assert_eq!(decoded_min(0), None);
        assert_eq!(decoded_min(1 << MIN_BITS), None);
        assert_eq!(decoded_min(1023 << 24), None);
        assert_eq!(MobileIdentity::Tmsi20(7).to_string(), "Tmsi20(7)");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSet {
    pub identity: MobileIdentity,
    pub messages: Vec<Vec<u8>>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArqStatus {
    pub partial_echo: u8,
    pub received_frames: [bool; ARQ_MAP_BITS],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pending(MobileIdentity),
    Duplicate(MobileIdentity),
    Complete(MessageSet),
    ArqStatus(ArqStatus),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RachLinkError {
    Length,
    FormatChange,
    Identity,
    MessageCount,
    MessageLength,
    Encrypted,
    Reserved,
    MissingBegin,
    ChangeIndicator,
    Filler,
    FrameType,
    Truncated,
}
impl fmt::Display for RachLinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid RACH link frame: {self:?}")
    }
}
impl Error for RachLinkError {}

fn information_bits(format: BurstFormat) -> usize {
    match format {
        BurstFormat::Normal => BlockFormat::ReverseNormal.information_bits(),
        BurstFormat::Abbreviated => BlockFormat::ReverseAbbreviated.information_bits(),
    }
}
fn put(bits: &mut Vec<bool>, value: u64, width: usize) {
    bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
}
fn set_type(bits: &mut [bool], kind: u8) {
    for (index, bit) in bits[..TYPE_BITS].iter_mut().enumerate() {
        *bit = kind & (1 << (TYPE_BITS - 1 - index)) != 0;
    }
}

// IS-136.1-A §5.2.1. Length indicators cannot straddle a frame boundary.
pub fn encode(
    format: BurstFormat,
    identity: MobileIdentity,
    messages: &[&[u8]],
) -> Result<Vec<Vec<bool>>, RachLinkError> {
    if messages.is_empty() || messages.len() > MAX_MESSAGES {
        return Err(RachLinkError::MessageCount);
    }
    if messages
        .iter()
        .any(|message| message.len() > MAX_MESSAGE_OCTETS)
    {
        return Err(RachLinkError::MessageLength);
    }
    let (kind, width, value) = identity.parts();
    if value >= 1 << width {
        return Err(RachLinkError::Identity);
    }
    let payload: Vec<bool> = messages
        .iter()
        .flat_map(|message| message.iter())
        .flat_map(|byte| (0..OCTET_BITS).rev().map(move |bit| byte & (1 << bit) != 0))
        .collect();
    let size = information_bits(format);
    let mut frame = Vec::with_capacity(size);
    put(&mut frame, u64::from(BEGIN), TYPE_BITS);
    put(&mut frame, u64::from(kind), IDENTITY_TYPE_BITS);
    frame.push(false);
    put(&mut frame, value, width);
    put(&mut frame, (messages.len() - 1) as u64, MESSAGE_COUNT_BITS);
    let mut lengths_sent = 0;
    let mut data_sent = 0;
    let mut ci = false;
    let mut first = true;
    let mut frames = Vec::new();
    loop {
        while lengths_sent < messages.len() && size - frame.len() >= LENGTH_BITS {
            put(&mut frame, messages[lengths_sent].len() as u64, LENGTH_BITS);
            lengths_sent += 1;
        }
        if lengths_sent == messages.len() {
            let take = (size - frame.len()).min(payload.len() - data_sent);
            frame.extend_from_slice(&payload[data_sent..data_sent + take]);
            data_sent += take;
        }
        let finished = lengths_sent == messages.len() && data_sent == payload.len();
        let kind = match (first, finished) {
            (true, true) => BEGIN_END,
            (true, false) => BEGIN,
            (false, true) => END,
            (false, false) => CONTINUE,
        };
        set_type(&mut frame, kind);
        if kind == CONTINUE {
            frame[TYPE_BITS] = ci;
            ci = !ci;
        }
        frame.resize(size, false);
        frames.push(frame);
        if finished {
            return Ok(frames);
        }
        first = false;
        frame = vec![false; TYPE_BITS + 1];
    }
}

pub fn encode_arq_status(
    format: BurstFormat,
    status: &ArqStatus,
) -> Result<Vec<bool>, RachLinkError> {
    if u64::from(status.partial_echo) > ECHO_MASK {
        return Err(RachLinkError::Identity);
    }
    let mut bits = Vec::with_capacity(information_bits(format));
    put(&mut bits, u64::from(ARQ_STATUS), TYPE_BITS);
    put(&mut bits, u64::from(status.partial_echo), ECHO_BITS);
    put(&mut bits, 0, ARQ_RESERVED_BITS);
    bits.extend(status.received_frames);
    bits.resize(information_bits(format), false);
    Ok(bits)
}

struct Pending {
    format: BurstFormat,
    identity: MobileIdentity,
    message_count: usize,
    lengths: Vec<usize>,
    payload: Vec<bool>,
    next_ci: bool,
    last_continue: Option<Vec<bool>>,
}
struct Completed {
    identity: MobileIdentity,
    frame: Vec<bool>,
    multiburst: bool,
}
#[derive(Default)]
pub struct RachReceiver {
    pending: Option<Pending>,
    completed: Option<Completed>,
}
impl RachReceiver {
    pub(crate) fn pending_identity(&self) -> Option<MobileIdentity> {
        self.pending.as_ref().map(|pending| pending.identity)
    }
    pub(crate) fn completed_multiburst(&self) -> bool {
        self.completed
            .as_ref()
            .is_some_and(|completed| completed.multiburst)
    }
    pub fn discontinuity(&mut self) {
        self.pending = None;
        self.completed = None;
    }

    /// Supply CRC-verified frames from one access attempt in order. Reset after gaps or a new attempt.
    pub fn push(&mut self, format: BurstFormat, bits: &[bool]) -> Result<Outcome, RachLinkError> {
        let result = self.push_frame(format, bits);
        if result.is_err() {
            self.discontinuity();
        }
        result
    }
    fn push_frame(&mut self, format: BurstFormat, bits: &[bool]) -> Result<Outcome, RachLinkError> {
        if bits.len() != information_bits(format) {
            return Err(RachLinkError::Length);
        }
        let mut reader = Reader { bits, offset: 0 };
        let kind = reader.take(TYPE_BITS)? as u8;
        if (kind == END || kind == BEGIN_END)
            && let Some(completed) = &self.completed
            && completed.frame.as_slice() == bits
        {
            return Ok(Outcome::Duplicate(completed.identity));
        }
        if kind == ARQ_STATUS {
            let partial_echo = reader.take(ECHO_BITS)? as u8;
            if reader.take(ARQ_RESERVED_BITS)? != 0 {
                return Err(RachLinkError::Reserved);
            }
            let mut received_frames = [false; ARQ_MAP_BITS];
            for bit in &mut received_frames {
                *bit = reader.take(FLAG_BITS)? != 0;
            }
            reader.filler()?;
            return Ok(Outcome::ArqStatus(ArqStatus {
                partial_echo,
                received_frames,
            }));
        }
        if kind == BEGIN || kind == BEGIN_END {
            self.completed = None;
            let idt = reader.take(IDENTITY_TYPE_BITS)? as u8;
            if reader.take(FLAG_BITS)? != 0 {
                return Err(RachLinkError::Encrypted);
            }
            let identity = match idt {
                TMSI20_TYPE => MobileIdentity::Tmsi20(reader.take(TMSI20_BITS)? as u32),
                TMSI24_TYPE => MobileIdentity::Tmsi24(reader.take(TMSI24_BITS)? as u32),
                MIN_TYPE => MobileIdentity::Min34(reader.take(MIN_BITS)?),
                IMSI_TYPE => MobileIdentity::Imsi50(reader.take(IMSI_BITS)?),
                _ => unreachable!("identity type is two bits"),
            };
            let message_count = reader.take(MESSAGE_COUNT_BITS)? as usize + 1;
            self.pending = Some(Pending {
                format,
                identity,
                message_count,
                lengths: Vec::with_capacity(message_count),
                payload: Vec::new(),
                next_ci: false,
                last_continue: None,
            });
        } else if kind == CONTINUE || kind == END {
            let indicator = reader.take(FLAG_BITS)? != 0;
            let pending = self.pending.as_mut().ok_or(RachLinkError::MissingBegin)?;
            if pending.format != format {
                return Err(RachLinkError::FormatChange);
            }
            if kind == END && indicator {
                return Err(RachLinkError::Reserved);
            }
            if kind == CONTINUE {
                if indicator != pending.next_ci {
                    if pending.last_continue.as_deref() == Some(bits) {
                        return Ok(Outcome::Duplicate(pending.identity));
                    }
                    return Err(RachLinkError::ChangeIndicator);
                }
                pending.next_ci = !pending.next_ci;
                pending.last_continue = Some(bits.to_vec());
            }
        } else {
            return Err(RachLinkError::FrameType);
        }
        let mut pending = self.pending.take().ok_or(RachLinkError::MissingBegin)?;
        while pending.lengths.len() < pending.message_count && reader.remaining() >= LENGTH_BITS {
            pending.lengths.push(reader.take(LENGTH_BITS)? as usize);
        }
        if pending.lengths.len() < pending.message_count {
            reader.filler()?;
        } else {
            let total_bits = pending.lengths.iter().sum::<usize>() * OCTET_BITS;
            let take = reader.remaining().min(total_bits - pending.payload.len());
            pending
                .payload
                .extend_from_slice(&bits[reader.offset..reader.offset + take]);
            reader.offset += take;
            reader.filler()?;
        }
        let complete = pending.lengths.len() == pending.message_count
            && pending.payload.len() == pending.lengths.iter().sum::<usize>() * OCTET_BITS;
        let final_frame = kind == END || kind == BEGIN_END;
        if final_frame && !complete {
            return Err(RachLinkError::Truncated);
        }
        if !final_frame && complete {
            return Err(RachLinkError::FrameType);
        }
        if complete {
            self.completed = Some(Completed {
                identity: pending.identity,
                frame: bits.to_vec(),
                multiburst: kind == END,
            });
            let mut offset = 0;
            let mut messages = Vec::with_capacity(pending.message_count);
            for length in pending.lengths {
                let end = offset + length * OCTET_BITS;
                messages.push(
                    pending.payload[offset..end]
                        .as_chunks::<OCTET_BITS>()
                        .0
                        .iter()
                        .map(|octet| octet.iter().fold(0, |byte, bit| byte * 2 + u8::from(*bit)))
                        .collect(),
                );
                offset = end;
            }
            return Ok(Outcome::Complete(MessageSet {
                identity: pending.identity,
                messages,
            }));
        }
        let identity = pending.identity;
        self.pending = Some(pending);
        Ok(Outcome::Pending(identity))
    }
}
struct Reader<'a> {
    bits: &'a [bool],
    offset: usize,
}
impl Reader<'_> {
    fn remaining(&self) -> usize {
        self.bits.len() - self.offset
    }
    fn take(&mut self, width: usize) -> Result<u64, RachLinkError> {
        if self.remaining() < width {
            return Err(RachLinkError::Truncated);
        }
        let value = self.bits[self.offset..self.offset + width]
            .iter()
            .fold(0, |value, bit| value * 2 + u64::from(*bit));
        self.offset += width;
        Ok(value)
    }
    fn filler(&mut self) -> Result<(), RachLinkError> {
        if self.bits[self.offset..].iter().any(|bit| *bit) {
            return Err(RachLinkError::Filler);
        }
        self.offset = self.bits.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn min_begin_end_header_matches_fixed_wire_fields() {
        let frames = encode(BurstFormat::Normal, MobileIdentity::Min34(1), &[&[0xa5]]).unwrap();
        assert_eq!(frames.len(), 1);
        let wire: String = frames[0][..59]
            .iter()
            .map(|b| if *b { '1' } else { '0' })
            .collect();
        assert_eq!(wire, format!("011100{:034b}0000000000110100101", 1));
    }
    #[test]
    fn all_identities_formats_and_spilled_lengths_reassemble() {
        let long: Vec<u8> = (0..255).collect();
        let messages: [&[u8]; 8] = [
            &long,
            &[0xa5],
            &[],
            &[0, 1, 2],
            &long,
            &[0x55],
            &[],
            &[0xff],
        ];
        for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
            for identity in [
                MobileIdentity::Tmsi20(0xfffff),
                MobileIdentity::Tmsi24(0xffffff),
                MobileIdentity::Min34(0x3ffffffff),
                MobileIdentity::Imsi50(0x3ffffffffffff),
            ] {
                let frames = encode(format, identity, &messages).unwrap();
                let mut receiver = RachReceiver::default();
                let mut result = None;
                for bits in &frames {
                    let outcome = receiver.push(format, bits).unwrap();
                    if bits[..3] == [false, false, true] {
                        assert_eq!(
                            receiver.push(format, bits).unwrap(),
                            Outcome::Duplicate(identity)
                        );
                    }
                    if let Outcome::Complete(set) = outcome {
                        assert_eq!(
                            receiver.push(format, bits).unwrap(),
                            Outcome::Duplicate(identity)
                        );
                        result = Some(set);
                    }
                }
                assert_eq!(
                    result,
                    Some(MessageSet {
                        identity,
                        messages: messages.iter().map(|m| m.to_vec()).collect()
                    })
                );
            }
        }
    }
    #[test]
    fn arq_map_reserved_filler_and_missing_bursts_are_checked() {
        let status = ArqStatus {
            partial_echo: 85,
            received_frames: std::array::from_fn(|i| i.is_multiple_of(3)),
        };
        for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
            let bits = encode_arq_status(format, &status).unwrap();
            let mut receiver = RachReceiver::default();
            assert_eq!(
                receiver.push(format, &bits).unwrap(),
                Outcome::ArqStatus(status.clone())
            );
            let mut bad = bits.clone();
            bad[10] = true;
            assert_eq!(receiver.push(format, &bad), Err(RachLinkError::Reserved));
            let mut bad = bits;
            *bad.last_mut().unwrap() = true;
            assert_eq!(receiver.push(format, &bad), Err(RachLinkError::Filler));
            let message = vec![0xa5; 255];
            let frames = encode(format, MobileIdentity::Min34(42), &[&message]).unwrap();
            receiver.push(format, &frames[0]).unwrap();
            receiver.discontinuity();
            assert_eq!(
                receiver.push(format, &frames[1]),
                Err(RachLinkError::MissingBegin)
            );
            let mut premature_end = frames[0].clone();
            set_type(&mut premature_end, BEGIN_END);
            assert_eq!(
                receiver.push(format, &premature_end),
                Err(RachLinkError::Truncated)
            );
            let mut encrypted = frames[0].clone();
            encrypted[TYPE_BITS + IDENTITY_TYPE_BITS] = true;
            assert_eq!(
                receiver.push(format, &encrypted),
                Err(RachLinkError::Encrypted)
            );
            receiver.push(format, &frames[0]).unwrap();
            receiver.push(format, &frames[1]).unwrap();
            let mut inconsistent_repeat = frames[1].clone();
            *inconsistent_repeat.last_mut().unwrap() ^= true;
            assert_eq!(
                receiver.push(format, &inconsistent_repeat),
                Err(RachLinkError::ChangeIndicator)
            );
            receiver.push(format, &frames[0]).unwrap();
            let other_format = match format {
                BurstFormat::Normal => BurstFormat::Abbreviated,
                BurstFormat::Abbreviated => BurstFormat::Normal,
            };
            let other_frames =
                encode(other_format, MobileIdentity::Min34(42), &[&message]).unwrap();
            assert_eq!(
                receiver.push(other_format, &other_frames[1]),
                Err(RachLinkError::FormatChange)
            );
            receiver.push(format, &frames[0]).unwrap();
            let mut bad = frames[1].clone();
            bad[3] = true;
            assert_eq!(
                receiver.push(format, &bad),
                Err(RachLinkError::ChangeIndicator)
            );
        }
        assert_eq!(
            encode(BurstFormat::Normal, MobileIdentity::Tmsi20(1 << 20), &[&[]]),
            Err(RachLinkError::Identity)
        );
        assert_eq!(
            encode(BurstFormat::Normal, MobileIdentity::Min34(42), &[]),
            Err(RachLinkError::MessageCount)
        );
    }
}
