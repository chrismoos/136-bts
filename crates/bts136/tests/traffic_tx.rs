use bts136::{
    codec::vselp::{VselpParameters, decoder::Decoder},
    l3::{legacy_traffic::TrafficMessage, relay::Direction},
    link::{
        rach::MobileIdentity,
        traffic::{SignalingWord, TrafficMessageReceiver},
        traffic_order::{OrderKind, OrderPolicy, OrderState, TrafficOrderExchange},
        traffic_tx::TrafficTx,
        voice::{FrameKind, TrafficFrameReceiver, TransmitFrame},
    },
    phy::{
        acquisition::decode_forward_traffic, forward::ForwardModulator, speech::CrcStatus,
        sync::SyncWord, traffic,
    },
};
const DVCC: u8 = 42;
const SAMPLES_PER_SYMBOL: usize = 8;
#[test]
fn scheduled_facch_starts_ack_wait_only_after_its_full_interleaved_word_and_survives_forward_iq() {
    let mut exchange = TrafficOrderExchange::try_new(
        MobileIdentity::Min34(1),
        OrderKind::Alert,
        OrderPolicy {
            response_timeout_ms: 100,
            overall_timeout_ms: 1000,
            max_transmissions: 3,
        },
        0,
    )
    .unwrap();
    let transmission = exchange.transmission().unwrap().clone();
    let mut tx = TrafficTx::try_new(DVCC, 4, 0).unwrap();
    let command = tx.enqueue(transmission.message()).unwrap();
    let seed = VselpParameters::default();
    let mut modulator = ForwardModulator::try_new(SAMPLES_PER_SYMBOL).unwrap();
    let mut samples = Vec::new();
    for interval in 0..4 {
        if interval == 2 {
            tx.offer_media(interval, TransmitFrame::speech(&seed))
                .unwrap();
        }
        let prepared = tx.prepare(interval).unwrap().clone();
        let burst = traffic::encode_forward(
            SyncWord::One,
            &[false; traffic::SACCH_BITS],
            prepared.data(),
            DVCC,
            0,
        )
        .unwrap();
        samples.extend(modulator.encode_slot(&burst));
        let completed = tx.transmitted(&prepared).unwrap();
        if interval == 0 {
            assert_eq!(completed, None);
            assert_eq!(exchange.state(), OrderState::Prepared);
        } else if interval == 1 {
            assert_eq!(completed, Some(command.clone()));
            assert_eq!(
                exchange.transmitted(&transmission, 40).unwrap(),
                OrderState::AwaitingAck {
                    deadline_monotonic_ms: 140
                }
            );
        } else {
            assert_eq!(completed, None);
        }
    }
    samples.extend(modulator.finish());
    let acquired = decode_forward_traffic(&samples, modulator.sample_rate_hz()).unwrap();
    assert_eq!(acquired.len(), 4);
    let mut receiver = TrafficFrameReceiver::new(DVCC, seed.clone());
    let mut signaling = TrafficMessageReceiver::new(Direction::Forward);
    let mut decoder = Decoder::new(seed.clone());
    let mut messages = Vec::new();
    let mut speech_frames = 0;
    assert!(
        receiver
            .push(Some(&acquired[0].fields.fields.data))
            .unwrap()
            .is_none()
    );
    for burst in &acquired[1..] {
        let frame = receiver
            .push(Some(&burst.fields.fields.data))
            .unwrap()
            .unwrap();
        match &frame.kind {
            FrameKind::Facch(word) => {
                assert_eq!(frame.samples, decoder.conceal_missing_frame().unwrap());
                if let Some(message) = signaling.push(&SignalingWord::from(word)).unwrap() {
                    messages.push(message);
                }
            }
            FrameKind::Speech {
                crc: CrcStatus::Valid,
                ..
            } => {
                assert_eq!(frame.parameters, Some(seed.clone()));
                assert_eq!(frame.samples, decoder.decode(&seed).unwrap());
                speech_frames += 1;
            }
            other => panic!("unexpected scheduled frame: {other:?}"),
        }
    }
    assert_eq!(messages, [transmission.message().clone()]);
    assert_eq!(speech_frames, 1);
    let ack = TrafficMessage::Ack {
        ack_message_type: messages[0].message_type(),
        information: Default::default(),
    };
    assert_eq!(
        exchange
            .acknowledge(MobileIdentity::Min34(1), &ack, 60)
            .unwrap(),
        OrderState::Acknowledged
    );
}
