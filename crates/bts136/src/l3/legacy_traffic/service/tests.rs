use super::*;
use crate::l3::legacy_traffic::{TrafficMessage, decode_prefix_in};
fn wire(text: &str) -> Vec<bool> {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c == '1')
        .collect()
}
#[test]
fn service_fixed_vectors_include_service_code_in_remaining_octets() {
    for (message, direction, text) in [
        (
            ServiceMessage::Request {
                request_number: 15,
                service_code: 1,
                options: ServiceOptions::default(),
            },
            Direction::Reverse,
            "00 11110011 1111 000001 0001 0000",
        ),
        (
            ServiceMessage::Request {
                request_number: 0,
                service_code: 1,
                options: ServiceOptions {
                    bandwidth: Some(vec![1]),
                    voice_mode: Some(vec![8]),
                    ..Default::default()
                },
            },
            Direction::Reverse,
            "00 11110011 0000 000101 0001 0001 000001 001 0010 000001 001000 0000000",
        ),
        (
            ServiceMessage::Response {
                request_number: 3,
                service_code: 6,
                causes: Some(vec![25]),
                unparsed_bits: vec![],
            },
            Direction::Forward,
            "00 10100011 0011 000011 0110 0001 000001 00011001 00",
        ),
        (
            ServiceMessage::Response {
                request_number: 4,
                service_code: 1,
                causes: None,
                unparsed_bits: vec![],
            },
            Direction::Forward,
            "00 10100011 0100 000001 0001 0000",
        ),
    ] {
        let message = TrafficMessage::Service(message);
        let bits = wire(text);
        assert_eq!(message.encode().unwrap(), bits);
        let decoded = decode_prefix_in(&bits, direction).unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.consumed_bits, bits.len());
        for end in 0..bits.len() {
            assert!(decode_prefix_in(&bits[..end], direction).is_err());
        }
        let opposite = if direction == Direction::Forward {
            Direction::Reverse
        } else {
            Direction::Forward
        };
        assert!(decode_prefix_in(&bits, opposite).is_err());
    }
}
#[test]
fn service_rejects_malformed_options_and_retains_unknown_tail() {
    for text in [
        "00 11110011 0000 000000",
        "00 11110011 0000 000001 00010001",
        "00 11110011 0000 000010 0001 0001 000010 00",
        "00 11110011 0000 000010 0001 0001 000001 00",
    ] {
        assert!(decode_prefix_in(&wire(text), Direction::Reverse).is_err());
    }
    let message = TrafficMessage::Service(ServiceMessage::Request {
        request_number: 1,
        service_code: 8,
        options: ServiceOptions {
            unparsed_bits: wire("0110 1111 0011"),
            ..Default::default()
        },
    });
    let bits = message.encode().unwrap();
    let decoded = decode_prefix_in(&bits, Direction::Reverse).unwrap();
    assert_eq!(decoded.message.encode().unwrap(), bits);
    let TrafficMessage::Service(ServiceMessage::Request { options, .. }) = decoded.message else {
        panic!()
    };
    assert_eq!(options.unparsed_bits, wire("0110 1111 0011"));
}
#[test]
fn service_multiple_causes_reassemble_only_with_every_signaling_word() {
    use crate::link::traffic::{TrafficMessageReceiver, encode};
    let message = TrafficMessage::Service(ServiceMessage::Response {
        request_number: 15,
        service_code: REJECTED,
        causes: Some(vec![ServiceRejection::UnsupportedService as u8; 10]),
        unparsed_bits: vec![],
    });
    let words = encode(&message).unwrap();
    assert_eq!(words.len(), 3);
    let mut receiver = TrafficMessageReceiver::new(Direction::Forward);
    let mut decoded = None;
    for word in &words {
        if let Some(value) = receiver.push(word).unwrap() {
            assert!(decoded.is_none());
            decoded = Some(value);
        }
    }
    assert_eq!(decoded, Some(message.clone()));
    for lost in 0..words.len() {
        let mut receiver = TrafficMessageReceiver::new(Direction::Forward);
        for (index, word) in words.iter().enumerate() {
            if index == lost {
                receiver.discontinuity();
            } else {
                assert!(receiver.push(word).unwrap().is_none());
            }
        }
    }
    for request in [
        ServiceMessage::Response {
            request_number: 16,
            service_code: REJECTED,
            causes: None,
            unparsed_bits: vec![],
        },
        ServiceMessage::Response {
            request_number: 0,
            service_code: REJECTED,
            causes: Some(vec![7; 11]),
            unparsed_bits: vec![],
        },
        ServiceMessage::Request {
            request_number: 0,
            service_code: 16,
            options: Default::default(),
        },
    ] {
        assert!(TrafficMessage::Service(request).encode().is_err());
    }
}
