use bts136::{
    link::rach::{self, MobileIdentity, Outcome, RachReceiver},
    phy::{
        rach::{self as phy_rach, BurstFormat},
        sync::SyncWord,
    },
};

#[test]
fn fragmented_access_messages_survive_slot_coding_and_repeated_frames() {
    let identity = MobileIdentity::Min34(0x123456789);
    let long: Vec<u8> = (0..30).collect();
    let messages: [&[u8]; 3] = [&long, &[0xa5, 0, 0xff, 0x11, 0x22], &[]];
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let frames = rach::encode(format, identity, &messages).unwrap();
        let mut receiver = RachReceiver::default();
        let mut recovered = None;
        for (index, frame) in frames.iter().enumerate() {
            let bits = phy_rach::encode(format, SyncWord::One, frame, 42).unwrap();
            let block = phy_rach::decode(&bits, format, 42).unwrap();
            let outcome = receiver.push(format, &block.block.information).unwrap();
            if index > 0 {
                assert_eq!(
                    receiver.push(format, &block.block.information).unwrap(),
                    Outcome::Duplicate(identity)
                );
            }
            if let Outcome::Complete(set) = outcome {
                recovered = Some(set);
            }
        }
        let recovered = recovered.unwrap();
        assert_eq!(recovered.identity, identity);
        assert_eq!(recovered.messages, messages);
        receiver.discontinuity();
        assert_eq!(
            receiver.push(format, &frames[0]).unwrap(),
            Outcome::Pending(identity)
        );
    }
}

#[test]
fn registration_and_serial_number_survive_both_access_formats() {
    use bts136::l3::{
        broadcast::ProtocolVersion,
        registration::{self, AccessMessage, RegistrationType},
    };
    let expected = [
        AccessMessage::Registration {
            registration_type: RegistrationType::PowerUp,
            scm: 0b10101,
            protocol_version: ProtocolVersion::Is136RevA,
        },
        AccessMessage::SerialNumber { esn: 0x12345678 },
    ];
    let messages: Vec<Vec<u8>> = expected
        .iter()
        .map(|message| message.encode().unwrap())
        .collect();
    let references: Vec<&[u8]> = messages.iter().map(Vec::as_slice).collect();
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let identity = MobileIdentity::Min34(0x123456789);
        let frames = rach::encode(format, identity, &references).unwrap();
        let mut receiver = RachReceiver::default();
        let mut recovered = None;
        for frame in frames {
            let bits = phy_rach::encode(format, SyncWord::Two, &frame, 42).unwrap();
            let decoded = phy_rach::decode(&bits, format, 42).unwrap();
            if let Outcome::Complete(set) =
                receiver.push(format, &decoded.block.information).unwrap()
            {
                recovered = Some(set);
            }
        }
        let set = recovered.unwrap();
        assert_eq!(set.identity, identity);
        assert_eq!(set.messages.len(), expected.len());
        for (octets, message) in set.messages.iter().zip(expected) {
            assert_eq!(
                registration::decode_prefix(octets).unwrap().message,
                message
            );
        }
    }
}

#[test]
fn hard_page_response_and_serial_number_reassemble_through_reverse_access() {
    use bts136::l3::{
        broadcast::ProtocolVersion,
        page_response::{self, PageResponse, ServiceCode},
        registration::AccessMessage,
    };
    use bts136::link::spach::paging::{PageBody, PageFrame, PagingFlags};
    let identity = MobileIdentity::Min34(0x123456789);
    let page = PageFrame {
        flags: PagingFlags::default(),
        body: PageBody::TripleMin34([None, Some(0x123456789), None]),
    };
    assert!(
        bts136::link::spach::paging::decode(&page.encode().unwrap())
            .unwrap()
            .addresses(identity)
    );
    let expected = PageResponse {
        protocol_version: ProtocolVersion::Is136RevA,
        last_try: false,
        scm: 0b10101,
        service_code: ServiceCode::DigitalSpeech,
    };
    let response = expected.encode().unwrap();
    let serial = AccessMessage::SerialNumber { esn: 0x12345678 }
        .encode()
        .unwrap();
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let frames = rach::encode(format, identity, &[&response, &serial]).unwrap();
        let mut receiver = RachReceiver::default();
        let mut recovered = None;
        for frame in frames {
            let bits = phy_rach::encode(format, SyncWord::One, &frame, 42).unwrap();
            let block = phy_rach::decode(&bits, format, 42).unwrap();
            if let Outcome::Complete(set) = receiver.push(format, &block.block.information).unwrap()
            {
                recovered = Some(set);
            }
        }
        let recovered = recovered.unwrap();
        assert_eq!(recovered.identity, identity);
        assert_eq!(recovered.messages, [response.clone(), serial.clone()]);
        assert_eq!(
            page_response::decode_prefix(&recovered.messages[0])
                .unwrap()
                .message,
            expected
        );
    }
}

#[test]
fn origination_called_number_survives_fragmented_reverse_access() {
    use bts136::l3::{
        address::{AddressInfo, AddressValue},
        broadcast::ProtocolVersion,
        origination::{self, Origination},
        page_response::ServiceCode,
    };
    let identity = MobileIdentity::Min34(0x123456789);
    let expected = Origination {
        protocol_version: ProtocolVersion::Is136RevA,
        emergency_call: false,
        called_party: AddressInfo {
            number_type: 0,
            numbering_plan: 1,
            address: AddressValue::Tbcd("120*#".repeat(12)),
        },
        last_try: false,
        scm: 0b10101,
        service_code: ServiceCode::DigitalSpeech,
    };
    let octets = expected.encode().unwrap();
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let frames = rach::encode(format, identity, &[&octets]).unwrap();
        assert!(frames.len() > 1);
        let mut receiver = RachReceiver::default();
        let mut recovered = None;
        for frame in frames {
            let bits = phy_rach::encode(format, SyncWord::One, &frame, 42).unwrap();
            let block = phy_rach::decode(&bits, format, 42).unwrap();
            if let Outcome::Complete(set) = receiver.push(format, &block.block.information).unwrap()
            {
                recovered = Some(set);
            }
        }
        let recovered = recovered.unwrap();
        assert_eq!(recovered.identity, identity);
        assert_eq!(recovered.messages.len(), 1);
        assert_eq!(
            origination::decode_prefix(&recovered.messages[0])
                .unwrap()
                .message,
            expected
        );
    }
}
