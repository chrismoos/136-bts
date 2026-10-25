use super::*;
use crate::link::traffic::{self, TrafficMessageReceiver};

fn common(version: ProtocolVersion) -> CapabilityFields {
    CapabilityFields {
        protocol_version: version,
        voice_privacy_map: 5,
        voice_coder_map: 33,
        encryption_key_map: 3,
        facch_sacch_arq: true,
        soc: 0xabc,
        bsmc: 0x5a,
        sms_map: 3,
        ira: true,
        encryption_algorithms: [Some(5), None, None, None, None, None, None, Some(10)],
    }
}

fn station(version: ProtocolVersion) -> BaseStationCapabilities {
    BaseStationCapabilities {
        common: common(version),
        pv1: at_least(version, ProtocolVersion::Pv1).then_some(StationPv1 {
            async_data: true,
            dads: true,
            oats: true,
            characters: CharacterSupport {
                latin1: true,
                bmp: false,
                latin_hebrew: true,
            },
            ..StationPv1::default()
        }),
        extended_bands: at_least(version, ProtocolVersion::Pv5).then_some(0x91),
        extended_characters: at_least(version, ProtocolVersion::Pv6)
            .then_some(std::array::from_fn(|index| index % 2 == 0)),
    }
}

fn mobile(version: ProtocolVersion) -> MobileCapabilities {
    MobileCapabilities {
        common: common(version),
        supported_bands: 0x91,
        analog_800_speech: true,
        pv1: at_least(version, ProtocolVersion::Pv1).then_some(MobilePv1 {
            scm: 0x15,
            software_vintage: 0x25,
            firmware_vintage: 0x12,
            model: 0xab,
            manufacturer: 0xcd,
            max_supported_pfc: 5,
            async_data: true,
            dads: true,
            subaddress: Some(Subaddress {
                odd: true,
                kind: 2,
                address: vec![0, 0x0e, 0xff],
            }),
            user_group: true,
            double_rate_dtc: true,
            stu_iii: true,
            characters: CharacterSupport {
                latin1: false,
                bmp: true,
                latin_hebrew: true,
            },
            ..MobilePv1::default()
        }),
        advice: at_least(version, ProtocolVersion::Pv4).then_some(true),
        pv5: at_least(version, ProtocolVersion::Pv5).then_some(MobilePv5 {
            positioning_map: 0xa5,
            extended_bands: 0x81,
            modulation_map: 0x3c,
            interleaving_map: 0xe7,
        }),
        extended_characters: at_least(version, ProtocolVersion::Pv6)
            .then_some(std::array::from_fn(|index| index % 3 == 0)),
    }
}

fn literal(value: &str) -> Vec<bool> {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .map(|character| match character {
            '0' => false,
            '1' => true,
            _ => panic!("nonbinary golden vector"),
        })
        .collect()
}

const VERSIONS: [ProtocolVersion; 11] = [
    ProtocolVersion::Eia553OrIs54A,
    ProtocolVersion::Tia627,
    ProtocolVersion::Is136Rev0,
    ProtocolVersion::Is136RevA,
    ProtocolVersion::Pv1,
    ProtocolVersion::Pv2,
    ProtocolVersion::Pv3,
    ProtocolVersion::Pv4,
    ProtocolVersion::Pv5,
    ProtocolVersion::Pv6,
    ProtocolVersion::Pv7,
];

#[test]
fn baseline_capability_response_direction_has_distinct_fixed_wire_vectors() {
    // TIA-136-133-E §§5.1.2.2.23 and 9.1.2.2.23 place the mobile-only nine bits before Domain Map.
    let cases = [
        (
            Direction::Forward,
            CapabilityResponse::BaseStation(station(ProtocolVersion::Is136RevA)),
            literal(
                "00 00001010 0100 0101 100001 0011 1 101010111100 01011010 11 1 10000001 1010 0101",
            ),
        ),
        (
            Direction::Reverse,
            CapabilityResponse::Mobile(mobile(ProtocolVersion::Is136RevA)),
            literal(
                "00 00001010 0100 0101 100001 0011 1 101010111100 01011010 11 1 10010001 1 10000001 1010 0101",
            ),
        ),
    ];
    for (direction, response, expected) in cases {
        let message = TrafficMessage::CapabilityUpdateResponse(response);
        assert_eq!(message.encode().unwrap(), expected);
        let mut padded = expected.clone();
        padded.extend([true; 48]);
        let decoded = decode_prefix_in(&padded, direction).unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.consumed_bits, expected.len());
    }
}

#[test]
fn capability_request_has_no_remaining_length_and_rejects_reserved_versions() {
    let expected = literal("00 00000100 0100");
    assert_eq!(
        TrafficMessage::CapabilityUpdateRequest(ProtocolVersion::Is136RevA)
            .encode()
            .unwrap(),
        expected
    );
    for version in VERSIONS {
        let message = TrafficMessage::CapabilityUpdateRequest(version);
        for direction in [Direction::Forward, Direction::Reverse] {
            let bits = message.encode().unwrap();
            assert_eq!(bits.len(), 14);
            assert_eq!(decode_prefix_in(&bits, direction).unwrap().message, message);
        }
    }
    for code in [3, 12, 13, 14, 15] {
        let mut bits = literal("00 00000100");
        put(&mut bits, code, PV_BITS);
        assert_eq!(
            decode_prefix(&bits),
            Err(TrafficMessageError::Field("protocol_version"))
        );
    }
}

#[test]
fn capability_extensions_obey_each_protocol_version_boundary_and_every_truncation() {
    for version in VERSIONS {
        for (direction, response) in [
            (
                Direction::Forward,
                CapabilityResponse::BaseStation(station(version)),
            ),
            (
                Direction::Reverse,
                CapabilityResponse::Mobile(mobile(version)),
            ),
        ] {
            let message = TrafficMessage::CapabilityUpdateResponse(response);
            let bits = message.encode().unwrap();
            assert_eq!(decode_prefix_in(&bits, direction).unwrap().message, message);
            for length in 0..bits.len() {
                assert!(
                    matches!(
                        decode_prefix_in(&bits[..length], direction),
                        Err(TrafficMessageError::Truncated { .. })
                    ),
                    "{version:?} {direction:?} length={length}"
                );
            }
            let expected_length = match direction {
                Direction::Forward => {
                    68 + usize::from(at_least(version, ProtocolVersion::Pv1)) * 8
                        + usize::from(at_least(version, ProtocolVersion::Pv5)) * 8
                        + usize::from(at_least(version, ProtocolVersion::Pv6)) * 13
                }
                Direction::Reverse => {
                    77 + usize::from(at_least(version, ProtocolVersion::Pv1)) * 88
                        + usize::from(at_least(version, ProtocolVersion::Pv4))
                        + usize::from(at_least(version, ProtocolVersion::Pv5)) * 32
                        + usize::from(at_least(version, ProtocolVersion::Pv6)) * 13
                }
            };
            assert_eq!(bits.len(), expected_length, "{version:?} {direction:?}");
        }
    }
}

#[test]
fn capability_word_reassembly_uses_direction_and_recovers_after_discontinuity() {
    for (direction, response) in [
        (
            Direction::Forward,
            CapabilityResponse::BaseStation(station(ProtocolVersion::Pv7)),
        ),
        (
            Direction::Reverse,
            CapabilityResponse::Mobile(mobile(ProtocolVersion::Pv7)),
        ),
    ] {
        let message = TrafficMessage::CapabilityUpdateResponse(response);
        let words = traffic::encode(&message).unwrap();
        let mut receiver = TrafficMessageReceiver::new(direction);
        assert_eq!(receiver.push(&words[0]).unwrap(), None);
        receiver.discontinuity();
        for word in &words[1..] {
            assert_eq!(receiver.push(word).unwrap(), None);
        }
        let mut decoded = Vec::new();
        for word in &words {
            if let Some(value) = receiver.push(word).unwrap() {
                decoded.push(value);
            }
        }
        assert_eq!(decoded, [message]);
    }
}

#[test]
fn capability_algorithm_domains_preserve_all_maps_in_msb_order() {
    let mut response = station(ProtocolVersion::Is136RevA);
    response.common.encryption_algorithms = std::array::from_fn(|domain| Some(domain as u8));
    let message =
        TrafficMessage::CapabilityUpdateResponse(CapabilityResponse::BaseStation(response));
    let bits = message.encode().unwrap();
    assert_eq!(
        &bits[52..],
        literal("11111111 0111 0110 0101 0100 0011 0010 0001 0000")
    );
    assert_eq!(
        decode_prefix_in(&bits, Direction::Forward).unwrap().message,
        message
    );
}

#[test]
fn capability_revision_groups_and_field_widths_are_validated() {
    let encode = |station| {
        TrafficMessage::CapabilityUpdateResponse(CapabilityResponse::BaseStation(station)).encode()
    };
    let mut response = station(ProtocolVersion::Is136RevA);
    response.pv1 = Some(StationPv1::default());
    assert!(encode(response).is_err());
    let mut response = station(ProtocolVersion::Pv7);
    response.extended_characters = None;
    assert!(encode(response).is_err());
    let mut response = station(ProtocolVersion::Pv5);
    response.extended_bands = None;
    assert!(encode(response).is_err());
    for field in ["privacy", "coder", "key", "soc", "sms", "algorithm"] {
        let mut response = station(ProtocolVersion::Is136RevA);
        match field {
            "privacy" => response.common.voice_privacy_map = 16,
            "coder" => response.common.voice_coder_map = 64,
            "key" => response.common.encryption_key_map = 16,
            "soc" => response.common.soc = 4096,
            "sms" => response.common.sms_map = 4,
            "algorithm" => response.common.encryption_algorithms[0] = Some(16),
            _ => unreachable!(),
        }
        assert!(encode(response).is_err(), "{field}");
    }
}

#[test]
fn capability_subaddress_minimum_maximum_and_absence_keep_extensions_aligned() {
    for address in [None, Some(Vec::new()), Some(vec![0x0e; 20])] {
        let mut response = mobile(ProtocolVersion::Pv7);
        response.pv1.as_mut().unwrap().subaddress = address.map(|address| Subaddress {
            address,
            ..Subaddress::default()
        });
        let message =
            TrafficMessage::CapabilityUpdateResponse(CapabilityResponse::Mobile(response));
        let bits = message.encode().unwrap();
        assert_eq!(decode_prefix(&bits).unwrap().message, message);
    }
    let mut response = mobile(ProtocolVersion::Pv7);
    response
        .pv1
        .as_mut()
        .unwrap()
        .subaddress
        .as_mut()
        .unwrap()
        .address = vec![0; 21];
    assert!(
        TrafficMessage::CapabilityUpdateResponse(CapabilityResponse::Mobile(response))
            .encode()
            .is_err()
    );
}

#[test]
fn malformed_subaddress_length_does_not_wait_for_nonexistent_continuations() {
    let message = TrafficMessage::CapabilityUpdateResponse(CapabilityResponse::Mobile(mobile(
        ProtocolVersion::Pv7,
    )));
    let mut bits = message.encode().unwrap();
    // The baseline response is 77 bits and PV1 has 36 identity bits plus four flags before N.
    const SUBADDRESS_LENGTH_OFFSET: usize = 117;
    for length in [0u64, 22, 255] {
        let mut encoded_length = Vec::new();
        put(&mut encoded_length, length, OCTET_BITS);
        bits[SUBADDRESS_LENGTH_OFFSET..SUBADDRESS_LENGTH_OFFSET + OCTET_BITS]
            .copy_from_slice(&encoded_length);
        assert_eq!(
            decode_prefix(&bits),
            Err(TrafficMessageError::Field("capability_subaddress_length"))
        );
    }
}

#[test]
fn pv7_capability_responses_match_fixed_extension_field_vectors() {
    let cases = [
        (
            Direction::Forward,
            CapabilityResponse::BaseStation(station(ProtocolVersion::Pv7)),
            literal(
                "00 00001010 1011 0101 100001 0011 1 101010111100 01011010 11 1
             10000001 1010 0101 10101 101 10010001 1010101010101",
            ),
        ),
        (
            Direction::Reverse,
            CapabilityResponse::Mobile(mobile(ProtocolVersion::Pv7)),
            literal(
                "00 00001010 1011 0101 100001 0011 1 101010111100 01011010 11 1
             10010001 1 10000001 1010 0101
             10101 100101 010010 10101011 11001101 101 1011
             00000100 000 1 010 0 00000000 00001110 11111111 10101 011
             1 10100101 10000001 00111100 11100111 1001001001001",
            ),
        ),
    ];
    for (direction, response, expected) in cases {
        let message = TrafficMessage::CapabilityUpdateResponse(response);
        assert_eq!(message.encode().unwrap(), expected);
        assert_eq!(
            decode_prefix_in(&expected, direction).unwrap().message,
            message
        );
    }
}
