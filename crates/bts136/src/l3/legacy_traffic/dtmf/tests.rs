use super::*;
use crate::l3::legacy_traffic::{TrafficMessage, decode_prefix_in};
fn wire(text: &str) -> Vec<bool> {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c == '1')
        .collect()
}
#[test]
fn fixed_dtmf_vectors_match_direction_request_numbers_digits_and_padding() {
    let cases = [
        (
            DtmfMessage::Burst {
                request_number: 15,
                digits: Some(vec![
                    DtmfDigit::One,
                    DtmfDigit::Zero,
                    DtmfDigit::Star,
                    DtmfDigit::Hash,
                ]),
            },
            Direction::Reverse,
            "00 10010010 1111 000100 0001 000100 0001 1010 1011 1100 000000",
        ),
        (
            DtmfMessage::Burst {
                request_number: 0,
                digits: None,
            },
            Direction::Reverse,
            "00 10010010 0000 000000",
        ),
        (
            DtmfMessage::Continuous {
                request_number: 3,
                digit: DtmfDigit::Hash,
            },
            Direction::Reverse,
            "00 01100111 0011 1100",
        ),
        (
            DtmfMessage::Continuous {
                request_number: 4,
                digit: DtmfDigit::Null,
            },
            Direction::Reverse,
            "00 01100111 0100 0000",
        ),
        (
            DtmfMessage::BurstAck {
                request_number: 15,
                last_decoded_parameter: Some(1),
            },
            Direction::Forward,
            "00 00110111 1111 000010 0001 000001 0001 00",
        ),
        (
            DtmfMessage::ContinuousAck { request_number: 3 },
            Direction::Forward,
            "00 01010111 0011",
        ),
    ];
    for (message, direction, text) in cases {
        let message = TrafficMessage::Dtmf(message);
        let bits = wire(text);
        assert_eq!(message.encode().unwrap(), bits);
        let mut padded = bits.clone();
        padded.extend([true; 48]);
        let decoded = decode_prefix_in(&padded, direction).unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.consumed_bits, bits.len());
        for end in 0..bits.len() {
            assert!(matches!(
                decode_prefix_in(&bits[..end], direction),
                Err(TrafficMessageError::Truncated { .. })
            ));
        }
        let other = if direction == Direction::Forward {
            Direction::Reverse
        } else {
            Direction::Forward
        };
        assert!(decode_prefix_in(&bits, other).is_err());
    }
}
#[test]
fn dtmf_bounds_and_malformed_complete_optional_fields_are_not_continuations() {
    for count in [0, MAX_DIGITS] {
        let request = DtmfMessage::Burst {
            request_number: 0,
            digits: Some(vec![DtmfDigit::Hash; count]),
        };
        let message = TrafficMessage::Dtmf(request.clone());
        assert_eq!(
            decode_prefix_in(&message.encode().unwrap(), Direction::Reverse)
                .unwrap()
                .message,
            message
        );
        assert!(request.acknowledgment().is_some());
    }
    for request in [
        DtmfMessage::Burst {
            request_number: 0,
            digits: Some(vec![DtmfDigit::One; MAX_DIGITS + 1]),
        },
        DtmfMessage::ContinuousAck { request_number: 16 },
        DtmfMessage::BurstAck {
            request_number: 1,
            last_decoded_parameter: Some(16),
        },
    ] {
        assert!(TrafficMessage::Dtmf(request).encode().is_err());
    }
    for code in 13..=15 {
        assert!(DtmfDigit::decode(code).is_err());
    }
    for text in [
        "00 10010010 0000 000001 00010000",
        "00 10010010 0000 000010 0001000001111100",
        "00 00110111 0000 000010 0001000010000100",
    ] {
        let bits = wire(text);
        let direction = if text.contains("00110111") {
            Direction::Forward
        } else {
            Direction::Reverse
        };
        assert!(matches!(
            decode_prefix_in(&bits, direction),
            Err(TrafficMessageError::Field(_))
        ));
    }
}

#[test]
fn maximum_dtmf_burst_reassembles_and_word_loss_cannot_create_partial_digits() {
    use crate::link::traffic::{TrafficMessageReceiver, encode};
    let message = TrafficMessage::Dtmf(DtmfMessage::Burst {
        request_number: 15,
        digits: Some(vec![DtmfDigit::Hash; MAX_DIGITS]),
    });
    let words = encode(&message).unwrap();
    assert_eq!(words.len(), 6);
    let mut receiver = TrafficMessageReceiver::new(Direction::Reverse);
    for word in &words[..words.len() - 1] {
        assert!(receiver.push(word).unwrap().is_none());
    }
    assert_eq!(
        receiver.push(words.last().unwrap()).unwrap(),
        Some(message.clone())
    );
    for missing in 0..words.len() {
        let mut receiver = TrafficMessageReceiver::new(Direction::Reverse);
        for (index, word) in words.iter().enumerate() {
            if index == missing {
                receiver.discontinuity();
            } else {
                assert!(receiver.push(word).unwrap().is_none());
            }
        }
        let recovered = words
            .iter()
            .filter_map(|word| receiver.push(word).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(recovered, vec![message.clone()]);
    }
}
