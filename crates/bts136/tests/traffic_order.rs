use bts136::{
    l3::{legacy_traffic::TrafficMessage, relay::Direction},
    link::{
        rach::MobileIdentity,
        traffic::{self, SignalingWord, TrafficMessageReceiver},
        traffic_order::{OrderKind, OrderPolicy, OrderState, TrafficOrderExchange},
    },
    phy::facch,
};
const DVCC: u8 = 42;
const RECIPIENT: MobileIdentity = MobileIdentity::Min34(1);
fn received_facch(message: &TrafficMessage, direction: Direction) -> TrafficMessage {
    let mut receiver = TrafficMessageReceiver::new(direction);
    let mut result = None;
    for word in traffic::encode(message).unwrap() {
        let coded = facch::encode(word.continuation, &word.contents, DVCC);
        let decoded = facch::decode(&coded, DVCC).unwrap();
        if let Some(message) = receiver.push(&SignalingWord::from(&decoded)).unwrap() {
            result = Some(message);
        }
    }
    result.unwrap()
}
#[test]
fn repeated_alert_and_release_orders_complete_only_on_matching_coded_mobile_ack() {
    for (kind, expected_type) in [(OrderKind::Alert, 0x31), (OrderKind::Release, 0xda)] {
        let mut exchange = TrafficOrderExchange::try_new(
            RECIPIENT,
            kind,
            OrderPolicy {
                response_timeout_ms: 100,
                overall_timeout_ms: 1000,
                max_transmissions: 3,
            },
            0,
        )
        .unwrap();
        let first = exchange.transmission().unwrap().clone();
        let order = received_facch(first.message(), Direction::Forward);
        assert_eq!(order.message_type(), expected_type);
        exchange.transmitted(&first, 20).unwrap();
        assert_eq!(exchange.advance(120).unwrap(), OrderState::Prepared);
        let retry = exchange.transmission().unwrap().clone();
        assert_eq!(received_facch(retry.message(), Direction::Forward), order);
        exchange.transmitted(&retry, 140).unwrap();
        let mobile_ack = TrafficMessage::Ack {
            ack_message_type: expected_type,
            information: Default::default(),
        };
        let received = received_facch(&mobile_ack, Direction::Reverse);
        assert_eq!(
            exchange.acknowledge(RECIPIENT, &received, 160).unwrap(),
            OrderState::Acknowledged
        );
        assert_eq!(exchange.transmissions(), 2);
        assert!(exchange.transmission().is_none());
    }
}
