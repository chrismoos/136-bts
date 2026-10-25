use super::*;
use crate::{
    l3::legacy_traffic::{self, TrafficMessage, TrafficMessageError, TrafficRelay},
    link::traffic::{SignalingWord, TrafficMessageReceiver},
};
#[test]
fn standardized_hlpi_values_preserve_the_tsar_bit_and_teleservice_identity() {
    for (raw, plain, service) in [
        (
            0x00,
            HigherLayerProtocol::NetworkSpecific,
            Teleservice::NetworkSpecific,
        ),
        (0x01, HigherLayerProtocol::Sms, Teleservice::Sms),
        (0x03, HigherLayerProtocol::Oats, Teleservice::Oats),
        (0x04, HigherLayerProtocol::Opts, Teleservice::Opts),
        (0x05, HigherLayerProtocol::Guts, Teleservice::Guts),
        (
            0x06,
            HigherLayerProtocol::ChargeIndication,
            Teleservice::ChargeIndication,
        ),
        (0x07, HigherLayerProtocol::Ghost, Teleservice::Ghost),
        (0x08, HigherLayerProtocol::Samps, Teleservice::Samps),
    ] {
        assert_eq!(HigherLayerProtocol::decode(raw).unwrap(), plain);
        assert_eq!(plain.code().unwrap(), raw);
        let segmented = HigherLayerProtocol::Segmented(service);
        assert_eq!(HigherLayerProtocol::decode(raw | 0x40).unwrap(), segmented);
        assert_eq!(segmented.code().unwrap(), raw | 0x40);
    }
    assert_eq!(
        HigherLayerProtocol::CarrierSpecific(0x41).code(),
        Err(RelayError::ProtocolIdentifier)
    );
}
#[test]
fn every_hlpi_octet_is_either_reserved_or_losslessly_preserved_on_both_dcch_directions() {
    let mut accepted = 0;
    for raw in 0..=u8::MAX {
        let valid = raw >= 0x80 || matches!(raw & 0x3f, 0 | 1 | 3..=8);
        for (direction, kind) in [(Direction::Forward, 26), (Direction::Reverse, 9)] {
            let bytes = [kind, 42, 2, raw, 0xaa];
            let decoded = decode_prefix(&bytes, direction);
            if valid {
                let decoded = decoded.unwrap();
                assert_eq!(decoded.consumed_octets, bytes.len());
                assert!(decoded.optional_octets.is_empty());
                assert_eq!(decoded.message.encode(direction).unwrap(), bytes);
                if raw >= 0x80 {
                    assert!(
                        matches!(decoded.message, RelayMessage::Data { protocol: HigherLayerProtocol::CarrierSpecific(code), .. } if code == raw)
                    );
                }
            } else {
                assert_eq!(decoded, Err(RelayError::ProtocolIdentifier));
            }
        }
        accepted += usize::from(valid);
    }
    assert_eq!(accepted, 144);
}
#[test]
fn segmented_sms_matches_the_fixed_dtc_vector_and_survives_receiver_recovery() {
    let expected: Vec<_> = "00 00001011 00101010 00000010 01000001 10101010 000000"
        .chars()
        .filter(|bit| *bit != ' ')
        .map(|bit| bit == '1')
        .collect();
    let message = TrafficMessage::Relay(TrafficRelay {
        message: RelayMessage::Data {
            transaction_id: 42,
            protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
            payload: vec![0xaa],
        },
        optional_octets: vec![],
    });
    assert_eq!(message.encode().unwrap(), expected);
    for direction in [Direction::Forward, Direction::Reverse] {
        assert_eq!(
            legacy_traffic::decode_prefix_in(&expected, direction)
                .unwrap()
                .message,
            message
        );
    }
    let mut invalid = expected.clone();
    const HLPI_START: usize = 26;
    const HLPI_END: usize = HLPI_START + u8::BITS as usize;
    invalid[HLPI_START..HLPI_END]
        .copy_from_slice(&[false, false, false, false, false, false, true, false]);
    let mut receiver = TrafficMessageReceiver::default();
    assert!(matches!(
        receiver.push(&SignalingWord {
            continuation: false,
            contents: invalid.try_into().unwrap()
        }),
        Err(crate::link::traffic::TrafficLinkError::Message(
            TrafficMessageError::Relay(RelayError::ProtocolIdentifier)
        ))
    ));
    assert_eq!(
        receiver
            .push(&SignalingWord {
                continuation: true,
                contents: expected.clone().try_into().unwrap()
            })
            .unwrap(),
        None
    );
    assert_eq!(
        receiver
            .push(&SignalingWord {
                continuation: false,
                contents: expected.try_into().unwrap()
            })
            .unwrap(),
        Some(message)
    );
}
