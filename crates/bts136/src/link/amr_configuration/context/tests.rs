use super::*;
use crate::{
    l3::legacy_traffic::{TrafficMessage, TrafficMessageError},
    link::amr_configuration::{Completion, State},
};

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
fn configuration(link: AmrLink, countdown: bool) -> AmrConfiguration {
    AmrConfiguration {
        countdown,
        link,
        initial_mode: ActiveAmrMode::Mode2,
        active_set: AmrActiveSet::try_new(0x89).unwrap(),
        thresholds: [12, 23, 0],
        common_hysteresis: 5,
    }
}
fn acknowledge(exchange: &mut Exchange) {
    let tx = exchange.transmission().unwrap().clone();
    exchange
        .transmitted(
            &tx,
            Completion {
                last_fdtc_block: 1,
                completed_monotonic_ms: 20,
            },
            time(1),
        )
        .unwrap();
    let ack = TrafficMessage::Ack {
        ack_message_type: tx.message().message_type(),
        information: Default::default(),
    };
    exchange
        .acknowledge(MobileIdentity::Min34(1), &ack, time(2))
        .unwrap();
}
#[test]
fn default_is_mr74_in_both_directions_without_defined_thresholds() {
    let context = context();
    for link in [context.uplink(), context.downlink()] {
        assert_eq!(link.active_set().bitmap(), 0x10);
        assert_eq!(link.initial_mode(), ActiveAmrMode::Mode1);
        assert_eq!(link.initial_codec(), AmrCodecMode::Mr74);
        assert_eq!(link.thresholds(), None);
    }
    assert!(matches!(
        ConfigurationContext::try_new(MobileIdentity::Min34(0), ProtocolVersion::Pv5),
        Err(Error::Identity)
    ));
    assert!(matches!(
        ConfigurationContext::try_new(MobileIdentity::Min34(1), ProtocolVersion::Pv2),
        Err(Error::Codec(_))
    ));
}
#[test]
fn each_link_bit_updates_only_its_selected_direction_after_ack() {
    for link in [
        AmrLink::Neither,
        AmrLink::Uplink,
        AmrLink::Downlink,
        AmrLink::Both,
    ] {
        let mut context = context();
        let default = *context.uplink();
        let config = configuration(link, false);
        let mut exchange = context.prepare(config, time(0)).unwrap();
        assert_eq!(
            exchange.commit_configuration(&mut context),
            Err(Error::NotApplied)
        );
        assert_eq!(*context.uplink(), default);
        assert_eq!(*context.downlink(), default);
        acknowledge(&mut exchange);
        exchange.commit_configuration(&mut context).unwrap();
        for (configured, selected) in [
            (
                context.uplink(),
                matches!(link, AmrLink::Uplink | AmrLink::Both),
            ),
            (
                context.downlink(),
                matches!(link, AmrLink::Downlink | AmrLink::Both),
            ),
        ] {
            if selected {
                assert_eq!(configured.active_set(), config.active_set);
                assert_eq!(configured.initial_codec(), AmrCodecMode::Mr67);
                assert_eq!(
                    configured.thresholds(),
                    Some(&AmrThresholds {
                        lower: [12, 23, 0],
                        hysteresis: [5; 3]
                    })
                );
            } else {
                assert_eq!(*configured, default);
            }
        }
        assert_eq!(
            exchange.commit_configuration(&mut context),
            Err(Error::AlreadyApplied)
        );
    }
}
#[test]
fn countdown_cannot_commit_before_the_application_block() {
    let mut context = context();
    let mut exchange = context
        .prepare(configuration(AmrLink::Both, true), time(0))
        .unwrap();
    acknowledge(&mut exchange);
    assert!(matches!(
        exchange.state(),
        State::CountdownAcknowledged { apply_block: 14 }
    ));
    assert_eq!(
        exchange.commit_configuration(&mut context),
        Err(Error::NotApplied)
    );
    exchange.advance(time(13)).unwrap();
    assert_eq!(
        exchange.commit_configuration(&mut context),
        Err(Error::NotApplied)
    );
    exchange.advance(time(14)).unwrap();
    exchange.commit_configuration(&mut context).unwrap();
    assert_eq!(context.uplink().initial_codec(), AmrCodecMode::Mr67);
}
#[test]
fn serialization_and_private_context_ownership_reject_foreign_and_unbound_exchanges() {
    let mut first = context();
    let mut second = context();
    let config = configuration(AmrLink::Both, false);
    let mut exchange = first.prepare(config, time(0)).unwrap();
    assert!(matches!(first.prepare(config, time(0)), Err(Error::Busy)));
    acknowledge(&mut exchange);
    assert_eq!(
        exchange.commit_configuration(&mut second),
        Err(Error::Context)
    );
    assert_eq!(second.uplink().initial_codec(), AmrCodecMode::Mr74);
    let mut unbound = Exchange::try_new(
        MobileIdentity::Min34(1),
        config,
        ProtocolVersion::Pv5,
        time(0),
    )
    .unwrap();
    acknowledge(&mut unbound);
    assert_eq!(
        unbound.commit_configuration(&mut first),
        Err(Error::Context)
    );
    exchange.commit_configuration(&mut first).unwrap();
    assert!(first.prepare(config, time(3)).is_ok());
    assert_eq!(
        exchange.commit_configuration(&mut first),
        Err(Error::AlreadyApplied)
    );
}
#[test]
fn invalid_configuration_is_atomic_and_does_not_reserve_the_context() {
    let mut context = context();
    let mut config = configuration(AmrLink::Both, false);
    config.initial_mode = ActiveAmrMode::Mode4;
    assert!(matches!(
        context.prepare(config, time(0)),
        Err(Error::Codec(TrafficMessageError::Field("amr_initial_mode")))
    ));
    assert_eq!(context.uplink().initial_codec(), AmrCodecMode::Mr74);
    assert!(
        context
            .prepare(configuration(AmrLink::Both, false), time(0))
            .is_ok()
    );
}
#[test]
fn unsent_cancellation_releases_context_but_transmitted_cancellation_requires_retirement() {
    let mut first = context();
    let config = configuration(AmrLink::Both, false);
    let mut unsent = first.prepare(config, time(0)).unwrap();
    assert_eq!(
        first.cancel(&mut unsent, time(1)).unwrap(),
        State::Cancelled
    );
    assert_eq!(
        unsent.commit_configuration(&mut first),
        Err(Error::NotApplied)
    );
    assert!(matches!(
        first.prepare(config, time(0)),
        Err(Error::ClockRegression)
    ));
    assert!(first.prepare(config, time(2)).is_ok());
    assert_eq!(first.uplink().initial_codec(), AmrCodecMode::Mr74);

    let mut second = context();
    let mut sent = second.prepare(config, time(0)).unwrap();
    let tx = sent.transmission().unwrap().clone();
    sent.transmitted(
        &tx,
        Completion {
            last_fdtc_block: 1,
            completed_monotonic_ms: 20,
        },
        time(1),
    )
    .unwrap();
    assert_eq!(second.cancel(&mut sent, time(2)).unwrap(), State::Cancelled);
    assert!(sent.reverse_muted());
    assert_eq!(
        sent.commit_configuration(&mut second),
        Err(Error::NotApplied)
    );
    assert!(matches!(second.prepare(config, time(3)), Err(Error::Busy)));
}
#[test]
fn successive_directional_configurations_preserve_the_other_link_and_clock_floor() {
    let mut context = context();
    let mut first = context
        .prepare(configuration(AmrLink::Uplink, false), time(0))
        .unwrap();
    acknowledge(&mut first);
    first.commit_configuration(&mut context).unwrap();
    let uplink = *context.uplink();
    let downlink = *context.downlink();
    assert!(matches!(
        context.prepare(configuration(AmrLink::Both, false), time(1)),
        Err(Error::ClockRegression)
    ));
    assert_eq!(*context.uplink(), uplink);
    assert_eq!(*context.downlink(), downlink);
    let mut config = configuration(AmrLink::Downlink, false);
    config.active_set = AmrActiveSet::try_new(0x30).unwrap();
    let mut second = context.prepare(config, time(2)).unwrap();
    let tx = second.transmission().unwrap().clone();
    second
        .transmitted(
            &tx,
            Completion {
                last_fdtc_block: 3,
                completed_monotonic_ms: 60,
            },
            time(3),
        )
        .unwrap();
    second
        .acknowledge(
            MobileIdentity::Min34(1),
            &TrafficMessage::Ack {
                ack_message_type: tx.message().message_type(),
                information: Default::default(),
            },
            time(4),
        )
        .unwrap();
    second.commit_configuration(&mut context).unwrap();
    assert_eq!(*context.uplink(), uplink);
    assert_eq!(context.downlink().initial_codec(), AmrCodecMode::Mr795);
    assert_eq!(
        first.commit_configuration(&mut context),
        Err(Error::AlreadyApplied)
    );
    assert_eq!(context.downlink().initial_codec(), AmrCodecMode::Mr795);
}
