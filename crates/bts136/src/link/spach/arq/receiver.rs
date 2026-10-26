use super::*;
use crate::link::rach::ArqStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRequest {
    pub status: ArqStatus,
    pub response_mode: ResponseMode,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reception {
    Ignored,
    Accepted {
        message: Option<SingleMessage>,
        status_request: Option<StatusRequest>,
    },
}
#[derive(Debug)]
struct Transaction {
    header: ArqHeader,
    identity: MobileIdentity,
    total_bits: usize,
    begin_data: Vec<bool>,
    continuations: Vec<Option<[bool; CONTINUE_DATA_BITS]>>,
    delivered: bool,
}
#[derive(Debug)]
pub struct ArqReassembler {
    identity: MobileIdentity,
    transaction: Option<Transaction>,
}
impl ArqReassembler {
    pub fn try_new(identity: MobileIdentity) -> Result<Self, SpachError> {
        let (_, width, value) = identity.parts();
        if value == 0 || value >= 1 << width {
            return Err(SpachError::Identity);
        }
        Ok(Self {
            identity,
            transaction: None,
        })
    }
    pub fn reset(&mut self) {
        self.transaction = None;
    }

    /// CRC-validated blocks only. The procedure driver must reset after transaction termination.
    pub fn push(&mut self, bits: &[bool]) -> Result<Reception, SpachError> {
        self.push_frame(decode(bits)?)
    }

    pub(crate) fn complete(&self) -> bool {
        self.transaction
            .as_ref()
            .is_some_and(|transaction| transaction.delivered)
    }

    pub(crate) fn push_frame(&mut self, frame: ArqFrame) -> Result<Reception, SpachError> {
        let polled_header = match frame {
            ArqFrame::Begin {
                header,
                identity,
                message_octets,
                data,
            } => {
                if identity != self.identity {
                    return Ok(Reception::Ignored);
                }
                let total_bits = usize::from(message_octets) * OCTET_BITS;
                let first_length = total_bits.min(data.len());
                let count = (total_bits - first_length).div_ceil(CONTINUE_DATA_BITS);
                if count > ARQ_MAP_BITS {
                    return Err(SpachError::FrameNumber);
                }
                self.transaction = Some(Transaction {
                    header,
                    identity,
                    total_bits,
                    begin_data: data[..first_length].to_vec(),
                    continuations: vec![None; count],
                    delivered: false,
                });
                header
            }
            ArqFrame::Continue {
                header,
                identity_type,
                frame_number,
                data,
            } => {
                let Some(transaction) = self.transaction.as_mut() else {
                    return Ok(Reception::Ignored);
                };
                if header.partial_echo != transaction.header.partial_echo
                    || header.channel != transaction.header.channel
                    || identity_type != transaction.identity.parts().0
                {
                    return Ok(Reception::Ignored);
                }
                let index = usize::from(frame_number);
                if index >= transaction.continuations.len() {
                    return Err(SpachError::FrameNumber);
                }
                let offset = transaction.begin_data.len() + index * CONTINUE_DATA_BITS;
                let length = (transaction.total_bits - offset).min(CONTINUE_DATA_BITS);
                if data[length..].iter().any(|bit| *bit) {
                    return Err(SpachError::Filler);
                }
                let slot = &mut transaction.continuations[index];
                if slot.is_some_and(|previous| previous != data) {
                    return Err(SpachError::DuplicateConflict);
                }
                *slot = Some(data);
                header
            }
        };
        let transaction = self
            .transaction
            .as_mut()
            .expect("accepted frame has a transaction");
        let status_request = if polled_header.polling {
            Some(StatusRequest {
                status: transaction.status(),
                response_mode: polled_header.response_mode,
            })
        } else {
            None
        };
        let message =
            if !transaction.delivered && transaction.continuations.iter().all(Option::is_some) {
                let mut data = transaction.begin_data.clone();
                for continuation in &transaction.continuations {
                    data.extend_from_slice(
                        continuation
                            .as_ref()
                            .expect("all continuation slots are present"),
                    );
                }
                data.truncate(transaction.total_bits);
                let message = data
                    .as_chunks::<OCTET_BITS>()
                    .0
                    .iter()
                    .map(|octet| {
                        octet
                            .iter()
                            .fold(0, |value, bit| value * 2 + u8::from(*bit))
                    })
                    .collect();
                transaction.delivered = true;
                Some(SingleMessage {
                    channel: transaction.header.channel,
                    flags: transaction.header.flags,
                    identity: transaction.identity,
                    message,
                })
            } else {
                None
            };
        Ok(Reception::Accepted {
            message,
            status_request,
        })
    }
    pub fn status(&self) -> Option<ArqStatus> {
        self.transaction.as_ref().map(Transaction::status)
    }
}
impl Transaction {
    fn status(&self) -> ArqStatus {
        let mut received_frames = [false; ARQ_MAP_BITS];
        for (received, frame) in received_frames.iter_mut().zip(&self.continuations) {
            *received = frame.is_some();
        }
        ArqStatus {
            partial_echo: self.header.partial_echo,
            received_frames,
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
            partial_echo: 42,
            polling: true,
            response_mode: ResponseMode::Reservation,
        }
    }
    #[test]
    fn shuffled_repeated_frames_recover_once_and_poll_snapshots_do_not_change() {
        let identity = MobileIdentity::Imsi50(1);
        let expected: Vec<u8> = (0..255).map(|value| value as u8).collect();
        let frames = fragment_message(header(), identity, &expected).unwrap();
        let mut receiver = ArqReassembler::try_new(identity).unwrap();
        let Reception::Accepted {
            status_request: Some(first_poll),
            ..
        } = receiver.push(&frames[0].encode().unwrap()).unwrap()
        else {
            panic!("expected polling");
        };
        assert_eq!(first_poll.status.received_frames, [false; ARQ_MAP_BITS]);
        let mut messages = Vec::new();
        for index in (1..frames.len()).rev() {
            let bits = frames[index].encode().unwrap();
            if let Reception::Accepted {
                message: Some(message),
                ..
            } = receiver.push(&bits).unwrap()
            {
                messages.push(message);
            }
            assert!(matches!(
                receiver.push(&bits).unwrap(),
                Reception::Accepted { message: None, .. }
            ));
        }
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message, expected);
        assert_eq!(
            receiver.status().unwrap().received_frames,
            [true; ARQ_MAP_BITS]
        );
        assert_eq!(first_poll.status.received_frames, [false; ARQ_MAP_BITS]);
    }
    #[test]
    fn unrelated_echo_invalid_filler_and_conflicting_repeats_do_not_corrupt_map() {
        let identity = MobileIdentity::Tmsi20(1);
        let frames = fragment_message(header(), identity, &[0xff; 30]).unwrap();
        let mut receiver = ArqReassembler::try_new(identity).unwrap();
        assert_eq!(
            receiver.push(&frames[1].encode().unwrap()).unwrap(),
            Reception::Ignored
        );
        receiver.push(&frames[0].encode().unwrap()).unwrap();
        let ArqFrame::Continue {
            header: mut other,
            identity_type,
            frame_number,
            data,
        } = frames[1].clone()
        else {
            panic!("expected CONTINUE");
        };
        other.partial_echo += 1;
        let unrelated = ArqFrame::Continue {
            header: other,
            identity_type,
            frame_number,
            data,
        };
        assert_eq!(
            receiver.push(&unrelated.encode().unwrap()).unwrap(),
            Reception::Ignored
        );
        receiver.push(&frames[1].encode().unwrap()).unwrap();
        let mut conflict = frames[1].encode().unwrap();
        conflict[INFORMATION_BITS - 1] = !conflict[INFORMATION_BITS - 1];
        assert_eq!(receiver.push(&conflict), Err(SpachError::DuplicateConflict));
        let snapshot = receiver.status().unwrap();
        let mut filler = frames.last().unwrap().encode().unwrap();
        filler[INFORMATION_BITS - 1] = true;
        assert_eq!(receiver.push(&filler), Err(SpachError::Filler));
        assert_eq!(receiver.status().unwrap(), snapshot);
        receiver.reset();
        assert_eq!(receiver.status(), None);
        assert_eq!(
            receiver.push(&frames[1].encode().unwrap()).unwrap(),
            Reception::Ignored
        );
    }
}
