use super::*;
fn map(count: usize, received: &[u8]) -> AcknowledgmentMap {
    AcknowledgmentMap::try_new(
        (0..count)
            .map(|index| received.contains(&(index as u8)))
            .collect(),
    )
    .unwrap()
}
fn response() -> Response {
    Response {
        transaction_id: 5,
        status: Status::Continuing,
        acknowledgments: map(8, &[0, 2, 7]),
        optional_services_support: None,
    }
}
fn support() -> OptionalServicesSupport {
    OptionalServicesSupport {
        compression: true,
        encryption: true,
        acknowledgment_window: true,
        crc: true,
    }
}
#[test]
fn response_matches_fixed_octet_and_partial_map_vectors() {
    let mut value = response();
    let mut cases = vec![(value.clone(), vec![0x1d, 0, 8, 0x85])];
    value.status = Status::Received;
    value.acknowledgments = map(3, &[0, 2]);
    cases.push((value.clone(), vec![0x1d, 0x0a, 3, 0xa0]));
    value.status = Status::MultipleServicesUnsupported;
    value.optional_services_support = Some(support());
    cases.push((value.clone(), vec![0x1d, 0x0d, 3, 0xa2, 0x1e]));
    value.acknowledgments = map(8, &[0, 2, 7]);
    cases.push((value, vec![0x1d, 0x0d, 8, 0x85, 0x10, 0xf0]));
    for (value, wire) in cases {
        assert_eq!(value.encode(ProtocolVersion::Pv5).unwrap(), wire);
        let decoded = decode_response(&wire, ProtocolVersion::Pv5).unwrap();
        assert_eq!(decoded.response, value);
        assert!(decoded.unparsed_bits.is_empty());
        for cut in 0..wire.len() {
            if cut == 4 && wire.len() == 6 {
                assert!(decode_response(&wire[..cut], ProtocolVersion::Pv5).is_ok());
            } else {
                assert!(
                    decode_response(&wire[..cut], ProtocolVersion::Pv5).is_err(),
                    "cut={cut}, wire={wire:02x?}"
                );
            }
        }
    }
}
#[test]
fn every_nonzero_map_length_preserves_sequence_positions_and_missing_is_distinct_from_false() {
    for count in 1..=255 {
        let received: Vec<_> = (0..count).map(|index| index % 3 == 0).collect();
        let mut value = response();
        value.acknowledgments = AcknowledgmentMap::try_new(received.clone()).unwrap();
        let wire = value.encode(ProtocolVersion::Pv1).unwrap();
        let decoded = decode_response(&wire, ProtocolVersion::Pv1).unwrap();
        assert_eq!(decoded.response, value);
        assert!(decoded.unparsed_bits.is_empty());
        for (index, expected) in received.into_iter().enumerate() {
            assert_eq!(
                decoded.response.acknowledgments.received(index as u8),
                Some(expected)
            );
        }
        assert_eq!(decoded.response.acknowledgments.received(count as u8), None);
    }
    assert!(AcknowledgmentMap::try_new(vec![]).is_err());
    assert!(AcknowledgmentMap::try_new(vec![false; 257]).is_err());
    let mut value = response();
    value.acknowledgments = AcknowledgmentMap::try_new(vec![true; 256]).unwrap();
    assert_eq!(value.acknowledgments.received(255), Some(true));
    assert_eq!(
        value.encode(ProtocolVersion::Pv1),
        Err(Error::AmbiguousMapLength)
    );
    assert_eq!(
        decode_response(&[0x1d, 0, 0], ProtocolVersion::Pv1),
        Err(Error::AmbiguousMapLength)
    );
}
#[test]
fn status_values_support_map_version_and_reserved_fields_are_checked() {
    for raw in 0..=u8::MAX {
        let wire = [0x1d, raw, 1, 0x80];
        let decoded = decode_response(&wire, ProtocolVersion::Pv5);
        if raw <= 0x0d {
            assert_eq!(
                decoded
                    .unwrap()
                    .response
                    .encode(ProtocolVersion::Pv5)
                    .unwrap(),
                wire
            );
        } else {
            assert_eq!(decoded, Err(Error::Reserved));
        }
    }
    for raw in 0..=u8::MAX {
        let wire = [0x1d, 0x0d, 8, 0x85, 0x10 | (raw >> 4), (raw & 0xf) << 4];
        let decoded = decode_response(&wire, ProtocolVersion::Pv5);
        if raw <= 0xf {
            assert_eq!(
                decoded
                    .unwrap()
                    .response
                    .encode(ProtocolVersion::Pv5)
                    .unwrap(),
                wire
            );
        } else {
            assert_eq!(decoded, Err(Error::Reserved));
        }
    }
    let mut value = response();
    value.optional_services_support = Some(support());
    assert_eq!(
        value.encode(ProtocolVersion::Pv5),
        Err(Error::Field("optional_services_support_status"))
    );
    assert_eq!(
        decode_response(&[0x1d, 0, 8, 0x85, 0x10, 0xf0], ProtocolVersion::Pv5),
        Err(Error::Field("optional_services_support_status"))
    );
    value.status = Status::MultipleServicesUnsupported;
    assert_eq!(value.encode(ProtocolVersion::Pv4), Err(Error::Version));
    assert_eq!(
        decode_response(&[0x1d, 0x0d, 8, 0x85], ProtocolVersion::Pv1)
            .unwrap()
            .response
            .status,
        Status::MultipleServicesUnsupported
    );
    let mut without_support = value.clone();
    without_support.optional_services_support = None;
    assert_eq!(
        without_support.encode(ProtocolVersion::Pv1).unwrap(),
        [0x1d, 0x0d, 8, 0x85]
    );
    assert_eq!(
        decode_response(&[0x1d, 0x0d, 8, 0x85, 0x10, 0xf0], ProtocolVersion::Pv4),
        Err(Error::Version)
    );
    assert_eq!(
        response().encode(ProtocolVersion::Is136RevA),
        Err(Error::Version)
    );
    value.transaction_id = 8;
    assert_eq!(
        value.encode(ProtocolVersion::Pv5),
        Err(Error::Field("transaction_id"))
    );
}
#[test]
fn duplicate_support_padding_unknown_suffix_and_wrong_headers_do_not_become_clean_responses() {
    assert_eq!(
        decode_response(&[0x1d, 0x0d, 8, 0x85, 0x10, 0xf1, 0], ProtocolVersion::Pv5),
        Err(Error::DuplicateParameter)
    );
    assert_eq!(
        decode_response(&[0x1d, 0, 7, 1], ProtocolVersion::Pv1),
        Err(Error::Reserved)
    );
    let decoded = decode_response(&[0x1d, 0, 8, 0x85, 0xf1, 0x23], ProtocolVersion::Pv1).unwrap();
    assert_eq!(decoded.unparsed_bits.len(), 16);
    assert_eq!(decoded.response, response());
    assert_eq!(
        decode_response(&[0x5d, 0, 8, 0x85], ProtocolVersion::Pv1),
        Err(Error::Protocol)
    );
    assert_eq!(
        decode_response(&[0x15, 0, 8, 0x85], ProtocolVersion::Pv1),
        Err(Error::MessageType)
    );
}
#[test]
fn response_remains_a_segmented_teleservice_inside_both_transports() {
    use crate::l3::{
        legacy_traffic::{self, TrafficMessage, TrafficRelay},
        relay::{self, Direction, HigherLayerProtocol, RelayMessage, Teleservice},
    };
    let value = response();
    let message = RelayMessage::Data {
        transaction_id: 42,
        protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
        payload: value.encode(ProtocolVersion::Pv1).unwrap(),
    };
    for direction in [Direction::Forward, Direction::Reverse] {
        assert_eq!(
            relay::decode_prefix(&message.encode(direction).unwrap(), direction)
                .unwrap()
                .message,
            message
        );
        let traffic = TrafficMessage::Relay(TrafficRelay {
            message: message.clone(),
            optional_octets: vec![],
        });
        let decoded = legacy_traffic::decode_prefix_in(&traffic.encode().unwrap(), direction)
            .unwrap()
            .message;
        assert_eq!(decoded, traffic);
        let TrafficMessage::Relay(TrafficRelay {
            message: RelayMessage::Data { payload, .. },
            ..
        }) = decoded
        else {
            unreachable!()
        };
        assert_eq!(
            decode_response(&payload, ProtocolVersion::Pv1)
                .unwrap()
                .response,
            value
        );
    }
}
