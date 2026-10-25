use super::*;

impl SingleMessage {
    // IS-136.1-A §5.2.5.1 and Figure 5-37. CONTINUE has both headers and no EHI.
    pub fn encode_fragmented(&self) -> Result<Vec<[bool; INFORMATION_BITS]>, SpachError> {
        let bits = self.begin_bits()?;
        let first_length = bits.len().min(INFORMATION_BITS);
        let mut first = [false; INFORMATION_BITS];
        first[..first_length].copy_from_slice(&bits[..first_length]);
        let mut frames = vec![first];
        let mut header = first[..HEADER_A_BITS + HEADER_B_BITS].to_vec();
        for bit in 0..TYPE_BITS {
            header[TYPE_START + bit] = CONTINUE & (1 << (TYPE_BITS - 1 - bit)) != 0;
        }
        for data in bits[first_length..].chunks(INFORMATION_BITS - header.len()) {
            let mut frame = [false; INFORMATION_BITS];
            frame[..header.len()].copy_from_slice(&header);
            frame[header.len()..header.len() + data.len()].copy_from_slice(data);
            frames.push(frame);
        }
        Ok(frames)
    }
}

#[derive(Debug)]
struct Pending {
    frame: SingleMessage,
    total_bits: usize,
    data: Vec<bool>,
}
#[derive(Debug, Default)]
pub struct SpachReceiver {
    pending: Option<Pending>,
}
impl SpachReceiver {
    pub fn discontinuity(&mut self) {
        self.pending = None;
    }

    /// Feed ordered CRC-validated frames for one non-ARQ transaction. Reset after a missing frame.
    pub fn push(&mut self, bits: &[bool]) -> Result<Option<SingleMessage>, SpachError> {
        let result = self.push_frame(bits);
        if result.is_err() {
            self.discontinuity();
        }
        result
    }

    fn push_frame(&mut self, bits: &[bool]) -> Result<Option<SingleMessage>, SpachError> {
        if bits.len() != INFORMATION_BITS {
            return Err(SpachError::Length);
        }
        let mut reader = Reader { bits, offset: 0 };
        let (channel, mut flags) = read_header_a(&mut reader)?;
        match reader.take(TYPE_BITS)? {
            SINGLE_MSID => {
                self.discontinuity();
                let (identity, length, reservation_response) = read_begin(&mut reader)?;
                flags.reservation_response = reservation_response;
                self.pending = Some(Pending {
                    frame: SingleMessage {
                        channel,
                        flags,
                        identity,
                        message: Vec::new(),
                    },
                    total_bits: length * OCTET_BITS,
                    data: Vec::with_capacity(MAX_MESSAGE_OCTETS * OCTET_BITS),
                });
            }
            CONTINUE => {
                let pending = self.pending.as_ref().ok_or(SpachError::MissingBegin)?;
                if pending.frame.channel != channel {
                    return Err(SpachError::ChannelChange);
                }
                reader.take(IDENTITY_TYPE_BITS + FLAG_BITS + FLAG_BITS)?;
            }
            _ => return Err(SpachError::UnsupportedFrame),
        }
        let pending = self
            .pending
            .as_mut()
            .expect("BEGIN or CONTINUE established pending state");
        let count = (pending.total_bits - pending.data.len()).min(bits.len() - reader.offset);
        pending
            .data
            .extend_from_slice(&bits[reader.offset..reader.offset + count]);
        reader.offset += count;
        if pending.data.len() < pending.total_bits {
            return Ok(None);
        }
        if bits[reader.offset..].iter().any(|bit| *bit) {
            return Err(SpachError::Filler);
        }
        let mut pending = self
            .pending
            .take()
            .expect("complete pending state remains present");
        pending.frame.message = pending
            .data
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|octet| {
                octet
                    .iter()
                    .fold(0, |value, bit| value * 2 + u8::from(*bit))
            })
            .collect();
        Ok(Some(pending.frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_identities_channels_and_lengths_reassemble_without_octet_alignment() {
        for identity in [
            MobileIdentity::Tmsi20(1),
            MobileIdentity::Tmsi24(2),
            MobileIdentity::Min34(3),
            MobileIdentity::Imsi50(4),
        ] {
            for channel in [Channel::Arch, Channel::Smsch, Channel::Pch] {
                for length in [0, 1, 5, 7, 8, 12, 30, 255] {
                    let expected = SingleMessage {
                        channel,
                        identity,
                        flags: HeaderFlags::default(),
                        message: (0..length).map(|value| value as u8).collect(),
                    };
                    let frames = expected.encode_fragmented().unwrap();
                    let mut receiver = SpachReceiver::default();
                    for (index, bits) in frames.iter().enumerate() {
                        let result = receiver.push(bits).unwrap();
                        if index + 1 == frames.len() {
                            assert_eq!(result, Some(expected.clone()));
                        } else {
                            assert_eq!(result, None);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn continuation_header_is_fixed_and_discontinuity_discards_partial_messages() {
        let frame = SingleMessage {
            channel: Channel::Arch,
            flags: HeaderFlags::default(),
            identity: MobileIdentity::Min34(3),
            message: vec![0xff; 30],
        };
        let frames = frame.encode_fragmented().unwrap();
        let header = "01100001001000";
        assert_eq!(
            frames[1][..14]
                .iter()
                .map(|bit| if *bit { '1' } else { '0' })
                .collect::<String>(),
            header
        );
        let mut receiver = SpachReceiver::default();
        assert_eq!(receiver.push(&frames[1]), Err(SpachError::MissingBegin));
        assert_eq!(receiver.push(&frames[0]).unwrap(), None);
        receiver.discontinuity();
        assert_eq!(receiver.push(&frames[1]), Err(SpachError::MissingBegin));
        receiver.push(&frames[0]).unwrap();
        let mut changed = frames[1];
        changed[0] = true;
        assert_eq!(receiver.push(&changed), Err(SpachError::ChannelChange));
        assert_eq!(receiver.push(&frames[1]), Err(SpachError::MissingBegin));
        for bits in frames {
            receiver.push(&bits).unwrap();
        }
        let mut malformed = frame.encode_fragmented().unwrap();
        malformed.last_mut().unwrap()[INFORMATION_BITS - 1] = true;
        for bits in &malformed[..malformed.len() - 1] {
            receiver.push(bits).unwrap();
        }
        assert_eq!(
            receiver.push(malformed.last().unwrap()),
            Err(SpachError::Filler)
        );
    }
}
