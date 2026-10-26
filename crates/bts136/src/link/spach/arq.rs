pub mod inactivity;
pub mod procedure;
mod sender;
pub use sender::{ArqDelivery, DeliveryState, StatusDisposition};

mod receiver;
pub use receiver::{ArqReassembler, Reception, StatusRequest};

use super::*;
use crate::link::rach::ARQ_MAP_BITS;

const BEGIN: u64 = 5;
const CONTINUE: u64 = 6;
const ECHO_BITS: usize = 7;
const RESERVED_BITS: usize = 2;
const FRAME_NUMBER_BITS: usize = 5;
pub const CONTINUE_DATA_BITS: usize = 79;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseMode {
    Contention,
    Reservation,
}
impl ResponseMode {
    fn code(self) -> u64 {
        match self {
            Self::Contention => 0,
            Self::Reservation => 1,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArqHeader {
    pub channel: Channel,
    pub flags: HeaderFlags,
    pub partial_echo: u8,
    pub polling: bool,
    pub response_mode: ResponseMode,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArqFrame {
    Begin {
        header: ArqHeader,
        identity: MobileIdentity,
        message_octets: u8,
        /// The received data region includes zero filler when the message ends in this frame.
        data: Vec<bool>,
    },
    Continue {
        header: ArqHeader,
        /// Header B identity type of the transaction's MSID. MIN-only handsets ignore other types.
        identity_type: u8,
        frame_number: u8,
        /// The received data region includes zero filler when the message ends in this frame.
        data: [bool; CONTINUE_DATA_BITS],
    },
}
impl ArqFrame {
    // IS-136.1-A Figures 5-38/39 and §5.4.2. FRNO map supports indices 0 through 25.
    pub fn encode(&self) -> Result<[bool; INFORMATION_BITS], SpachError> {
        let (header, kind, identity_type) = match self {
            Self::Begin {
                header, identity, ..
            } => (header, BEGIN, identity.parts().0),
            Self::Continue {
                header,
                identity_type,
                ..
            } => (header, CONTINUE, *identity_type),
        };
        if header.channel == Channel::Pch {
            return Err(SpachError::UnsupportedUsage);
        }
        if usize::from(header.partial_echo) >= 1 << ECHO_BITS {
            return Err(SpachError::PartialEcho);
        }
        let mut bits = Vec::new();
        put(&mut bits, header.channel as u64, USAGE_BITS);
        for flag in [
            header.flags.pch_continuation,
            header.flags.bcch_change,
            header.flags.paging_frame_modifier,
            false,
        ] {
            put(&mut bits, u64::from(flag), FLAG_BITS);
        }
        put(&mut bits, kind, TYPE_BITS);
        put(&mut bits, u64::from(identity_type), IDENTITY_TYPE_BITS);
        put(&mut bits, 0, FLAG_BITS);
        put(
            &mut bits,
            u64::from(header.flags.reservation_response),
            FLAG_BITS,
        );
        if let Self::Begin { identity, .. } = self {
            let (_, width, value) = identity.parts();
            if value == 0 || value >= 1 << width {
                return Err(SpachError::Identity);
            }
            put(&mut bits, 0, FLAG_BITS);
            put(&mut bits, value, width);
        }
        put(&mut bits, u64::from(header.partial_echo), ECHO_BITS);
        put(&mut bits, 0, RESERVED_BITS);
        put(&mut bits, u64::from(header.polling), FLAG_BITS);
        put(&mut bits, header.response_mode.code(), FLAG_BITS);
        match self {
            Self::Begin {
                message_octets,
                data,
                ..
            } => {
                put(&mut bits, u64::from(*message_octets), LENGTH_BITS);
                if data.len() > INFORMATION_BITS - bits.len() {
                    return Err(SpachError::Payload);
                }
                let message_bits = usize::from(*message_octets) * OCTET_BITS;
                if data
                    .get(message_bits..)
                    .is_some_and(|suffix| suffix.iter().any(|bit| *bit))
                {
                    return Err(SpachError::Filler);
                }
                bits.extend_from_slice(data);
                bits.resize(INFORMATION_BITS, false);
            }
            Self::Continue {
                frame_number, data, ..
            } => {
                if usize::from(*frame_number) >= ARQ_MAP_BITS {
                    return Err(SpachError::FrameNumber);
                }
                put(&mut bits, u64::from(*frame_number), FRAME_NUMBER_BITS);
                bits.extend_from_slice(data);
            }
        }
        Ok(bits.try_into().expect("ARQ frame has information length"))
    }
}

// IS-136.1-A §5.4.2.1. Polling policy is supplied by the caller in header.
pub fn fragment_message(
    header: ArqHeader,
    identity: MobileIdentity,
    message: &[u8],
) -> Result<Vec<ArqFrame>, SpachError> {
    let message_octets = u8::try_from(message.len()).map_err(|_| SpachError::Length)?;
    let prototype = ArqFrame::Begin {
        header,
        identity,
        message_octets,
        data: Vec::new(),
    };
    prototype.encode()?;
    let (_, identity_bits, _) = identity.parts();
    let begin_header_bits = HEADER_A_BITS
        + HEADER_B_BITS
        + FLAG_BITS
        + identity_bits
        + ECHO_BITS
        + RESERVED_BITS
        + FLAG_BITS
        + FLAG_BITS
        + LENGTH_BITS;
    let capacity = INFORMATION_BITS - begin_header_bits;
    let payload: Vec<bool> = message
        .iter()
        .flat_map(|octet| {
            (0..OCTET_BITS)
                .rev()
                .map(move |bit| octet & (1 << bit) != 0)
        })
        .collect();
    let first_length = payload.len().min(capacity);
    let mut frames = vec![ArqFrame::Begin {
        header,
        identity,
        message_octets,
        data: payload[..first_length].to_vec(),
    }];
    for (frame_number, chunk) in payload[first_length..]
        .chunks(CONTINUE_DATA_BITS)
        .enumerate()
    {
        if frame_number >= ARQ_MAP_BITS {
            return Err(SpachError::FrameNumber);
        }
        let mut data = [false; CONTINUE_DATA_BITS];
        data[..chunk.len()].copy_from_slice(chunk);
        frames.push(ArqFrame::Continue {
            header,
            identity_type: identity.parts().0,
            frame_number: frame_number as u8,
            data,
        });
    }
    Ok(frames)
}

/// Input must be one CRC-validated SPACH information block. Message reassembly checks final filler.
pub fn decode(bits: &[bool]) -> Result<ArqFrame, SpachError> {
    if bits.len() != INFORMATION_BITS {
        return Err(SpachError::Length);
    }
    let mut reader = Reader { bits, offset: 0 };
    let (channel, mut flags) = read_header_a(&mut reader)?;
    if channel == Channel::Pch {
        return Err(SpachError::UnsupportedUsage);
    }
    let kind = reader.take(TYPE_BITS)?;
    enum Address {
        Begin(MobileIdentity),
        Continue(u8),
    }
    let address = match kind {
        BEGIN => {
            let (identity, reservation_response) = read_address(&mut reader)?;
            flags.reservation_response = reservation_response;
            Address::Begin(identity)
        }
        CONTINUE => {
            let identity_type = reader.take(IDENTITY_TYPE_BITS)? as u8;
            reader.take(FLAG_BITS)?;
            flags.reservation_response = reader.take(FLAG_BITS)? != 0;
            Address::Continue(identity_type)
        }
        _ => return Err(SpachError::UnsupportedFrame),
    };
    let partial_echo = reader.take(ECHO_BITS)? as u8;
    if reader.take(RESERVED_BITS)? != 0 {
        return Err(SpachError::Reserved);
    }
    let polling = reader.take(FLAG_BITS)? != 0;
    let response_mode = if reader.take(FLAG_BITS)? == 0 {
        ResponseMode::Contention
    } else {
        ResponseMode::Reservation
    };
    let header = ArqHeader {
        channel,
        flags,
        partial_echo,
        polling,
        response_mode,
    };
    match address {
        Address::Begin(identity) => {
            let message_octets = reader.take(LENGTH_BITS)? as u8;
            let data = bits[reader.offset..].to_vec();
            let message_bits = usize::from(message_octets) * OCTET_BITS;
            if data
                .get(message_bits..)
                .is_some_and(|suffix| suffix.iter().any(|bit| *bit))
            {
                return Err(SpachError::Filler);
            }
            Ok(ArqFrame::Begin {
                header,
                identity,
                message_octets,
                data,
            })
        }
        Address::Continue(identity_type) => {
            let frame_number = reader.take(FRAME_NUMBER_BITS)? as u8;
            if usize::from(frame_number) >= ARQ_MAP_BITS {
                return Err(SpachError::FrameNumber);
            }
            let data = bits[reader.offset..]
                .try_into()
                .expect("ARQ CONTINUE carries 79 data bits");
            Ok(ArqFrame::Continue {
                header,
                identity_type,
                frame_number,
                data,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header() -> ArqHeader {
        ArqHeader {
            channel: Channel::Smsch,
            flags: HeaderFlags::default(),
            partial_echo: 0x55,
            polling: true,
            response_mode: ResponseMode::Reservation,
        }
    }
    #[test]
    fn maximum_imsi_message_fits_one_begin_and_twenty_six_continuations() {
        let message: Vec<u8> = (0..255).map(|value| value as u8).collect();
        let frames = fragment_message(header(), MobileIdentity::Imsi50(1), &message).unwrap();
        assert_eq!(frames.len(), 27);
        let mut bits = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            let decoded = decode(&frame.encode().unwrap()).unwrap();
            match decoded {
                ArqFrame::Begin {
                    data,
                    message_octets,
                    ..
                } => {
                    assert_eq!(index, 0);
                    assert_eq!(message_octets, 255);
                    assert_eq!(data.len(), 25);
                    bits.extend(data);
                }
                ArqFrame::Continue {
                    frame_number, data, ..
                } => {
                    assert_eq!(usize::from(frame_number), index - 1);
                    bits.extend(data);
                }
            }
        }
        assert!(bits[message.len() * OCTET_BITS..].iter().all(|bit| !bit));
        let recovered: Vec<u8> = bits[..message.len() * OCTET_BITS]
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|octet| {
                octet
                    .iter()
                    .fold(0, |value, bit| value * 2 + u8::from(*bit))
            })
            .collect();
        assert_eq!(recovered, message);
        assert_eq!(
            fragment_message(header(), MobileIdentity::Tmsi20(1), &[0; 256]),
            Err(SpachError::Length)
        );
    }

    #[test]
    fn continue_matches_fixed_wire_header_and_all_valid_frame_numbers() {
        for frame_number in 0..ARQ_MAP_BITS as u8 {
            let frame = ArqFrame::Continue {
                header: header(),
                identity_type: 0,
                frame_number,
                data: [true; CONTINUE_DATA_BITS],
            };
            let bits = frame.encode().unwrap();
            let expected = format!("1000000110000010101010011{frame_number:05b}");
            assert_eq!(
                bits[..30]
                    .iter()
                    .map(|bit| if *bit { '1' } else { '0' })
                    .collect::<String>(),
                expected
            );
            assert_eq!(decode(&bits).unwrap(), frame);
        }
        assert_eq!(
            ArqFrame::Continue {
                header: header(),
                identity_type: 0,
                frame_number: 26,
                data: [false; CONTINUE_DATA_BITS]
            }
            .encode(),
            Err(SpachError::FrameNumber)
        );
    }
    #[test]
    fn continuations_carry_the_transaction_identity_type() {
        let message = [0x5a; 40];
        for (identity, idt) in [
            (MobileIdentity::Tmsi20(1), "00"),
            (MobileIdentity::Tmsi24(1), "01"),
            (MobileIdentity::Min34(1), "10"),
            (MobileIdentity::Imsi50(1), "11"),
        ] {
            let frames = fragment_message(header(), identity, &message).unwrap();
            assert!(frames.len() > 1);
            for frame in &frames[1..] {
                let bits = frame.encode().unwrap();
                let header_b: String = bits[HEADER_A_BITS..HEADER_A_BITS + HEADER_B_BITS]
                    .iter()
                    .map(|bit| if *bit { '1' } else { '0' })
                    .collect();
                assert_eq!(header_b, format!("110{idt}00"));
                assert_eq!(&decode(&bits).unwrap(), frame);
            }
        }
    }
    #[test]
    fn begin_preserves_identity_length_polling_and_padding() {
        for identity in [
            MobileIdentity::Tmsi20(1),
            MobileIdentity::Tmsi24(2),
            MobileIdentity::Min34(3),
            MobileIdentity::Imsi50(4),
        ] {
            let frame = ArqFrame::Begin {
                header: header(),
                identity,
                message_octets: 1,
                data: vec![true; OCTET_BITS],
            };
            let bits = frame.encode().unwrap();
            let decoded = decode(&bits).unwrap();
            assert_eq!(decoded.encode().unwrap(), bits);
            match decoded {
                ArqFrame::Begin {
                    header: decoded_header,
                    identity: decoded_identity,
                    message_octets,
                    data,
                } => {
                    assert_eq!(decoded_header, header());
                    assert_eq!(decoded_identity, identity);
                    assert_eq!(message_octets, 1);
                    assert_eq!(data[..OCTET_BITS], [true; OCTET_BITS]);
                    assert!(data[OCTET_BITS..].iter().all(|bit| !bit));
                }
                _ => panic!("expected BEGIN"),
            }
            let mut malformed = bits;
            malformed[INFORMATION_BITS - 1] = true;
            assert_eq!(decode(&malformed), Err(SpachError::Filler));
        }
        let invalid = ArqHeader {
            partial_echo: 128,
            ..header()
        };
        assert_eq!(
            ArqFrame::Continue {
                header: invalid,
                identity_type: 0,
                frame_number: 0,
                data: [false; CONTINUE_DATA_BITS]
            }
            .encode(),
            Err(SpachError::PartialEcho)
        );
    }
}
