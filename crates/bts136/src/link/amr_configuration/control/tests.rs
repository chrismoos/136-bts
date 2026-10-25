use super::*;
use crate::l3::legacy_traffic::{ActiveAmrMode, AmrActiveSet, AmrConfiguration, AmrLink};
use crate::link::amr_configuration::ThresholdState;
fn time(block: u64) -> Time {
    Time {
        fdtc_block: block,
        block_offset_ns: 0,
        monotonic_ms: block * 20,
    }
}
fn completion(block: u64) -> Completion {
    Completion {
        last_fdtc_block: block,
        completed_monotonic_ms: block * 20,
    }
}
fn thresholds() -> AmrThresholds {
    AmrThresholds {
        lower: [10, 20, 30],
        hysteresis: [3, 4, 5],
    }
}
fn ack(kind: u8) -> TrafficMessage {
    TrafficMessage::Ack {
        ack_message_type: kind,
        information: Default::default(),
    }
}
fn configured_context() -> ConfigurationContext {
    let mut context =
        ConfigurationContext::try_new(MobileIdentity::Min34(1), ProtocolVersion::Pv5).unwrap();
    let mut exchange = context
        .prepare(
            AmrConfiguration {
                countdown: false,
                link: AmrLink::Both,
                initial_mode: ActiveAmrMode::Mode2,
                active_set: AmrActiveSet::try_new(0x33).unwrap(),
                thresholds: [12, 23, 34],
                common_hysteresis: 5,
            },
            time(0),
        )
        .unwrap();
    let tx = exchange.transmission().unwrap().clone();
    exchange.transmitted(&tx, completion(1), time(1)).unwrap();
    exchange
        .acknowledge(
            MobileIdentity::Min34(1),
            &ack(tx.message().message_type()),
            time(2),
        )
        .unwrap();
    exchange.commit_configuration(&mut context).unwrap();
    context
}
#[test]
fn threshold_ack_changes_only_downlink_thresholds_after_validated_transmission() {
    let mut context = configured_context();
    let uplink = *context.uplink();
    let downlink = *context.downlink();
    let mut exchange = context.prepare_thresholds(thresholds(), time(3)).unwrap();
    let tx = exchange.transmission().unwrap().clone();
    let ack = ack(tx.message().message_type());
    assert_eq!(
        exchange.commit_thresholds(&mut context),
        Err(Error::NotApplied)
    );
    assert_eq!(
        exchange.acknowledge(MobileIdentity::Min34(1), &ack, time(4)),
        Err(Error::NotTransmitted)
    );
    assert_eq!(*context.downlink(), downlink);
    exchange.transmitted(&tx, completion(4), time(4)).unwrap();
    assert!(exchange.transmission().is_none());
    assert_eq!(
        exchange.transmitted(&tx, completion(4), time(4)),
        Err(Error::StaleTransmission)
    );
    exchange
        .acknowledge(MobileIdentity::Min34(1), &ack, time(5))
        .unwrap();
    assert_eq!(*context.downlink(), downlink);
    exchange.commit_thresholds(&mut context).unwrap();
    assert_eq!(*context.uplink(), uplink);
    assert_eq!(context.downlink().active_set(), downlink.active_set());
    assert_eq!(context.downlink().initial_mode(), downlink.initial_mode());
    assert_eq!(context.downlink().thresholds(), Some(&thresholds()));
    assert_eq!(
        exchange.commit_thresholds(&mut context),
        Err(Error::AlreadyApplied)
    );
}
#[test]
fn invalid_thresholds_reject_atomically_and_unused_entries_remain_valid() {
    let mut context = configured_context();
    let mut invalid = thresholds();
    invalid.lower[1] = invalid.lower[0];
    assert!(matches!(
        context.prepare_thresholds(invalid, time(3)),
        Err(Error::Codec(_))
    ));
    invalid = thresholds();
    invalid.hysteresis[0] = 16;
    assert!(matches!(
        context.prepare_thresholds(invalid, time(3)),
        Err(Error::Codec(_))
    ));
    assert!(context.prepare_thresholds(thresholds(), time(3)).is_ok());
    let mut single =
        ConfigurationContext::try_new(MobileIdentity::Min34(1), ProtocolVersion::Pv5).unwrap();
    assert!(
        single
            .prepare_thresholds(
                AmrThresholds {
                    lower: [63, 0, 0],
                    hysteresis: [15, 0, 0]
                },
                time(0)
            )
            .is_ok()
    );
}
#[test]
fn configuration_and_threshold_exchanges_share_serialization_and_context_ownership() {
    let mut first = configured_context();
    let mut second = configured_context();
    let mut exchange = first.prepare_thresholds(thresholds(), time(3)).unwrap();
    assert!(matches!(
        first.prepare_thresholds(thresholds(), time(3)),
        Err(Error::Busy)
    ));
    let config = AmrConfiguration {
        countdown: false,
        link: AmrLink::Both,
        initial_mode: ActiveAmrMode::Mode1,
        active_set: AmrActiveSet::try_new(0x10).unwrap(),
        thresholds: [0; 3],
        common_hysteresis: 0,
    };
    assert!(matches!(first.prepare(config, time(3)), Err(Error::Busy)));
    assert_eq!(
        second.cancel_thresholds(&mut exchange, time(4)),
        Err(Error::Context)
    );
    let tx = exchange.transmission().unwrap().clone();
    exchange.transmitted(&tx, completion(4), time(4)).unwrap();
    exchange
        .acknowledge(
            MobileIdentity::Min34(1),
            &ack(tx.message().message_type()),
            time(5),
        )
        .unwrap();
    assert_eq!(exchange.commit_thresholds(&mut second), Err(Error::Context));
    exchange.commit_thresholds(&mut first).unwrap();
    assert!(first.prepare(config, time(6)).is_ok());
    assert!(matches!(
        first.prepare_thresholds(thresholds(), time(6)),
        Err(Error::Busy)
    ));
}
#[test]
fn bad_ack_and_receipt_metadata_do_not_advance_the_exchange() {
    let mut context = configured_context();
    let mut foreign_context = configured_context();
    let mut exchange = context.prepare_thresholds(thresholds(), time(3)).unwrap();
    let other = foreign_context
        .prepare_thresholds(thresholds(), time(3))
        .unwrap()
        .transmission()
        .unwrap()
        .clone();
    let tx = exchange.transmission().unwrap().clone();
    assert_eq!(
        exchange.transmitted(&other, completion(4), time(4)),
        Err(Error::StaleTransmission)
    );
    assert_eq!(
        exchange.transmitted(&tx, completion(5), time(4)),
        Err(Error::TransmissionTime)
    );
    assert_eq!(
        exchange.transmitted(&tx, completion(2), time(4)),
        Err(Error::TransmissionTime)
    );
    exchange.transmitted(&tx, completion(4), time(5)).unwrap();
    assert_eq!(
        exchange.acknowledge(
            MobileIdentity::Min34(1),
            &ack(tx.message().message_type()),
            time(4)
        ),
        Err(Error::ClockRegression)
    );
    assert_eq!(
        exchange.acknowledge(
            MobileIdentity::Min34(2),
            &ack(tx.message().message_type()),
            time(6)
        ),
        Err(Error::Recipient)
    );
    assert_eq!(
        exchange.acknowledge(MobileIdentity::Min34(1), &TrafficMessage::Connect, time(6)),
        Err(Error::NotAck)
    );
    assert!(matches!(
        exchange.acknowledge(MobileIdentity::Min34(1), &ack(0xd1), time(6)),
        Err(Error::AckType { .. })
    ));
    assert_eq!(exchange.state(), ThresholdState::AwaitingAck);
    exchange
        .acknowledge(
            MobileIdentity::Min34(1),
            &ack(tx.message().message_type()),
            time(7),
        )
        .unwrap();
}
#[test]
fn no_configuration_retry_timer_is_invented_and_cancellation_preserves_uncertain_state() {
    let mut context = configured_context();
    let mut unsent = context.prepare_thresholds(thresholds(), time(3)).unwrap();
    context.cancel_thresholds(&mut unsent, time(4)).unwrap();
    assert_eq!(
        unsent.commit_thresholds(&mut context),
        Err(Error::NotApplied)
    );
    let mut sent = context.prepare_thresholds(thresholds(), time(5)).unwrap();
    let tx = sent.transmission().unwrap().clone();
    sent.transmitted(&tx, completion(6), time(6)).unwrap();
    assert!(sent.transmission().is_none());
    sent.acknowledge(
        MobileIdentity::Min34(1),
        &ack(tx.message().message_type()),
        time(1000),
    )
    .unwrap();
    sent.commit_thresholds(&mut context).unwrap();
    let mut uncertain = context
        .prepare_thresholds(thresholds(), time(1001))
        .unwrap();
    let tx = uncertain.transmission().unwrap().clone();
    uncertain
        .transmitted(&tx, completion(1002), time(1002))
        .unwrap();
    context
        .cancel_thresholds(&mut uncertain, time(1003))
        .unwrap();
    assert!(matches!(
        context.prepare_thresholds(thresholds(), time(1004)),
        Err(Error::Busy)
    ));
    assert_eq!(
        uncertain.acknowledge(
            MobileIdentity::Min34(1),
            &ack(tx.message().message_type()),
            time(1004)
        ),
        Err(Error::Closed)
    );
}
