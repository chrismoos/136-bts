use crate::{
    l3::broadcast::ProtocolVersion,
    l3::legacy_traffic::{self, MAX_MESSAGE_BITS, TrafficMessage, TrafficMessageError},
    l3::relay::Direction,
    phy::{facch::FacchWord, sacch::SacchWord},
};
use std::{error::Error, fmt};

pub const WORD_BITS: usize = 48;
const OCTET_BITS: usize = 8;
const FILLER: u8 = 0x0e;
const MAX_PENDING_BITS: usize = MAX_MESSAGE_BITS + WORD_BITS - 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalingWord {
    pub continuation: bool,
    pub contents: [bool; WORD_BITS],
}
impl From<&FacchWord> for SignalingWord {
    fn from(word: &FacchWord) -> Self {
        Self {
            continuation: word.continuation,
            contents: word.message,
        }
    }
}
impl From<&SacchWord> for SignalingWord {
    fn from(word: &SacchWord) -> Self {
        Self {
            continuation: word.continuation,
            contents: word.message,
        }
    }
}
impl SignalingWord {
    pub fn filler() -> Self {
        Self {
            continuation: false,
            contents: std::array::from_fn(|bit| {
                FILLER & (1 << (OCTET_BITS - 1 - bit % OCTET_BITS)) != 0
            }),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficLinkError {
    Overflow,
    Message(TrafficMessageError),
}
impl fmt::Display for TrafficLinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid traffic word stream: {self:?}")
    }
}
impl Error for TrafficLinkError {}
impl From<TrafficMessageError> for TrafficLinkError {
    fn from(error: TrafficMessageError) -> Self {
        Self::Message(error)
    }
}

// IS-54-B §2.6.5.6.3 allows no second message in a word and truncates the last filler octet.
pub fn encode(message: &TrafficMessage) -> Result<Vec<SignalingWord>, TrafficMessageError> {
    let bits = message.encode()?;
    let mut words = Vec::with_capacity(bits.len().div_ceil(WORD_BITS));
    for (index, chunk) in bits.chunks(WORD_BITS).enumerate() {
        let mut contents = [false; WORD_BITS];
        contents[..chunk.len()].copy_from_slice(chunk);
        for (offset, bit) in contents[chunk.len()..].iter_mut().enumerate() {
            *bit = FILLER & (1 << (OCTET_BITS - 1 - offset % OCTET_BITS)) != 0;
        }
        words.push(SignalingWord {
            continuation: index != 0,
            contents,
        });
    }
    Ok(words)
}
#[derive(Clone)]
pub struct TrafficMessageReceiver {
    pending: Vec<bool>,
    discarding: bool,
    direction: Direction,
    version: Option<ProtocolVersion>,
}
impl Default for TrafficMessageReceiver {
    fn default() -> Self {
        Self::new(Direction::Reverse)
    }
}
impl TrafficMessageReceiver {
    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn new(direction: Direction) -> Self {
        Self {
            pending: Vec::new(),
            discarding: false,
            direction,
            version: None,
        }
    }
    /// Supply the negotiated protocol version for version-dependent mandatory fields.
    pub fn with_version(direction: Direction, version: ProtocolVersion) -> Self {
        Self {
            version: Some(version),
            ..Self::new(direction)
        }
    }
    pub fn discontinuity(&mut self) {
        self.pending.clear();
        self.discarding = true;
    }

    /// Supply CRC-valid words from one channel. Reset after a failed word or discontinuity.
    pub fn push(
        &mut self,
        word: &SignalingWord,
    ) -> Result<Option<TrafficMessage>, TrafficLinkError> {
        let mut start = 0;
        if !word.continuation {
            self.pending.clear();
            self.discarding = false;
            while start + OCTET_BITS <= WORD_BITS {
                let octet = word.contents[start..start + OCTET_BITS]
                    .iter()
                    .fold(0, |value, bit| value * 2 + u8::from(*bit));
                if octet != FILLER {
                    break;
                }
                start += OCTET_BITS;
            }
        } else if self.discarding || self.pending.is_empty() {
            return Ok(None);
        }
        if self.pending.len() + WORD_BITS - start > MAX_PENDING_BITS {
            self.discontinuity();
            return Err(TrafficLinkError::Overflow);
        }
        self.pending.extend_from_slice(&word.contents[start..]);
        if self.pending.is_empty() {
            self.discarding = true;
            return Ok(None);
        }
        let decoded = match self.version {
            Some(version) => {
                legacy_traffic::decode_prefix_in_version(&self.pending, self.direction, version)
            }
            None => legacy_traffic::decode_prefix_in(&self.pending, self.direction),
        };
        match decoded {
            Ok(decoded) => {
                self.pending.clear();
                self.discarding = true;
                Ok(Some(decoded.message))
            }
            Err(TrafficMessageError::Truncated { .. }) => Ok(None),
            Err(error) => {
                self.discontinuity();
                Err(error.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extended_orders_require_version_context_and_survive_multiframe_word_assembly() {
        use crate::l3::legacy_traffic::{
            ExtendedMeasurementOrder, ExtendedMeasurementTargets, ExtendedMeasurementType,
            Hyperband, MAX_EXTENDED_MEASUREMENT_CHANNELS, MeasurementChannel,
        };
        for (version, support) in [
            (ProtocolVersion::Pv2, None),
            (ProtocolVersion::Pv6, Some(false)),
            (ProtocolVersion::Pv6, Some(true)),
        ] {
            let targets = ExtendedMeasurementTargets::try_new(
                ExtendedMeasurementType::WordErrorRate,
                (0..MAX_EXTENDED_MEASUREMENT_CHANNELS)
                    .map(|channel| MeasurementChannel {
                        hyperband: Hyperband::Pcs1900,
                        channel: channel as u16,
                    })
                    .collect(),
            )
            .unwrap();
            let message = TrafficMessage::ExtendedMeasurementOrder(ExtendedMeasurementOrder {
                targets,
                mobile_ack_support: support,
            });
            let words = encode(&message).unwrap();
            assert!(words.len() > 1);
            let mut receiver = TrafficMessageReceiver::with_version(Direction::Forward, version);
            for word in &words[..words.len() - 1] {
                assert!(receiver.push(word).unwrap().is_none());
            }
            assert_eq!(
                receiver.push(words.last().unwrap()).unwrap(),
                Some(message.clone())
            );
            let mut unknown = TrafficMessageReceiver::new(Direction::Forward);
            assert!(matches!(
                unknown.push(&words[0]),
                Err(TrafficLinkError::Message(TrafficMessageError::Field(
                    "extended_order_version_required"
                )))
            ));
            for word in &words[1..] {
                assert!(unknown.push(word).unwrap().is_none());
            }
            receiver.push(&words[0]).unwrap();
            receiver.discontinuity();
            for word in &words[1..] {
                assert!(receiver.push(word).unwrap().is_none());
            }
            let mut decoded = None;
            for word in &words {
                decoded = receiver.push(word).unwrap().or(decoded);
            }
            assert_eq!(decoded, Some(message));
        }
    }
    #[test]
    fn wrong_direction_headers_discard_continuations_and_recover_on_a_fresh_word() {
        use crate::l3::legacy_traffic::StatusFields;
        for (direction, invalid, valid) in [
            (
                Direction::Reverse,
                TrafficMessage::StatusRequest(StatusFields::identity_query()),
                TrafficMessage::Connect,
            ),
            (
                Direction::Forward,
                TrafficMessage::Status(StatusFields::identity_query()),
                TrafficMessage::Audit,
            ),
        ] {
            let mut receiver =
                TrafficMessageReceiver::with_version(direction, ProtocolVersion::Pv2);
            let words = encode(&invalid).unwrap();
            assert!(receiver.push(&words[0]).is_err());
            let mut continuation = words[0].clone();
            continuation.continuation = true;
            assert_eq!(receiver.push(&continuation).unwrap(), None);
            assert_eq!(
                receiver.push(&encode(&valid).unwrap()[0]).unwrap(),
                Some(valid)
            );
        }
    }

    #[test]
    fn connect_word_contains_unaligned_filler_and_exact_type_bits() {
        let words = encode(&TrafficMessage::Connect).unwrap();
        assert_eq!(words.len(), 1);
        let octets: Vec<_> = words[0]
            .contents
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|octet| {
                octet
                    .iter()
                    .fold(0, |value, bit| value * 2 + u8::from(*bit))
            })
            .collect();
        assert_eq!(octets, [0x30, 0x43, 0x83, 0x83, 0x83, 0x83]);
        assert!(!words[0].continuation);
    }
    #[test]
    fn fragmented_messages_preserve_filler_in_payload_and_reset_after_loss() {
        let message = TrafficMessage::Ack {
            ack_message_type: 0xc1,
            information: crate::l3::legacy_traffic::AcknowledgmentInformation {
                unparsed_bits: ([0x0e; 63])
                    .iter()
                    .flat_map(|byte| {
                        (0..u8::BITS)
                            .rev()
                            .map(move |shift| byte & (1 << shift) != 0)
                    })
                    .collect(),
                ..Default::default()
            },
        };
        let words = encode(&message).unwrap();
        let mut receiver = TrafficMessageReceiver::default();
        let mut decoded = Vec::new();
        for word in &words {
            if let Some(message) = receiver.push(word).unwrap() {
                decoded.push(message);
            }
        }
        assert_eq!(decoded, [message]);
        receiver.push(&words[0]).unwrap();
        receiver.discontinuity();
        for word in &words[1..] {
            assert_eq!(receiver.push(word).unwrap(), None);
        }
        assert_eq!(receiver.push(&SignalingWord::filler()).unwrap(), None);
        assert_eq!(
            receiver
                .push(&encode(&TrafficMessage::Connect).unwrap()[0])
                .unwrap(),
            Some(TrafficMessage::Connect)
        );
    }
    #[test]
    fn leading_filler_partial_header_unknown_types_and_continuation_precedence() {
        let mut first = SignalingWord::filler();
        first.contents[40..]
            .copy_from_slice(&[false, false, true, true, false, false, false, false]);
        let octets = [0x43u8, 0x83, 0x83, 0x83, 0x83, 0x83];
        let mut last = SignalingWord {
            continuation: true,
            contents: std::array::from_fn(|index| {
                octets[index / OCTET_BITS] & (1 << (OCTET_BITS - 1 - index % OCTET_BITS)) != 0
            }),
        };
        last.continuation = true;
        let mut receiver = TrafficMessageReceiver::default();
        assert_eq!(receiver.push(&first).unwrap(), None);
        assert_eq!(receiver.push(&last).unwrap(), Some(TrafficMessage::Connect));
        let mut unknown = SignalingWord::filler();
        unknown.contents[..10]
            .copy_from_slice(&[false, false, true, true, true, true, true, true, true, true]);
        assert_eq!(
            receiver.push(&unknown),
            Err(TrafficLinkError::Message(
                TrafficMessageError::UnsupportedType(0xff)
            ))
        );
        assert_eq!(receiver.push(&last).unwrap(), None);
        let partial = encode(&TrafficMessage::Release {
            information: crate::l3::legacy_traffic::ReleaseInformation {
                unparsed_bits: ([1; 63])
                    .iter()
                    .flat_map(|byte| {
                        (0..u8::BITS)
                            .rev()
                            .map(move |shift| byte & (1 << shift) != 0)
                    })
                    .collect(),
                ..Default::default()
            },
        })
        .unwrap();
        receiver.push(&partial[0]).unwrap();
        assert_eq!(
            receiver
                .push(&encode(&TrafficMessage::Connect).unwrap()[0])
                .unwrap(),
            Some(TrafficMessage::Connect)
        );
    }
}
