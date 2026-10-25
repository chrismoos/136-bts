pub mod arq;
mod fragmentation;
pub mod paging;
pub use fragmentation::SpachReceiver;

use super::rach::MobileIdentity;
use crate::phy::control::INFORMATION_BITS;
use std::{error::Error, fmt};

const USAGE_BITS: usize = 3;
const TYPE_BITS: usize = 3;
const IDENTITY_TYPE_BITS: usize = 2;
const FLAG_BITS: usize = 1;
const LENGTH_BITS: usize = 8;
const OCTET_BITS: usize = 8;
const SINGLE_MSID: u64 = 0;
const CONTINUE: u64 = 4;
const HEADER_A_BITS: usize = 7;
const HEADER_B_BITS: usize = 7;
const TYPE_START: usize = HEADER_A_BITS;
const MAX_MESSAGE_OCTETS: usize = (1 << LENGTH_BITS) - 1;
const ARCH_USAGE: u8 = 3;
const SMSCH_USAGE: u8 = 4;
const PCH_USAGE: u8 = 7;
const TMSI20_TYPE: u64 = 0;
const TMSI24_TYPE: u64 = 1;
const MIN_TYPE: u64 = 2;
const TMSI20_BITS: usize = 20;
const TMSI24_BITS: usize = 24;
const MIN_BITS: usize = 34;
const IMSI_BITS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Channel {
    Arch = ARCH_USAGE,
    Smsch = SMSCH_USAGE,
    Pch = PCH_USAGE,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeaderFlags {
    pub pch_continuation: bool,
    pub bcch_change: bool,
    pub paging_frame_modifier: bool,
    pub reservation_response: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleMessage {
    pub channel: Channel,
    pub flags: HeaderFlags,
    pub identity: MobileIdentity,
    pub message: Vec<u8>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpachError {
    Length,
    Identity,
    UnsupportedUsage,
    UnsupportedFrame,
    Encrypted,
    Reserved,
    Filler,
    NeedsContinuation,
    MissingBegin,
    ChannelChange,
    PartialEcho,
    FrameNumber,
    Payload,
    DuplicateConflict,
}
impl fmt::Display for SpachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid non-ARQ SPACH frame: {self:?}")
    }
}
impl Error for SpachError {}

impl SingleMessage {
    // IS-136.1-A Figures 5-23/24/31/32 and Table 5-7. CRC and tails belong to PHY.
    fn begin_bits(&self) -> Result<Vec<bool>, SpachError> {
        let (identity_type, width, value) = self.identity.parts();
        if value == 0 || value >= 1 << width {
            return Err(SpachError::Identity);
        }
        let mut bits = Vec::new();
        put(&mut bits, self.channel as u64, USAGE_BITS);
        for flag in [
            self.flags.pch_continuation,
            self.flags.bcch_change,
            self.flags.paging_frame_modifier,
            false,
        ] {
            put(&mut bits, u64::from(flag), FLAG_BITS);
        }
        put(&mut bits, SINGLE_MSID, TYPE_BITS);
        put(&mut bits, u64::from(identity_type), IDENTITY_TYPE_BITS);
        put(&mut bits, 0, FLAG_BITS);
        put(
            &mut bits,
            u64::from(self.flags.reservation_response),
            FLAG_BITS,
        );
        put(&mut bits, 0, FLAG_BITS);
        put(&mut bits, value, width);
        if self.message.len() >= 1 << LENGTH_BITS {
            return Err(SpachError::Length);
        }
        put(&mut bits, self.message.len() as u64, LENGTH_BITS);
        for octet in &self.message {
            put(&mut bits, u64::from(*octet), OCTET_BITS);
        }
        Ok(bits)
    }

    pub fn encode(&self) -> Result<[bool; INFORMATION_BITS], SpachError> {
        let mut bits = self.begin_bits()?;
        if bits.len() > INFORMATION_BITS {
            return Err(SpachError::NeedsContinuation);
        }
        bits.resize(INFORMATION_BITS, false);
        Ok(bits.try_into().expect("frame padded to information length"))
    }
}

/// Input must be one CRC-validated SPACH block. Longer messages require continuation reassembly.
pub fn decode_single(bits: &[bool]) -> Result<SingleMessage, SpachError> {
    if bits.len() != INFORMATION_BITS {
        return Err(SpachError::Length);
    }
    let mut reader = Reader { bits, offset: 0 };
    let (channel, mut flags) = read_header_a(&mut reader)?;
    if reader.take(TYPE_BITS)? != SINGLE_MSID {
        return Err(SpachError::UnsupportedFrame);
    }
    let (identity, length, reservation_response) = read_begin(&mut reader)?;
    flags.reservation_response = reservation_response;
    if length * OCTET_BITS > bits.len() - reader.offset {
        return Err(SpachError::NeedsContinuation);
    }
    let message = (0..length)
        .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
        .collect::<Result<Vec<_>, _>>()?;
    if bits[reader.offset..].iter().any(|bit| *bit) {
        return Err(SpachError::Filler);
    }
    Ok(SingleMessage {
        channel,
        flags,
        identity,
        message,
    })
}
fn read_header_a(reader: &mut Reader<'_>) -> Result<(Channel, HeaderFlags), SpachError> {
    let channel = match reader.take(USAGE_BITS)? {
        value if value == u64::from(ARCH_USAGE) => Channel::Arch,
        value if value == u64::from(SMSCH_USAGE) => Channel::Smsch,
        value if value == u64::from(PCH_USAGE) => Channel::Pch,
        _ => return Err(SpachError::UnsupportedUsage),
    };
    let flags = HeaderFlags {
        pch_continuation: reader.take(FLAG_BITS)? != 0,
        bcch_change: reader.take(FLAG_BITS)? != 0,
        paging_frame_modifier: reader.take(FLAG_BITS)? != 0,
        reservation_response: false,
    };
    if reader.take(FLAG_BITS)? != 0 {
        return Err(SpachError::Reserved);
    }
    Ok((channel, flags))
}

fn read_begin(reader: &mut Reader<'_>) -> Result<(MobileIdentity, usize, bool), SpachError> {
    let (identity, reservation_response) = read_address(reader)?;
    let length = reader.take(LENGTH_BITS)? as usize;
    Ok((identity, length, reservation_response))
}

fn read_address(reader: &mut Reader<'_>) -> Result<(MobileIdentity, bool), SpachError> {
    let identity_type = reader.take(IDENTITY_TYPE_BITS)?;
    if reader.take(FLAG_BITS)? != 0 {
        return Err(SpachError::UnsupportedFrame);
    }
    let reservation_response = reader.take(FLAG_BITS)? != 0;
    if reader.take(FLAG_BITS)? != 0 {
        return Err(SpachError::Encrypted);
    }
    let identity = match identity_type {
        TMSI20_TYPE => MobileIdentity::Tmsi20(reader.take(TMSI20_BITS)? as u32),
        TMSI24_TYPE => MobileIdentity::Tmsi24(reader.take(TMSI24_BITS)? as u32),
        MIN_TYPE => MobileIdentity::Min34(reader.take(MIN_BITS)?),
        _ => MobileIdentity::Imsi50(reader.take(IMSI_BITS)?),
    };
    if identity.parts().2 == 0 {
        return Err(SpachError::Identity);
    }
    Ok((identity, reservation_response))
}

fn put(bits: &mut Vec<bool>, value: u64, width: usize) {
    bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
}
struct Reader<'a> {
    bits: &'a [bool],
    offset: usize,
}
impl Reader<'_> {
    fn take(&mut self, width: usize) -> Result<u64, SpachError> {
        let end = self.offset + width;
        let field = self.bits.get(self.offset..end).ok_or(SpachError::Length)?;
        self.offset = end;
        Ok(field
            .iter()
            .fold(0, |value, bit| value * 2 + u64::from(*bit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_arch_header_matches_fixed_bit_fields_and_rejects_invalid_frames() {
        let frame = SingleMessage {
            channel: Channel::Arch,
            flags: HeaderFlags::default(),
            identity: MobileIdentity::Tmsi20(1),
            message: vec![0x1d],
        };
        let bits = frame.encode().unwrap();
        let expected = "011000000000000000000000000000000010000000100011101";
        assert_eq!(
            bits[..expected.len()]
                .iter()
                .map(|bit| if *bit { '1' } else { '0' })
                .collect::<String>(),
            expected
        );
        assert_eq!(decode_single(&bits).unwrap(), frame);
        let mut broken = bits;
        broken[6] = true;
        assert_eq!(decode_single(&broken), Err(SpachError::Reserved));
        broken = bits;
        broken[14] = true;
        assert_eq!(decode_single(&broken), Err(SpachError::Encrypted));
        broken = bits;
        broken[INFORMATION_BITS - 1] = true;
        assert_eq!(decode_single(&broken), Err(SpachError::Filler));
        assert_eq!(
            SingleMessage {
                identity: MobileIdentity::Tmsi20(0),
                ..frame.clone()
            }
            .encode(),
            Err(SpachError::Identity)
        );
        assert_eq!(
            SingleMessage {
                message: vec![0; 255],
                ..frame
            }
            .encode(),
            Err(SpachError::NeedsContinuation)
        );
    }
}
