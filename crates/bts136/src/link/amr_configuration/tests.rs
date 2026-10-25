use super::*;
use crate::l3::legacy_traffic::{
    AcknowledgmentInformation, ActiveAmrMode, AmrActiveSet, AmrLink, ControlChannel, Hyperband,
};
fn time(fdtc_block: u64, monotonic_ms: u64) -> Time {
    Time {
        fdtc_block,
        block_offset_ns: ((monotonic_ms % 20) * 1_000_000) as u32,
        monotonic_ms,
    }
}
fn completion(now: Time) -> Completion {
    Completion {
        last_fdtc_block: now.fdtc_block,
        completed_monotonic_ms: now.monotonic_ms,
    }
}
fn configuration(countdown: bool) -> AmrConfiguration {
    AmrConfiguration {
        countdown,
        link: AmrLink::Both,
        initial_mode: ActiveAmrMode::Mode2,
        active_set: AmrActiveSet::try_new(0x33).unwrap(),
        thresholds: [12, 23, 34],
        common_hysteresis: 5,
    }
}
fn exchange(countdown: bool) -> Exchange {
    Exchange::try_new(
        MobileIdentity::Min34(1),
        configuration(countdown),
        ProtocolVersion::Pv5,
        time(0, 0),
    )
    .unwrap()
}
fn ack() -> TrafficMessage {
    TrafficMessage::Ack {
        ack_message_type: 0xd1,
        information: Default::default(),
    }
}
fn send(exchange: &mut Exchange, now: Time) -> Transmission {
    let tx = exchange.transmission().unwrap().clone();
    exchange.transmitted(&tx, completion(now), now).unwrap();
    tx
}
#[test]
fn response_timers_start_at_validated_final_transmission_and_stale_receipts_do_not_start_them() {
    let mut first = exchange(false);
    let other = exchange(false).transmission().unwrap().clone();
    assert_eq!(
        first.transmitted(&other, completion(time(1, 20)), time(1, 20)),
        Err(Error::StaleTransmission)
    );
    assert_eq!(
        first.acknowledge(MobileIdentity::Min34(1), &ack(), time(1, 20)),
        Err(Error::NotTransmitted)
    );
    assert_eq!(first.advance(time(1000, 20000)).unwrap(), State::Prepared);
    assert!(!first.reverse_muted());
    let tx = send(&mut first, time(1001, 20020));
    assert_eq!(
        first.state(),
        State::AwaitingAck {
            deadline_monotonic_ms: 20120
        }
    );
    assert_eq!(
        first.transmitted(&tx, completion(time(1002, 20040)), time(1002, 20040)),
        Err(Error::StaleTransmission)
    );
    assert_eq!(first.transmissions(), 1);
    assert_eq!(
        first.advance(time(1005, 20119)).unwrap(),
        State::AwaitingAck {
            deadline_monotonic_ms: 20120
        }
    );
    assert_eq!(first.advance(time(1006, 20120)).unwrap(), State::Prepared);
    assert!(first.filler_requested() && first.reverse_muted());
    assert!(!first.transmission().unwrap().matches(&tx));
}
#[test]
fn immediate_ack_applies_without_waiting_for_a_countdown() {
    let mut exchange = exchange(false);
    send(&mut exchange, time(10, 200));
    assert_eq!(
        exchange
            .acknowledge(MobileIdentity::Min34(1), &ack(), time(11, 220))
            .unwrap(),
        State::Applied
    );
    assert!(exchange.transmission().is_none());
    assert!(!exchange.filler_requested() && !exchange.reverse_muted());
}
#[test]
fn countdown_ack_is_inclusive_at_twelve_and_application_waits_for_thirteen() {
    for timeout_first in [false, true] {
        let mut exchange = exchange(true);
        send(&mut exchange, time(100, 2000));
        assert_eq!(
            exchange.state(),
            State::Countdown {
                ack_block: 112,
                apply_block: 113
            }
        );
        let mut withdrawn = None;
        if timeout_first {
            exchange.advance(time(112, 2240)).unwrap();
            assert!(exchange.filler_requested());
            withdrawn = exchange.transmission().cloned();
            assert!(!exchange.configuration().countdown);
        }
        assert_eq!(
            exchange
                .acknowledge(MobileIdentity::Min34(1), &ack(), time(112, 2240))
                .unwrap(),
            State::CountdownAcknowledged { apply_block: 113 }
        );
        assert!(exchange.configuration().countdown);
        assert!(!exchange.filler_requested() && !exchange.reverse_muted());
        assert_eq!(
            exchange.advance(time(112, 2259)).unwrap(),
            State::CountdownAcknowledged { apply_block: 113 }
        );
        assert_eq!(exchange.advance(time(113, 2260)).unwrap(), State::Applied);
        if let Some(tx) = withdrawn {
            assert_eq!(
                exchange.transmitted(&tx, completion(time(114, 2280)), time(114, 2280)),
                Err(Error::Closed)
            );
        }
    }
}
#[test]
fn early_countdown_ack_does_not_advance_application_or_restart_on_duplicates() {
    let mut exchange = exchange(true);
    send(&mut exchange, time(10, 200));
    let expected = State::CountdownAcknowledged { apply_block: 23 };
    assert_eq!(
        exchange
            .acknowledge(MobileIdentity::Min34(1), &ack(), time(11, 220))
            .unwrap(),
        expected
    );
    assert_eq!(
        exchange
            .acknowledge(MobileIdentity::Min34(1), &ack(), time(20, 400))
            .unwrap(),
        expected
    );
    assert_eq!(exchange.advance(time(22, 440)).unwrap(), expected);
    assert_eq!(exchange.advance(time(23, 460)).unwrap(), State::Applied);
}
#[test]
fn missed_countdown_switches_to_immediate_retry_and_rejects_late_original_ack() {
    let mut exchange = exchange(true);
    let old = send(&mut exchange, time(100, 2000));
    assert_eq!(
        exchange.advance(time(111, 2220)).unwrap(),
        State::Countdown {
            ack_block: 112,
            apply_block: 113
        }
    );
    assert_eq!(exchange.advance(time(112, 2240)).unwrap(), State::Prepared);
    assert!(!exchange.configuration().countdown);
    assert!(exchange.reverse_muted());
    assert_eq!(
        exchange.acknowledge(MobileIdentity::Min34(1), &ack(), time(113, 2260)),
        Err(Error::LateCountdownAck)
    );
    assert_eq!(
        exchange.transmitted(&old, completion(time(113, 2260)), time(113, 2260)),
        Err(Error::StaleTransmission)
    );
    let retry = exchange.transmission().unwrap().clone();
    assert!(
        matches!(retry.message(), TrafficMessage::AmrControl(AmrControlMessage::Configuration(configuration)) if !configuration.countdown)
    );
    exchange
        .transmitted(&retry, completion(time(114, 2280)), time(114, 2280))
        .unwrap();
    assert_eq!(
        exchange
            .acknowledge(MobileIdentity::Min34(1), &ack(), time(115, 2300))
            .unwrap(),
        State::Applied
    );
    assert!(!exchange.reverse_muted());
}
#[test]
fn retries_keep_one_filler_deadline_and_abort_at_five_seconds_of_actual_filler() {
    let mut exchange = exchange(false);
    send(&mut exchange, time(10, 200));
    exchange.advance(time(15, 300)).unwrap();
    let filler = exchange.filler_handle().unwrap().clone();
    exchange
        .filler_transmitted(&filler, 302, time(16, 320))
        .unwrap();
    send(&mut exchange, time(16, 320));
    assert_eq!(
        exchange.advance(time(20, 419)).unwrap(),
        State::AwaitingAck {
            deadline_monotonic_ms: 420
        }
    );
    assert_eq!(exchange.advance(time(21, 420)).unwrap(), State::Prepared);
    exchange
        .filler_transmitted(&filler, 400, time(21, 420))
        .unwrap();
    send(&mut exchange, time(22, 440));
    assert_eq!(exchange.advance(time(27, 540)).unwrap(), State::Prepared);
    assert_eq!(exchange.advance(time(265, 5301)).unwrap(), State::Prepared);
    assert_eq!(exchange.advance(time(265, 5302)).unwrap(), State::Aborted);
    assert!(exchange.reverse_muted());
    assert!(!exchange.filler_requested());
    assert!(exchange.transmission().is_none());
    assert_eq!(
        exchange.acknowledge(MobileIdentity::Min34(1), &ack(), time(266, 5320)),
        Err(Error::Closed)
    );
}
#[test]
fn no_abort_timer_runs_before_filler_and_delayed_receipts_use_the_actual_start() {
    let mut exchange = exchange(false);
    send(&mut exchange, time(0, 0));
    exchange.advance(time(5, 100)).unwrap();
    let filler = exchange.filler_handle().unwrap().clone();
    assert_eq!(exchange.advance(time(100, 2000)).unwrap(), State::Prepared);
    assert_eq!(
        exchange
            .filler_transmitted(&filler, 200, time(300, 6000))
            .unwrap(),
        State::Aborted
    );
    assert!(exchange.reverse_muted());
}
#[test]
fn earlier_filler_receipt_tightens_deadline_and_later_receipts_cannot_extend_it() {
    let mut exchange = exchange(false);
    send(&mut exchange, time(0, 0));
    exchange.advance(time(5, 100)).unwrap();
    let filler = exchange.filler_handle().unwrap().clone();
    exchange
        .filler_transmitted(&filler, 200, time(10, 200))
        .unwrap();
    exchange
        .filler_transmitted(&filler, 150, time(11, 220))
        .unwrap();
    exchange
        .filler_transmitted(&filler, 220, time(11, 220))
        .unwrap();
    assert_eq!(exchange.advance(time(257, 5149)).unwrap(), State::Prepared);
    assert_eq!(exchange.advance(time(257, 5150)).unwrap(), State::Aborted);
}
#[test]
fn valid_late_immediate_ack_withdraws_retry_and_retires_pending_filler() {
    let mut exchange = exchange(false);
    send(&mut exchange, time(0, 0));
    exchange.advance(time(5, 100)).unwrap();
    let retry = exchange.transmission().unwrap().clone();
    let filler = exchange.filler_handle().unwrap().clone();
    assert_eq!(
        exchange
            .acknowledge(MobileIdentity::Min34(1), &ack(), time(6, 120))
            .unwrap(),
        State::Applied
    );
    assert_eq!(
        exchange.transmitted(&retry, completion(time(7, 140)), time(7, 140)),
        Err(Error::Closed)
    );
    assert_eq!(
        exchange.filler_transmitted(&filler, 110, time(7, 140)),
        Err(Error::Closed)
    );
    assert!(!exchange.reverse_muted());
}
#[test]
fn wrong_ack_fields_and_foreign_filler_do_not_change_deadlines_or_ownership() {
    let mut exchange = exchange(false);
    send(&mut exchange, time(0, 0));
    assert_eq!(
        exchange.acknowledge(MobileIdentity::Min34(2), &ack(), time(1, 20)),
        Err(Error::Recipient)
    );
    assert_eq!(
        exchange.acknowledge(
            MobileIdentity::Min34(1),
            &TrafficMessage::Connect,
            time(1, 20)
        ),
        Err(Error::NotAck)
    );
    let mut wrong = ack();
    if let TrafficMessage::Ack {
        ack_message_type, ..
    } = &mut wrong
    {
        *ack_message_type = 0xd2;
    }
    assert_eq!(
        exchange.acknowledge(MobileIdentity::Min34(1), &wrong, time(1, 20)),
        Err(Error::AckType {
            expected: 0xd1,
            received: 0xd2
        })
    );
    let invalid = TrafficMessage::Ack {
        ack_message_type: 0xd1,
        information: AcknowledgmentInformation {
            control_channels: vec![ControlChannel {
                hyperband: Hyperband::Cellular800,
                channel: 1,
                dvcc: 1,
            }],
            ..Default::default()
        },
    };
    assert!(matches!(
        exchange.acknowledge(MobileIdentity::Min34(1), &invalid, time(1, 20)),
        Err(Error::Codec(_))
    ));
    for information in [
        AcknowledgmentInformation {
            last_decoded_parameter: Some(16),
            ..Default::default()
        },
        AcknowledgmentInformation {
            unparsed_bits: vec![false; 512],
            ..Default::default()
        },
    ] {
        let invalid = TrafficMessage::Ack {
            ack_message_type: 0xd1,
            information,
        };
        assert!(matches!(
            exchange.acknowledge(MobileIdentity::Min34(1), &invalid, time(1, 20)),
            Err(Error::Codec(_))
        ));
    }
    assert_eq!(
        exchange.state(),
        State::AwaitingAck {
            deadline_monotonic_ms: 100
        }
    );
    exchange.advance(time(5, 100)).unwrap();
    let mut other = super::tests::exchange(false);
    send(&mut other, time(0, 0));
    other.advance(time(5, 100)).unwrap();
    assert_eq!(
        exchange.filler_transmitted(other.filler_handle().unwrap(), 100, time(5, 100)),
        Err(Error::StaleFiller)
    );
    let filler = exchange.filler_handle().unwrap().clone();
    assert_eq!(
        exchange.filler_transmitted(&filler, 99, time(5, 100)),
        Err(Error::FillerTime)
    );
    assert_eq!(
        exchange.filler_transmitted(&filler, 101, time(5, 100)),
        Err(Error::FillerTime)
    );
}
#[test]
fn cancellation_after_transmission_keeps_mute_until_bearer_retirement() {
    for transmitted in [false, true] {
        let mut exchange = exchange(true);
        let tx = exchange.transmission().unwrap().clone();
        if transmitted {
            exchange
                .transmitted(&tx, completion(time(0, 0)), time(0, 0))
                .unwrap();
        }
        assert_eq!(exchange.cancel(time(1, 20)).unwrap(), State::Cancelled);
        assert_eq!(exchange.reverse_muted(), transmitted);
        assert!(!exchange.filler_requested());
        assert_eq!(
            exchange.transmitted(&tx, completion(time(2, 40)), time(2, 40)),
            Err(Error::Closed)
        );
        assert_eq!(
            exchange.acknowledge(MobileIdentity::Min34(1), &ack(), time(2, 40)),
            Err(Error::Closed)
        );
    }
}
#[test]
fn clock_regression_and_deadline_overflow_leave_prepared_transmissions_intact() {
    let mut exchange = exchange(false);
    exchange.advance(time(10, 200)).unwrap();
    assert_eq!(exchange.advance(time(9, 220)), Err(Error::ClockRegression));
    assert_eq!(exchange.advance(time(11, 199)), Err(Error::ClockRegression));
    let tx = exchange.transmission().unwrap().clone();
    assert_eq!(
        exchange.transmitted(&tx, completion(time(11, u64::MAX)), time(11, u64::MAX)),
        Err(Error::TimeOverflow)
    );
    assert_eq!(exchange.state(), State::Prepared);
    assert!(exchange.transmission().unwrap().matches(&tx));
    send(&mut exchange, time(11, 220));
    let mut countdown = super::tests::exchange(true);
    let tx = countdown.transmission().unwrap().clone();
    assert_eq!(
        countdown.transmitted(&tx, completion(time(u64::MAX, 0)), time(u64::MAX, 0)),
        Err(Error::TimeOverflow)
    );
    assert_eq!(countdown.transmissions(), 0);
}
#[test]
fn request_and_ack_are_compatible_with_versioned_traffic_word_assembly() {
    use crate::link::traffic::{self, TrafficMessageReceiver};
    let mut exchange = exchange(true);
    let tx = exchange.transmission().unwrap().clone();
    let mut mobile = TrafficMessageReceiver::with_version(Direction::Forward, ProtocolVersion::Pv5);
    let request = mobile
        .push(&traffic::encode(tx.message()).unwrap()[0])
        .unwrap()
        .unwrap();
    assert_eq!(request, *tx.message());
    exchange
        .transmitted(&tx, completion(time(0, 0)), time(0, 0))
        .unwrap();
    let mut base = TrafficMessageReceiver::with_version(Direction::Reverse, ProtocolVersion::Pv5);
    let ack = base
        .push(&traffic::encode(&ack()).unwrap()[0])
        .unwrap()
        .unwrap();
    assert_eq!(
        exchange
            .acknowledge(MobileIdentity::Min34(1), &ack, time(1, 20))
            .unwrap(),
        State::CountdownAcknowledged { apply_block: 13 }
    );
}

#[test]
fn filler_abort_deadline_precedes_an_ack_at_the_exact_same_time() {
    let mut exchange = exchange(false);
    send(&mut exchange, time(0, 0));
    exchange.advance(time(5, 100)).unwrap();
    let filler = exchange.filler_handle().unwrap().clone();
    let retry = exchange.transmission().unwrap().clone();
    exchange
        .filler_transmitted(&filler, 100, time(5, 100))
        .unwrap();
    assert_eq!(
        exchange.acknowledge(MobileIdentity::Min34(1), &ack(), time(255, 5100)),
        Err(Error::Closed)
    );
    assert_eq!(exchange.state(), State::Aborted);
    assert_eq!(
        exchange.transmitted(&retry, completion(time(255, 5100)), time(255, 5100)),
        Err(Error::Closed)
    );
    assert!(exchange.reverse_muted());
}
#[test]
fn invalid_construction_and_filler_overflow_do_not_create_a_partial_exchange() {
    for identity in [MobileIdentity::Min34(0), MobileIdentity::Min34(1 << 34)] {
        assert!(matches!(
            Exchange::try_new(
                identity,
                configuration(false),
                ProtocolVersion::Pv5,
                time(0, 0)
            ),
            Err(Error::Identity)
        ));
    }
    assert!(matches!(
        Exchange::try_new(
            MobileIdentity::Min34(1),
            configuration(false),
            ProtocolVersion::Pv2,
            time(0, 0)
        ),
        Err(Error::Codec(_))
    ));
    let invalid = AmrConfiguration {
        thresholds: [64, 0, 0],
        ..configuration(false)
    };
    assert!(matches!(
        Exchange::try_new(
            MobileIdentity::Min34(1),
            invalid,
            ProtocolVersion::Pv5,
            time(0, 0)
        ),
        Err(Error::Codec(_))
    ));
    let mut exchange = exchange(false);
    send(&mut exchange, time(0, 0));
    exchange.advance(time(5, 100)).unwrap();
    let filler = exchange.filler_handle().unwrap().clone();
    assert_eq!(
        exchange.filler_transmitted(&filler, u64::MAX - 4999, time(6, u64::MAX)),
        Err(Error::TimeOverflow)
    );
    assert_eq!(exchange.state(), State::Prepared);
    assert!(exchange.filler_handle().unwrap().matches(&filler));
    exchange
        .filler_transmitted(&filler, 120, time(6, 120))
        .unwrap();
}

#[test]
fn countdown_deadline_does_not_extend_into_the_rest_of_block_twelve() {
    for timeout_first in [false, true] {
        for offset in [0, 1] {
            let mut exchange = exchange(true);
            send(&mut exchange, time(0, 0));
            if timeout_first {
                exchange.advance(time(12, 240)).unwrap();
            }
            let now = Time {
                block_offset_ns: offset,
                ..time(12, 240)
            };
            let result = exchange.acknowledge(MobileIdentity::Min34(1), &ack(), now);
            if offset == 0 {
                assert_eq!(
                    result.unwrap(),
                    State::CountdownAcknowledged { apply_block: 13 }
                );
            } else {
                assert_eq!(result, Err(Error::LateCountdownAck));
                assert_eq!(exchange.state(), State::Prepared);
                assert!(exchange.reverse_muted());
            }
        }
    }
    let mut exchange = exchange(true);
    send(&mut exchange, time(0, 0));
    let before = Time {
        fdtc_block: 11,
        block_offset_ns: 19_999_999,
        monotonic_ms: 239,
    };
    assert_eq!(
        exchange
            .acknowledge(MobileIdentity::Min34(1), &ack(), before)
            .unwrap(),
        State::CountdownAcknowledged { apply_block: 13 }
    );
    assert_eq!(
        exchange.advance(Time {
            block_offset_ns: 20_000_000,
            ..time(12, 240)
        }),
        Err(Error::BlockOffset)
    );
    assert_eq!(
        exchange.advance(Time {
            block_offset_ns: 19_999_998,
            ..before
        }),
        Err(Error::ClockRegression)
    );
}

#[test]
fn delayed_completions_use_actual_air_boundaries_and_expire_already_due_timers() {
    let mut countdown = exchange(true);
    let tx = countdown.transmission().unwrap().clone();
    let final_portion = Completion {
        last_fdtc_block: 0,
        completed_monotonic_ms: 20,
    };
    assert_eq!(
        countdown
            .transmitted(&tx, final_portion, time(1, 20))
            .unwrap(),
        State::Countdown {
            ack_block: 12,
            apply_block: 13
        }
    );
    let mut immediate = exchange(false);
    let tx = immediate.transmission().unwrap().clone();
    immediate.advance(time(10, 200)).unwrap();
    assert_eq!(
        immediate
            .transmitted(&tx, final_portion, time(10, 200))
            .unwrap(),
        State::Prepared
    );
    assert!(immediate.filler_requested() && immediate.reverse_muted());
    assert_eq!(immediate.transmissions(), 1);
    let retry = immediate.transmission().unwrap().clone();
    assert_eq!(
        immediate.transmitted(&retry, final_portion, time(11, 220)),
        Err(Error::TransmissionTime)
    );
    assert!(immediate.transmission().unwrap().matches(&retry));
    let future = Completion {
        last_fdtc_block: 12,
        completed_monotonic_ms: 240,
    };
    assert_eq!(
        immediate.transmitted(&retry, future, time(11, 220)),
        Err(Error::TransmissionTime)
    );
}
