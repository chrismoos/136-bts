use bts136::{
    link::{
        rach::{self, MobileIdentity, Outcome, RachReceiver},
        spach::{
            Channel, HeaderFlags,
            arq::{ArqDelivery, ArqHeader, ArqReassembler, DeliveryState, Reception, ResponseMode},
        },
    },
    phy::{
        rach::{self as phy_rach, BurstFormat},
        sync::SyncWord,
    },
};

#[test]
fn reverse_status_drives_exact_retransmissions_and_completes_delivery() {
    let identity = MobileIdentity::Imsi50(1);
    let message: Vec<u8> = (0..255).map(|value| value as u8).collect();
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let mut sender = ArqDelivery::try_new(
            ArqHeader {
                channel: Channel::Smsch,
                flags: HeaderFlags::default(),
                partial_echo: 42,
                polling: true,
                response_mode: ResponseMode::Reservation,
            },
            identity,
            &message,
        )
        .unwrap();
        let mut receiver = ArqReassembler::try_new(identity).unwrap();
        for (index, frame) in sender.frames().iter().enumerate() {
            if index == 2 || index == 7 {
                continue;
            }
            assert!(matches!(
                receiver.push(&frame.encode().unwrap()).unwrap(),
                Reception::Accepted { message: None, .. }
            ));
        }
        let status = receiver.status().unwrap();
        let information = rach::encode_arq_status(format, &status).unwrap();
        let bits = phy_rach::encode(format, SyncWord::Two, &information, 42).unwrap();
        let decoded = phy_rach::decode(&bits, format, 42).unwrap();
        let Outcome::ArqStatus(status) = RachReceiver::default()
            .push(format, &decoded.block.information)
            .unwrap()
        else {
            panic!("expected reverse status");
        };
        sender.apply_status(&status);
        let pending = sender.pending_frames();
        assert_eq!(pending, [&sender.frames()[2], &sender.frames()[7]]);
        let mut delivered = Vec::new();
        for frame in pending {
            if let Reception::Accepted {
                message: Some(message),
                ..
            } = receiver.push(&frame.encode().unwrap()).unwrap()
            {
                delivered.push(message.message);
            }
        }
        assert_eq!(delivered.as_slice(), std::slice::from_ref(&message));
        sender.apply_status(&receiver.status().unwrap());
        assert_eq!(sender.state(), DeliveryState::Complete);
        assert!(sender.pending_frames().is_empty());
    }
}
