use super::*;
fn vectors() -> Vec<(Segment, Vec<u8>, Vec<usize>)> {
    vec![
        (
            Segment {
                transaction_id: 5,
                sequence_number: 0,
                segment_data: vec![0xaa, 0x55],
                kind: SegmentKind::Begin {
                    message_length: Some(0x123),
                    services: Some(OptionalServices {
                        compression: Compression::V42Bis,
                        acknowledgment_window: AcknowledgmentWindow::Every8,
                        crc_status: CrcStatus::Crc16,
                    }),
                },
            },
            vec![0x05, 0, 2, 0xaa, 0x55, 0x11, 0x23, 0x22, 0x12],
            vec![5, 7],
        ),
        (
            Segment {
                transaction_id: 5,
                sequence_number: 1,
                segment_data: vec![0xcc],
                kind: SegmentKind::Continue {
                    message_length: Some(0x123),
                },
            },
            vec![0x0d, 1, 1, 0xcc, 0x11, 0x23],
            vec![4],
        ),
        (
            Segment {
                transaction_id: 5,
                sequence_number: 2,
                segment_data: vec![0xdd],
                kind: SegmentKind::End {
                    message_length: Some(0x123),
                    crc: Some(MessageCrc::Crc16(0xbeef)),
                },
            },
            vec![0x15, 2, 1, 0xdd, 0x11, 0x23, 0x2b, 0xee, 0xf0],
            vec![4, 6],
        ),
        (
            Segment {
                transaction_id: 5,
                sequence_number: 2,
                segment_data: vec![0xdd],
                kind: SegmentKind::End {
                    message_length: None,
                    crc: Some(MessageCrc::Crc24(0xabcdef)),
                },
            },
            vec![0x15, 2, 1, 0xdd, 0x3a, 0xbc, 0xde, 0xf0],
            vec![4],
        ),
    ]
}
#[test]
fn segment_formats_match_fixed_octet_vectors_and_all_prefix_boundaries() {
    for (segment, wire, complete_prefixes) in vectors() {
        assert_eq!(segment.encode(ProtocolVersion::Pv1).unwrap(), wire);
        assert_eq!(
            decode_segment(&wire, ProtocolVersion::Pv1).unwrap(),
            DecodedSegment {
                segment,
                unparsed_octets: vec![]
            }
        );
        for cut in 0..wire.len() {
            let result = decode_segment(&wire[..cut], ProtocolVersion::Pv1);
            if complete_prefixes.contains(&cut) {
                assert!(result.unwrap().unparsed_octets.is_empty());
            } else {
                assert_eq!(result, Err(Error::Truncated), "cut={cut}, wire={wire:02x?}");
            }
        }
    }
}
#[test]
fn bounds_and_protocol_version_are_validated_without_transport_specific_size_assumptions() {
    let mut segment = Segment {
        transaction_id: 7,
        sequence_number: 255,
        segment_data: vec![0xff; 255],
        kind: SegmentKind::End {
            message_length: Some(4095),
            crc: Some(MessageCrc::Crc24(0xffffff)),
        },
    };
    let wire = segment.encode(ProtocolVersion::Pv7).unwrap();
    assert_eq!(
        decode_segment(&wire, ProtocolVersion::Pv7).unwrap().segment,
        segment
    );
    assert_eq!(
        segment.encode(ProtocolVersion::Is136RevA),
        Err(Error::Version)
    );
    assert_eq!(
        decode_segment(&wire, ProtocolVersion::Is136RevA),
        Err(Error::Version)
    );
    segment.transaction_id = 8;
    assert_eq!(
        segment.encode(ProtocolVersion::Pv1),
        Err(Error::Field("transaction_id"))
    );
    segment.transaction_id = 7;
    segment.segment_data.push(0);
    assert_eq!(
        segment.encode(ProtocolVersion::Pv1),
        Err(Error::Field("segment_data_length"))
    );
    segment.segment_data.clear();
    segment.kind = SegmentKind::Continue {
        message_length: Some(4096),
    };
    assert_eq!(
        segment.encode(ProtocolVersion::Pv1),
        Err(Error::Field("message_length"))
    );
    segment.kind = SegmentKind::End {
        message_length: None,
        crc: Some(MessageCrc::Crc24(0x1000000)),
    };
    assert_eq!(
        segment.encode(ProtocolVersion::Pv1),
        Err(Error::Field("crc24"))
    );
    segment.kind = SegmentKind::Begin {
        message_length: None,
        services: None,
    };
    assert_eq!(
        segment.encode(ProtocolVersion::Pv1),
        Err(Error::Field("begin_sequence_number"))
    );
    assert_eq!(
        decode_segment(&[0, 1, 0], ProtocolVersion::Pv1),
        Err(Error::Field("begin_sequence_number"))
    );
}
#[test]
fn optional_services_accept_only_defined_selections_and_preserve_absence() {
    assert_eq!(
        decode_segment(&[0, 0, 0], ProtocolVersion::Pv1)
            .unwrap()
            .segment
            .kind,
        SegmentKind::Begin {
            message_length: None,
            services: None
        }
    );
    assert_eq!(
        OptionalServices::default(),
        OptionalServices {
            compression: Compression::None,
            acknowledgment_window: AcknowledgmentWindow::Prohibited,
            crc_status: CrcStatus::None
        }
    );
    for raw in 0..=0xfff_u16 {
        let bytes = (0x2000 | raw).to_be_bytes();
        let wire = [0, 0, 0, bytes[0], bytes[1]];
        let valid = raw & 1 == 0
            && ((raw >> 6) & 7) == 0
            && ((raw >> 9) & 7) <= 1
            && ((raw >> 3) & 7) <= 5
            && ((raw >> 1) & 3) <= 2;
        let result = decode_segment(&wire, ProtocolVersion::Pv1);
        if valid {
            assert_eq!(
                result
                    .unwrap()
                    .segment
                    .encode(ProtocolVersion::Pv1)
                    .unwrap(),
                wire
            );
        } else {
            assert_eq!(result, Err(Error::Reserved));
        }
    }
}
#[test]
fn duplicate_options_conflicting_crcs_and_crc_reserved_bits_are_rejected() {
    for wire in [
        vec![0, 0, 0, 0x10, 0, 0x10, 0],
        vec![0, 0, 0, 0x20, 0, 0x20, 0],
        vec![0x10, 1, 0, 0x20, 0, 0, 0x30, 0, 0, 0],
        vec![0x10, 1, 0, 0x30, 0, 0, 0, 0x20, 0, 0],
    ] {
        assert_eq!(
            decode_segment(&wire, ProtocolVersion::Pv1),
            Err(Error::DuplicateParameter)
        );
    }
    for wire in [
        vec![0x10, 1, 0, 0x20, 0, 1],
        vec![0x10, 1, 0, 0x30, 0, 0, 1],
    ] {
        assert_eq!(
            decode_segment(&wire, ProtocolVersion::Pv1),
            Err(Error::Reserved)
        );
    }
}
#[test]
fn unknown_optional_suffixes_are_not_silently_discarded_and_response_is_explicitly_unsupported() {
    let wire = [0, 0, 0, 0x10, 1, 0xf1, 0x20, 0];
    let decoded = decode_segment(&wire, ProtocolVersion::Pv1).unwrap();
    assert_eq!(decoded.unparsed_octets, [0xf1, 0x20, 0]);
    assert_eq!(
        decoded.segment.encode(ProtocolVersion::Pv1).unwrap(),
        wire[..5]
    );
    assert_eq!(
        decode_segment(&[0x18, 0, 0], ProtocolVersion::Pv1),
        Err(Error::UnsupportedResponse)
    );
    assert_eq!(
        decode_segment(&[0x20, 0, 0], ProtocolVersion::Pv1),
        Err(Error::MessageType)
    );
    assert_eq!(
        decode_segment(&[0x40, 0, 0], ProtocolVersion::Pv1),
        Err(Error::Protocol)
    );
}

#[test]
fn segments_keep_their_tsar_identity_through_dcch_and_dtc_encapsulation() {
    use crate::l3::{
        legacy_traffic::{self, TrafficMessage, TrafficRelay},
        relay::{self, Direction, HigherLayerProtocol, RelayMessage, Teleservice},
    };
    for (segment, _, _) in vectors() {
        let payload = segment.encode(ProtocolVersion::Pv1).unwrap();
        let message = RelayMessage::Data {
            transaction_id: 42,
            protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
            payload,
        };
        for direction in [Direction::Forward, Direction::Reverse] {
            let wire = message.encode(direction).unwrap();
            assert_eq!(
                relay::decode_prefix(&wire, direction).unwrap().message,
                message
            );
            let traffic = TrafficMessage::Relay(TrafficRelay {
                message: message.clone(),
                optional_octets: vec![],
            });
            let decoded = legacy_traffic::decode_prefix_in_version(
                &traffic.encode().unwrap(),
                direction,
                ProtocolVersion::Pv1,
            )
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
                decode_segment(&payload, ProtocolVersion::Pv1)
                    .unwrap()
                    .segment,
                segment
            );
        }
    }
}
