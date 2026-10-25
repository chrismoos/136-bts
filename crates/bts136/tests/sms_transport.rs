use bts136::{
    l3::{
        address::{AddressInfo, AddressValue},
        relay::{
            Direction, HigherLayerProtocol, RelayMessage,
            addressing::{self, AddressedRelay, RelayAddresses},
        },
        sms::{self, DisplayTime, Metadata, SmsMessage, UserData, Validity},
    },
    link::{
        rach::{self, MobileIdentity, Outcome, RachReceiver},
        spach::{Channel, HeaderFlags, SingleMessage, SpachReceiver},
    },
    phy::{
        acquisition::decode_forward_control,
        control::{self, LogicalChannel, SharedFeedback},
        forward::ForwardModulator,
        rach::{self as phy_rach, BurstFormat},
        sync::SyncWord,
        traffic::{self, DATA_BITS, SACCH_BITS},
    },
};
fn number() -> AddressInfo {
    AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd("5551234".into()),
    }
}
fn metadata() -> Metadata {
    Metadata {
        message_reference: 42,
        privacy: 0,
        urgency: 1,
        delivery_acknowledgment: true,
        manual_acknowledgment: false,
        new_message: true,
    }
}
#[test]
fn sms_submit_and_delivery_ack_recover_inside_addressed_reverse_rdata() {
    for sms in [
        SmsMessage::Submit {
            metadata: metadata(),
            user_data: UserData::Ira(b"hello from mobile".to_vec()),
        },
        SmsMessage::DeliveryAck {
            message_reference: 42,
            user_data: UserData::Ira(Vec::new()),
        },
    ] {
        let relay = AddressedRelay {
            message: RelayMessage::Data {
                transaction_id: 19,
                protocol: HigherLayerProtocol::Sms,
                payload: sms.encode().unwrap(),
            },
            addresses: RelayAddresses {
                destination: Some(number()),
                ..RelayAddresses::default()
            },
        };
        let bytes = relay.encode(Direction::Reverse).unwrap();
        for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
            let frames = rach::encode(format, MobileIdentity::Min34(1), &[&bytes]).unwrap();
            let mut receiver = RachReceiver::default();
            let mut recovered = None;
            for frame in frames {
                let bits = phy_rach::encode(format, SyncWord::One, &frame, 42).unwrap();
                let block = phy_rach::decode(&bits, format, 42).unwrap();
                if let Outcome::Complete(set) =
                    receiver.push(format, &block.block.information).unwrap()
                {
                    recovered = Some(set.messages[0].clone());
                }
            }
            let decoded = addressing::decode(&recovered.unwrap(), Direction::Reverse).unwrap();
            assert_eq!(decoded.relay, relay);
            let RelayMessage::Data { payload, .. } = decoded.relay.message else {
                panic!("expected R-DATA");
            };
            assert_eq!(
                sms::decode_prefix(&payload, Direction::Reverse)
                    .unwrap()
                    .message,
                sms
            );
        }
    }
}
#[test]
fn addressed_sms_deliver_recovers_exact_text_from_generated_forward_iq() {
    let sms = SmsMessage::Deliver {
        metadata: metadata(),
        validity: Validity::Indefinite,
        display_time: DisplayTime::Default,
        user_data: UserData::Ira(b"hello from base station".repeat(3)),
    };
    let relay = AddressedRelay {
        message: RelayMessage::Data {
            transaction_id: 19,
            protocol: HigherLayerProtocol::Sms,
            payload: sms.encode().unwrap(),
        },
        addresses: RelayAddresses {
            originating: Some(number()),
            ..RelayAddresses::default()
        },
    };
    let message = SingleMessage {
        channel: Channel::Smsch,
        flags: HeaderFlags::default(),
        identity: MobileIdentity::Min34(1),
        message: relay.encode(Direction::Forward).unwrap(),
    };
    let frames = message.encode_fragmented().unwrap();
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for (phase, information) in frames.iter().enumerate() {
        samples.extend(
            transmitter.encode_slot(
                &control::encode_forward(
                    SyncWord::One,
                    phase as u8,
                    &SharedFeedback {
                        leading: [false; 12],
                        trailing: [false; 10],
                    },
                    information,
                    LogicalChannel::Spach,
                    42,
                )
                .unwrap(),
            ),
        );
        for word in [SyncWord::Two, SyncWord::Three] {
            samples.extend(
                transmitter.encode_slot(
                    &traffic::encode_forward(
                        word,
                        &[false; SACCH_BITS],
                        &[false; DATA_BITS],
                        42,
                        0,
                    )
                    .unwrap(),
                ),
            );
        }
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_control(
        &samples,
        transmitter.sample_rate_hz(),
        LogicalChannel::Spach,
        42,
    )
    .unwrap();
    assert_eq!(bursts.len(), frames.len());
    let mut receiver = SpachReceiver::default();
    let mut recovered = Vec::new();
    for burst in bursts {
        if let Some(message) = receiver.push(&burst.fields.block.information).unwrap() {
            recovered.push(message);
        }
    }
    assert_eq!(recovered, [message]);
    let decoded = addressing::decode(&recovered[0].message, Direction::Forward).unwrap();
    assert_eq!(decoded.relay, relay);
    let RelayMessage::Data { payload, .. } = decoded.relay.message else {
        panic!("expected R-DATA");
    };
    assert_eq!(
        sms::decode_prefix(&payload, Direction::Forward)
            .unwrap()
            .message,
        sms
    );
}
