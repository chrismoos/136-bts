use super::*;
use crate::link::amr_configuration::DownlinkPhase;
fn time(block: u64) -> Time {
    Time {
        fdtc_block: block,
        block_offset_ns: 0,
        monotonic_ms: block * 20,
    }
}
fn context() -> ConfigurationContext {
    ConfigurationContext::try_new(MobileIdentity::Min34(1), ProtocolVersion::Pv5).unwrap()
}
fn send(exchange: &mut AcknowledgedControlExchange, block: u64) {
    let tx = exchange.transmission().unwrap().clone();
    exchange
        .transmitted(
            &tx,
            Completion {
                last_fdtc_block: block,
                completed_monotonic_ms: block * 20,
            },
            time(block),
        )
        .unwrap();
}
fn ack(exchange: &mut AcknowledgedControlExchange, block: u64) {
    let message = TrafficMessage::Ack {
        ack_message_type: exchange.transmission.message.message_type(),
        information: Default::default(),
    };
    exchange
        .acknowledge(MobileIdentity::Min34(1), &message, time(block))
        .unwrap();
}
#[test]
fn phase_orders_match_fixed_wire_vectors_and_leave_freeze_unchanged() {
    for (phase, wire) in [
        (AmrPhase::DefaultOdd, 0x2a48_u16),
        (AmrPhase::OppositeEven, 0x2a58),
        (AmrPhase::Maintain, 0x2a68),
    ] {
        let mut context = context();
        let exchange = context.prepare_phase(phase, time(0)).unwrap();
        let message = exchange.transmission().unwrap().message();
        assert_eq!(
            *message,
            TrafficMessage::AmrControl(AmrControlMessage::PhaseFreeze {
                phase,
                freeze: AmrFreeze::Maintain,
                mode: ActiveAmrMode::Mode1
            })
        );
        let expected: Vec<_> = (0..u16::BITS)
            .rev()
            .map(|bit| wire & (1 << bit) != 0)
            .collect();
        assert_eq!(message.encode().unwrap(), expected);
    }
}
#[test]
fn phase_applies_once_after_ack_and_maintain_preserves_even_without_changing_codec_configuration() {
    let mut context = context();
    let uplink = *context.uplink();
    let downlink = *context.downlink();
    assert_eq!(context.downlink_phase(), DownlinkPhase::Odd);
    let mut even = context
        .prepare_phase(AmrPhase::OppositeEven, time(0))
        .unwrap();
    assert_eq!(even.commit(&mut context), Err(Error::NotApplied));
    send(&mut even, 1);
    assert_eq!(context.downlink_phase(), DownlinkPhase::Odd);
    ack(&mut even, 2);
    assert_eq!(context.downlink_phase(), DownlinkPhase::Odd);
    even.commit(&mut context).unwrap();
    assert_eq!(context.downlink_phase(), DownlinkPhase::Even);
    let mut maintain = context.prepare_phase(AmrPhase::Maintain, time(3)).unwrap();
    send(&mut maintain, 4);
    ack(&mut maintain, 5);
    maintain.commit(&mut context).unwrap();
    assert_eq!(context.downlink_phase(), DownlinkPhase::Even);
    let mut odd = context
        .prepare_phase(AmrPhase::DefaultOdd, time(6))
        .unwrap();
    send(&mut odd, 7);
    ack(&mut odd, 8);
    odd.commit(&mut context).unwrap();
    assert_eq!(even.commit(&mut context), Err(Error::AlreadyApplied));
    assert_eq!(context.downlink_phase(), DownlinkPhase::Odd);
    assert_eq!(*context.uplink(), uplink);
    assert_eq!(*context.downlink(), downlink);
}
#[test]
fn wrong_ack_type_context_and_commit_method_cannot_change_phase() {
    let mut first = context();
    let mut second = context();
    let mut exchange = first
        .prepare_phase(AmrPhase::OppositeEven, time(0))
        .unwrap();
    assert_eq!(
        exchange.commit_thresholds(&mut first),
        Err(Error::ControlKind)
    );
    assert_eq!(
        first.cancel_thresholds(&mut exchange, time(0)),
        Err(Error::ControlKind)
    );
    send(&mut exchange, 1);
    let wrong = TrafficMessage::Ack {
        ack_message_type: TrafficMessage::AmrControl(AmrControlMessage::Thresholds(
            AmrThresholds {
                lower: [0; 3],
                hysteresis: [0; 3],
            },
        ))
        .message_type(),
        information: Default::default(),
    };
    assert!(matches!(
        exchange.acknowledge(MobileIdentity::Min34(1), &wrong, time(2)),
        Err(Error::AckType { .. })
    ));
    assert_eq!(exchange.state(), ControlState::AwaitingAck);
    ack(&mut exchange, 3);
    assert_eq!(exchange.commit(&mut second), Err(Error::Context));
    assert_eq!(second.downlink_phase(), DownlinkPhase::Odd);
    exchange.commit(&mut first).unwrap();
    assert_eq!(first.downlink_phase(), DownlinkPhase::Even);
}
#[test]
fn phase_serializes_with_thresholds_and_fenced_unsent_cancellation_releases_the_context() {
    let mut context = context();
    let thresholds = AmrThresholds {
        lower: [0; 3],
        hysteresis: [0; 3],
    };
    let mut phase = context
        .prepare_phase(AmrPhase::OppositeEven, time(0))
        .unwrap();
    assert!(matches!(
        context.prepare_thresholds(thresholds, time(0)),
        Err(Error::Busy)
    ));
    assert!(matches!(
        context.prepare_phase(AmrPhase::DefaultOdd, time(0)),
        Err(Error::Busy)
    ));
    assert_eq!(
        context.cancel_control(&mut phase, time(1)).unwrap(),
        ControlState::Cancelled
    );
    let mut sent = context
        .prepare_phase(AmrPhase::OppositeEven, time(2))
        .unwrap();
    send(&mut sent, 3);
    context.cancel_control(&mut sent, time(4)).unwrap();
    assert!(matches!(
        context.prepare_thresholds(thresholds, time(5)),
        Err(Error::Busy)
    ));
    assert_eq!(sent.commit(&mut context), Err(Error::NotApplied));
    assert_eq!(context.downlink_phase(), DownlinkPhase::Odd);
}
