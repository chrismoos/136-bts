use bts136::{
    core::registry::{LocalRegistry, ProvisionedSubscriber},
    l3::{
        broadcast::ProtocolVersion,
        registration::{self, AccessMessage, RegistrationResponse, RegistrationType},
    },
    link::{
        rach::{self, MobileIdentity, Outcome, RachReceiver},
        spach::{Channel, HeaderFlags, SingleMessage, SpachReceiver},
    },
    phy::{
        control::{self, LogicalChannel, SharedFeedback},
        rach::{self as phy_rach, BurstFormat},
        sync::SyncWord,
    },
};
#[test]
fn registration_request_updates_core_and_accept_reply_survives_addressed_arch() {
    let identity = MobileIdentity::Imsi50(1);
    let request = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 21,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
            identity,
            number: "1001".into(),
        }])
        .unwrap();
        let mut receiver = RachReceiver::default();
        let mut response = None;
        for frame in rach::encode(format, identity, &[&request]).unwrap() {
            let bits = phy_rach::encode(format, SyncWord::One, &frame, 42).unwrap();
            let block = phy_rach::decode(&bits, format, 42).unwrap();
            if let Outcome::Complete(set) = receiver.push(format, &block.block.information).unwrap()
            {
                response = Some(registry.register(&set, 100).unwrap());
            }
        }
        assert!(registry.by_number("1001").unwrap().registration.is_some());
        let reply = response.unwrap();
        assert_eq!(reply.identity, identity);
        let message = SingleMessage {
            channel: Channel::Arch,
            flags: HeaderFlags::default(),
            identity,
            message: reply.message.encode(),
        };
        let mut receiver = SpachReceiver::default();
        let mut recovered = None;
        for (phase, information) in message.encode_fragmented().unwrap().iter().enumerate() {
            let bits = control::encode_forward(
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
            .unwrap();
            let block = control::decode_forward(&bits, LogicalChannel::Spach, 42).unwrap();
            if let Some(message) = receiver.push(&block.block.information).unwrap() {
                recovered = Some(message);
            }
        }
        let recovered = recovered.unwrap();
        assert_eq!(recovered.identity, identity);
        assert_eq!(
            registration::RegistrationResponse::decode_prefix(&recovered.message)
                .unwrap()
                .message,
            RegistrationResponse::Accept
        );
    }
}
