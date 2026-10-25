use super::*;
use crate::l3::legacy_traffic::{TrafficMessage, decode_prefix_in};
fn wire(text: &str) -> Vec<bool> {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c == '1')
        .collect()
}
#[test]
fn fixed_flash_vectors_preserve_direction_specific_fields_and_ascii_number_encoding() {
    let cases = [
        (
            FlashMessage::Forward {
                request_number: 5,
                information: CallInformation {
                    calling_numbers: vec![CallingNumber::network("1001").unwrap()],
                    ..Default::default()
                },
            },
            Direction::Forward,
            "00 11110001 0101 001000 0100 000110 0000001000000011 00110001 00110000 00110000 00110001 000000",
        ),
        (
            FlashMessage::Reverse {
                request_number: 3,
                request: ReverseFlashFields {
                    feature: Some(vec![7]),
                    keypad: Some("*2".into()),
                    ..Default::default()
                },
            },
            Direction::Reverse,
            "00 11110001 0011 000101 0010 000001 0111 0011 000010 00101010 00110010",
        ),
        (
            FlashMessage::Forward {
                request_number: 0,
                information: CallInformation {
                    calling_name: Some(CallingName {
                        presentation: 1,
                        screening: 0,
                        characters: "AB".into(),
                    }),
                    ..Default::default()
                },
            },
            Direction::Forward,
            "00 11110001 0000 000101 0110 000011 00000100 01000001 01000010 000000",
        ),
        (
            FlashMessage::Ack(FlashAcknowledgment {
                request_number: 5,
                last_decoded_parameter: Some(4),
                unparsed_bits: vec![],
            }),
            Direction::Reverse,
            "00 10100001 0101 000010 0001 000001 0100 00",
        ),
    ];
    for (message, direction, text) in cases {
        let bits = wire(text);
        let message = TrafficMessage::Flash(message);
        assert_eq!(message.encode().unwrap(), bits);
        let decoded = decode_prefix_in(&bits, direction).unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.consumed_bits, bits.len());
        for end in 0..bits.len() {
            assert!(matches!(
                decode_prefix_in(&bits[..end], direction),
                Err(TrafficMessageError::Truncated { .. })
            ));
        }
    }
}
#[test]
fn complete_optional_errors_are_malformed_and_unknown_tails_are_preserved() {
    for text in [
        "00 11110001 0000 000001 01000000",
        "00 10100001 0000 000010 0001000010000100",
    ] {
        assert!(matches!(
            decode_prefix_in(&wire(text), Direction::Reverse),
            Err(TrafficMessageError::Field(_))
        ));
    }
    let message = TrafficMessage::Flash(FlashMessage::Forward {
        request_number: 15,
        information: CallInformation {
            voice_messages: Some(vec![0]),
            unparsed_bits: wire("0111 0011 000001 01"),
            ..Default::default()
        },
    });
    let bits = message.encode().unwrap();
    let decoded = decode_prefix_in(&bits, Direction::Forward).unwrap();
    assert_eq!(decoded.message.encode().unwrap(), bits);
    let TrafficMessage::Flash(FlashMessage::Forward { information, .. }) = decoded.message else {
        panic!()
    };
    assert!(!information.unparsed_bits.is_empty());
    assert_eq!(information.voice_messages, Some(vec![0]));
}
#[test]
fn maximum_name_fragmentation_and_field_bounds_keep_messages_bounded() {
    use crate::link::traffic::{TrafficMessageReceiver, encode};
    let message = TrafficMessage::Flash(FlashMessage::Forward {
        request_number: 15,
        information: CallInformation {
            calling_name: Some(CallingName {
                presentation: 0,
                screening: 3,
                characters: "x".repeat(MAX_NAME_CHARACTERS),
            }),
            ..Default::default()
        },
    });
    let bits = message.encode().unwrap();
    assert_eq!(bits.len(), 524);
    let words = encode(&message).unwrap();
    assert_eq!(words.len(), 11);
    let mut receiver = TrafficMessageReceiver::new(Direction::Forward);
    let mut result = None;
    for word in &words {
        if let Some(decoded) = receiver.push(word).unwrap() {
            assert!(result.is_none());
            result = Some(decoded);
        }
    }
    assert_eq!(result, Some(message.clone()));
    let mut received_words = words.clone();
    // TIA-136-133-E §5.1.2.3.16 puts the RSVD nibble after the 20-bit Flash prefix and 10-bit IE header.
    const NAME_RESERVED_START: usize = 30;
    const NAME_RESERVED_END: usize = 34;
    received_words[0].contents[NAME_RESERVED_START..NAME_RESERVED_END].fill(true);
    let mut receiver = TrafficMessageReceiver::new(Direction::Forward);
    let mut decoded = Vec::new();
    for word in received_words {
        let coded = crate::phy::facch::encode(word.continuation, &word.contents, 1);
        let received = crate::phy::facch::decode(&coded, 1).unwrap();
        decoded.extend(
            receiver
                .push(&crate::link::traffic::SignalingWord::from(&received))
                .unwrap(),
        );
    }
    assert_eq!(decoded.as_slice(), std::slice::from_ref(&message));
    for missing in 0..words.len() {
        let mut receiver = TrafficMessageReceiver::new(Direction::Forward);
        for (index, word) in words.iter().enumerate() {
            if index == missing {
                receiver.discontinuity();
            } else {
                assert!(receiver.push(word).unwrap().is_none());
            }
        }
    }
    assert!(CallingNumber::network(&"1".repeat(16)).is_err());
    assert!(CallingNumber::network("é").is_err());
    for information in [
        CallInformation {
            calling_name: Some(CallingName {
                presentation: 0,
                screening: 3,
                characters: "x".repeat(61),
            }),
            ..Default::default()
        },
        CallInformation {
            voice_messages: Some(vec![64]),
            ..Default::default()
        },
        CallInformation {
            calling_numbers: vec![CallingNumber::network("1").unwrap(); 3],
            ..Default::default()
        },
    ] {
        assert!(information.validate().is_err());
    }
}
#[test]
fn baseline_notifications_roundtrip_with_zero_counts_and_two_numbers() {
    let information = CallInformation {
        voice_messages: Some(vec![0]),
        signals: Some(vec![0]),
        calling_numbers: vec![
            CallingNumber::network("1001").unwrap(),
            CallingNumber::unavailable(),
        ],
        other_messages: Some(vec![
            MessageWaiting { kind: 1, count: 0 },
            MessageWaiting { kind: 2, count: 63 },
        ]),
        calling_name: Some(CallingName {
            presentation: 0,
            screening: 3,
            characters: "Alice".into(),
        }),
        ..Default::default()
    };
    let message = TrafficMessage::Flash(FlashMessage::Forward {
        request_number: 1,
        information,
    });
    let bits = message.encode().unwrap();
    assert_eq!(
        decode_prefix_in(&bits, Direction::Forward).unwrap().message,
        message
    );
}

#[test]
fn received_flash_reserved_bits_are_ignored_and_encoding_clears_them() {
    for direction in [Direction::Forward, Direction::Reverse] {
        let canonical = wire(
            "00 11110001 0101 001000 0100 000110 000 0001 00000 00 11 00110001 00110000 00110000 00110001 000000",
        );
        let expected = decode_prefix_in(&canonical, direction).unwrap().message;
        for reserved in 0..32 {
            let received = wire(&format!(
                "00 11110001 0101 001000 0100 000110 000 0001 {reserved:05b} 00 11 00110001 00110000 00110000 00110001 000000"
            ));
            let decoded = decode_prefix_in(&received, direction).unwrap();
            assert_eq!(decoded.consumed_bits, received.len());
            assert_eq!(decoded.message, expected);
            assert_eq!(decoded.message.encode().unwrap(), canonical);
        }
    }
    let canonical = wire(
        "00 11110001 0000 000111 0101 000101 000 0001 0 00110001 00110000 00110000 00110010 000000",
    );
    let expected = decode_prefix_in(&canonical, Direction::Reverse)
        .unwrap()
        .message;
    for reserved in 0..2 {
        let received = wire(&format!(
            "00 11110001 0000 000111 0101 000101 000 0001 {reserved:01b} 00110001 00110000 00110000 00110010 000000"
        ));
        let decoded = decode_prefix_in(&received, Direction::Reverse).unwrap();
        assert_eq!(decoded.message, expected);
        assert_eq!(decoded.message.encode().unwrap(), canonical);
    }
    let canonical = wire("00 11110001 0000 000101 0110 000011 0000 01 00 01000001 01000010 000000");
    let expected = decode_prefix_in(&canonical, Direction::Forward)
        .unwrap()
        .message;
    for reserved in 0..16 {
        let received = wire(&format!(
            "00 11110001 0000 000101 0110 000011 {reserved:04b} 01 00 01000001 01000010 000000"
        ));
        let decoded = decode_prefix_in(&received, Direction::Forward).unwrap();
        assert_eq!(decoded.message, expected);
        assert_eq!(decoded.message.encode().unwrap(), canonical);
    }
    let invalid = wire("00 11110001 0000 000101 0110 000011 1111 11 00 01000001 01000010 000000");
    assert!(matches!(
        decode_prefix_in(&invalid, Direction::Forward),
        Err(TrafficMessageError::Field("presentation_screening"))
    ));
}
