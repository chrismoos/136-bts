use super::*;
use crate::link::spach::arq::{ArqReassembler, Reception};
fn echo(frame: &PreparedFrame) -> u8 {
    match arq::decode(&frame.information).unwrap() {
        ArqFrame::Begin { header, .. } | ArqFrame::Continue { header, .. } => header.partial_echo,
    }
}
fn new_sender() -> Sender<u64> {
    Sender::try_new(32).unwrap()
}
fn enqueue(sender: &mut Sender<u64>, identity: u64, bytes: Vec<u8>) {
    sender
        .enqueue(
            Request {
                identity: MobileIdentity::Min34(identity),
                channel: Channel::Smsch,
                message: bytes,
                completion: identity,
                deadline_ms: 6400,
            },
            0,
            0,
        )
        .unwrap();
}
fn send(sender: &mut Sender<u64>, block: u64) -> PreparedFrame {
    let frame = sender.prepare(block, block * 20).unwrap().unwrap();
    sender.submitted(&frame.token, block, block * 20).unwrap();
    assert!(sender.transmitted(&frame.token, block, block * 20).unwrap());
    frame
}
#[test]
fn lost_continue_is_retransmitted_selectively_and_completes_exact_message_once() {
    let mut sender = new_sender();
    let bytes: Vec<_> = (0..255).map(|value| value as u8).collect();
    enqueue(&mut sender, 1, bytes.clone());
    let mut receiver = ArqReassembler::try_new(MobileIdentity::Min34(1)).unwrap();
    let mut status = None;
    let mut lost = None;
    let mut block = 0;
    while sender.prepare(block, block * 20).unwrap().is_some() {
        let frame = send(&mut sender, block);
        if matches!(
            arq::decode(&frame.information).unwrap(),
            ArqFrame::Continue {
                frame_number: 0,
                ..
            }
        ) {
            lost = Some(frame.information);
        } else if let Reception::Accepted {
            message,
            status_request,
        } = receiver.push(&frame.information).unwrap()
        {
            assert!(message.is_none());
            status = status_request.or(status);
        }
        block += 1;
    }
    let status = status.unwrap();
    assert!(!status.status.received_frames[0]);
    assert!(sender.status(&status.status, block, block * 20).unwrap());
    let retry = send(&mut sender, block);
    let ArqFrame::Continue {
        header,
        frame_number,
        data,
    } = arq::decode(&retry.information).unwrap()
    else {
        panic!()
    };
    assert_eq!(frame_number, 0);
    assert!(header.polling);
    let ArqFrame::Continue { data: original, .. } = arq::decode(&lost.unwrap()).unwrap() else {
        panic!()
    };
    assert_eq!(data, original);
    let Reception::Accepted {
        message,
        status_request,
    } = receiver.push(&retry.information).unwrap()
    else {
        panic!()
    };
    assert_eq!(message.unwrap().message, bytes);
    assert!(sender.pop_event().is_none());
    assert!(
        sender
            .status(&status_request.unwrap().status, block + 1, (block + 1) * 20)
            .unwrap()
    );
    assert!(matches!(sender.pop_event(), Some(Event::Complete(1))));
    assert!(sender.pop_event().is_none());
    assert_eq!(sender.pending(), 0);
}
#[test]
fn status_before_host_completion_waits_for_all_submitted_receipts() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![0x55; 20]);
    let begin = sender.prepare(0, 0).unwrap().unwrap();
    let status = ArqStatus {
        partial_echo: echo(&begin),
        received_frames: [true; crate::link::rach::ARQ_MAP_BITS],
    };
    assert!(!sender.status(&status, 0, 0).unwrap());
    let mut receipts = Vec::new();
    let mut block = 0;
    while let Some(frame) = sender.prepare(block, block * 20).unwrap() {
        sender.submitted(&frame.token, block, block * 20).unwrap();
        receipts.push(frame.token);
        block += 1;
    }
    assert!(sender.status(&status, block, block * 20).unwrap());
    assert!(sender.pop_event().is_none());
    for token in receipts {
        sender.transmitted(&token, block, block * 20).unwrap();
    }
    assert!(matches!(sender.pop_event(), Some(Event::Complete(1))));
}
#[test]
fn unsent_frame_bits_in_status_cannot_complete_a_transfer() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![0x55; 20]);
    let begin = send(&mut sender, 0);
    let status = ArqStatus {
        partial_echo: echo(&begin),
        received_frames: [true; crate::link::rach::ARQ_MAP_BITS],
    };
    sender.status(&status, 1, 20).unwrap();
    let mut block = 1;
    while sender.prepare(block, block * 20).unwrap().is_some() {
        send(&mut sender, block);
        block += 1;
        assert!(block < 8, "sender repeated a round without a new status");
    }
    assert!(sender.pop_event().is_none());
    assert_eq!(sender.pending(), 1);
    sender.status(&status, block, block * 20).unwrap();
    assert!(matches!(sender.pop_event(), Some(Event::Complete(1))));
}
#[test]
fn missing_status_restarts_with_new_echo_and_ignores_old_status_and_receipts() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![0xaa]);
    let old = send(&mut sender, 0);
    assert!(sender.prepare(63, 1260).unwrap().is_none());
    let restarted = sender.prepare(64, 1280).unwrap().unwrap();
    assert_ne!(echo(&old), echo(&restarted));
    assert!(matches!(
        arq::decode(&restarted.information).unwrap(),
        ArqFrame::Begin { .. }
    ));
    let stale = ArqStatus {
        partial_echo: echo(&old),
        received_frames: [true; crate::link::rach::ARQ_MAP_BITS],
    };
    assert!(!sender.status(&stale, 64, 1280).unwrap());
    assert!(!sender.transmitted(&old.token, 64, 1280).unwrap());
    assert!(sender.pop_event().is_none());
    sender.submitted(&restarted.token, 64, 1280).unwrap();
    sender.transmitted(&restarted.token, 64, 1280).unwrap();
    let status = ArqStatus {
        partial_echo: echo(&restarted),
        ..stale
    };
    sender.status(&status, 65, 1300).unwrap();
    assert!(matches!(sender.pop_event(), Some(Event::Complete(1))));
}
#[test]
fn interrupted_round_restarts_before_sending_a_contextless_continue() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![0xaa; 255]);
    let begin = send(&mut sender, 0);
    let frame = sender.prepare(64, 1280).unwrap().unwrap();
    assert_ne!(echo(&frame), echo(&begin));
    assert!(matches!(
        arq::decode(&frame.information).unwrap(),
        ArqFrame::Begin { .. }
    ));
}
#[test]
fn multiple_phones_have_distinct_echoes_round_robin_frames_and_independent_status() {
    let mut sender = new_sender();
    for identity in 1..=32 {
        enqueue(&mut sender, identity, vec![0xaa; 20]);
    }
    let mut echoes = std::collections::HashSet::new();
    for block in 0..32 {
        let frame = send(&mut sender, block);
        assert!(echoes.insert(echo(&frame)));
        let ArqFrame::Begin { identity, .. } = arq::decode(&frame.information).unwrap() else {
            panic!()
        };
        assert_eq!(identity, MobileIdentity::Min34(block + 1));
    }
    let status = ArqStatus {
        partial_echo: 127,
        received_frames: [true; crate::link::rach::ARQ_MAP_BITS],
    };
    assert!(!sender.status(&status, 32, 640).unwrap());
    assert_eq!(sender.pending(), 32);
}
#[test]
fn rejected_submission_keeps_identical_prepared_frame_and_does_not_start_timeout() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![0xaa]);
    let first = sender.prepare(0, 0).unwrap().unwrap();
    let later = sender.prepare(65, 1300).unwrap().unwrap();
    assert_eq!(first.information, later.information);
    assert!(first.token.matches(&later.token));
    sender.submitted(&later.token, 65, 1300).unwrap();
    assert_eq!(sender.submitted(&later.token, 65, 1300), Err(Error::Stale));
}
#[test]
fn expiry_wins_over_late_receipt_and_status_without_completing_registration() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![0xaa]);
    let frame = sender.prepare(0, 0).unwrap().unwrap();
    sender.submitted(&frame.token, 0, 0).unwrap();
    let status = ArqStatus {
        partial_echo: echo(&frame),
        received_frames: [false; crate::link::rach::ARQ_MAP_BITS],
    };
    sender.status(&status, 1, 20).unwrap();
    sender.transmitted(&frame.token, 320, 6400).unwrap();
    assert!(matches!(sender.pop_event(), Some(Event::Expired(1))));
    assert!(!sender.status(&status, 320, 6400).unwrap());
}
#[test]
fn replacement_is_atomic_and_stale_generation_cannot_complete_new_request() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![0xaa]);
    let old = send(&mut sender, 0);
    assert!(
        sender
            .enqueue(
                Request {
                    identity: MobileIdentity::Min34(1),
                    channel: Channel::Pch,
                    message: vec![1],
                    completion: 2,
                    deadline_ms: 6400
                },
                1,
                20
            )
            .is_err()
    );
    assert_eq!(sender.pending(), 1);
    sender
        .enqueue(
            Request {
                identity: MobileIdentity::Min34(1),
                channel: Channel::Arch,
                message: vec![1],
                completion: 2,
                deadline_ms: 6400,
            },
            1,
            20,
        )
        .unwrap();
    assert!(matches!(sender.pop_event(), Some(Event::Cancelled(1))));
    assert!(!sender.transmitted(&old.token, 1, 20).unwrap());
    let status = ArqStatus {
        partial_echo: echo(&old),
        received_frames: [true; crate::link::rach::ARQ_MAP_BITS],
    };
    assert!(!sender.status(&status, 1, 20).unwrap());
    assert!(sender.cancel(MobileIdentity::Min34(1)));
    assert!(matches!(sender.pop_event(), Some(Event::Cancelled(2))));
}
#[test]
fn echo_quarantine_and_event_backpressure_are_bounded() {
    let mut sender = Sender::try_new(1).unwrap();
    for number in 0..128 {
        sender
            .enqueue(
                Request {
                    identity: MobileIdentity::Min34(1),
                    channel: Channel::Arch,
                    message: vec![1],
                    completion: number,
                    deadline_ms: 6400,
                },
                0,
                0,
            )
            .unwrap();
        sender.cancel(MobileIdentity::Min34(1));
        assert!(matches!(sender.pop_event(), Some(Event::Cancelled(_))));
    }
    assert_eq!(
        sender.enqueue(
            Request {
                identity: MobileIdentity::Min34(1),
                channel: Channel::Arch,
                message: vec![1],
                completion: 129,
                deadline_ms: 6400
            },
            0,
            0
        ),
        Err(Error::EchoExhausted)
    );
    let now = 6400 + ECHO_GUARD_MS;
    sender
        .enqueue(
            Request {
                identity: MobileIdentity::Min34(1),
                channel: Channel::Arch,
                message: vec![1],
                completion: 130,
                deadline_ms: now + 6400,
            },
            now / 20,
            now,
        )
        .unwrap();
    sender.cancel(MobileIdentity::Min34(1));
    assert_eq!(
        sender.enqueue(
            Request {
                identity: MobileIdentity::Min34(1),
                channel: Channel::Arch,
                message: vec![1],
                completion: 131,
                deadline_ms: now + 6400
            },
            now / 20,
            now
        ),
        Err(Error::Capacity)
    );
}

#[test]
fn exhausted_restart_is_a_resource_failure_and_shutdown_cancels_outstanding_owners() {
    let mut sender = Sender::try_new(2).unwrap();
    enqueue(&mut sender, 1, vec![1]);
    for _ in 1..128 {
        enqueue(&mut sender, 2, vec![1]);
        assert!(sender.cancel(MobileIdentity::Min34(2)));
        sender.pop_event().unwrap();
    }
    send(&mut sender, 0);
    sender.advance(64, 1280).unwrap();
    assert!(matches!(
        sender.pop_event(),
        Some(Event::Failed {
            completion: 1,
            error: Error::EchoExhausted
        })
    ));
    assert_eq!(sender.pending(), 0);
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![1]);
    enqueue(&mut sender, 2, vec![1]);
    let in_flight = sender.prepare(0, 0).unwrap().unwrap();
    sender.submitted(&in_flight.token, 0, 0).unwrap();
    sender.cancel_all();
    assert_eq!(sender.pending(), 0);
    assert!(!sender.transmitted(&in_flight.token, 0, 0).unwrap());
    let mut cancelled = Vec::new();
    while let Some(event) = sender.pop_event() {
        let Event::Cancelled(owner) = event else {
            panic!()
        };
        cancelled.push(owner);
    }
    cancelled.sort();
    assert_eq!(cancelled, [1, 2]);
}

#[test]
fn stale_actor_cancellation_cannot_cancel_a_replacement_for_the_same_phone() {
    let mut sender = new_sender();
    let old = sender
        .enqueue(
            Request {
                identity: MobileIdentity::Min34(1),
                channel: Channel::Arch,
                message: vec![1],
                completion: 1,
                deadline_ms: 6400,
            },
            0,
            0,
        )
        .unwrap();
    let current = sender
        .enqueue(
            Request {
                identity: MobileIdentity::Min34(1),
                channel: Channel::Arch,
                message: vec![2],
                completion: 2,
                deadline_ms: 6400,
            },
            0,
            0,
        )
        .unwrap();
    assert_ne!(old, current);
    assert!(matches!(sender.pop_event(), Some(Event::Cancelled(1))));
    assert!(!sender.cancel_transaction(&old));
    assert_eq!(sender.pending(), 1);
    send(&mut sender, 0);
    sender.advance(64, 1280).unwrap();
    assert!(sender.cancel_transaction(&current));
    assert!(matches!(sender.pop_event(), Some(Event::Cancelled(2))));
}

#[test]
fn polling_refresh_does_not_postpone_missing_status_recovery() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![1; 20]);
    let mut last = None;
    let mut block = 0;
    while sender.prepare(block, block * 20).unwrap().is_some() {
        last = Some(send(&mut sender, block));
        block += 1;
    }
    let old = last.unwrap();
    let waiting_since = block - 1;
    for period in [16, 32, 48] {
        let at = waiting_since + period;
        sender.refresh_waiting(at, at * 20).unwrap();
        let refresh = send(&mut sender, at);
        assert_eq!(echo(&old), echo(&refresh));
        assert!(matches!(
            arq::decode(&refresh.information).unwrap(),
            ArqFrame::Continue {
                header: ArqHeader { polling: true, .. },
                ..
            }
        ));
    }
    let at = waiting_since + 64;
    sender.refresh_waiting(at, at * 20).unwrap();
    let frame = sender.prepare(at, at * 20).unwrap().unwrap();
    assert_ne!(echo(&old), echo(&frame));
    assert!(matches!(
        arq::decode(&frame.information).unwrap(),
        ArqFrame::Begin { .. }
    ));
}

#[test]
fn refresh_window_bounds_silence_for_sixteen_peers_without_extending_expiry() {
    let mut sender = Sender::try_new(REFRESH_CAPACITY).unwrap();
    for identity in 1..=REFRESH_CAPACITY as u64 {
        enqueue(&mut sender, identity, vec![1; 20]);
    }
    let mut last = std::collections::HashMap::new();
    for block in 0..320 {
        sender.refresh_waiting(block, block * 20).unwrap();
        if sender.prepare(block, block * 20).unwrap().is_some() {
            let frame = send(&mut sender, block);
            if let Some(previous) = last.insert(echo(&frame), block) {
                assert!(block - previous - 1 < FULL_RATE_INTERRUPTION_FRAMES as u64);
            }
        }
    }
    sender.advance(320, 6400).unwrap();
    let mut expired = 0;
    while let Some(event) = sender.pop_event() {
        assert!(matches!(event, Event::Expired(_)));
        expired += 1;
    }
    assert_eq!(expired, REFRESH_CAPACITY);
    assert_eq!(sender.pending(), 0);
}

#[test]
fn message_progress_distinguishes_all_submitted_from_all_host_receipts_and_stale_tokens() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![1; 40]);
    let mut frames = Vec::new();
    let mut block = 0;
    while let Some(frame) = sender.prepare(block, block * 20).unwrap() {
        assert!(sender.submitted_message(&frame.token).is_none());
        sender.submitted(&frame.token, block, block * 20).unwrap();
        frames.push(frame);
        block += 1;
    }
    assert_eq!(
        sender.submitted_message(&frames.last().unwrap().token),
        Some(&1)
    );
    let mut complete = Vec::new();
    for frame in frames.iter().rev().skip(1) {
        sender
            .transmitted_with(&frame.token, block, block * 20, |owner| {
                complete.push(*owner)
            })
            .unwrap();
    }
    assert!(complete.is_empty());
    sender
        .transmitted_with(&frames.last().unwrap().token, block, block * 20, |owner| {
            complete.push(*owner)
        })
        .unwrap();
    assert_eq!(complete, vec![1]);
    assert!(
        !sender
            .transmitted_with(
                &frames.last().unwrap().token,
                block,
                block * 20,
                |_| panic!("duplicate receipt")
            )
            .unwrap()
    );
    sender.cancel(MobileIdentity::Min34(1));
    assert!(sender.submitted_message(&frames[0].token).is_none());
    assert!(
        !sender
            .transmitted_with(&frames[0].token, block, block * 20, |_| panic!(
                "cancelled receipt"
            ))
            .unwrap()
    );
}

#[test]
fn restarted_sender_rejects_old_status_and_receipts_until_the_radio_quiet_boundary() {
    let config = crate::config::Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let crate::l3::broadcast::MandatoryBroadcast::AccessParameters(parameters) =
        crate::runtime::serving::messages(&config)[1].clone()
    else {
        panic!()
    };
    let policy = reuse::ReusePolicy::full_rate_contention(&parameters).unwrap();
    let mut previous = new_sender();
    enqueue(&mut previous, 1, vec![1; 20]);
    let old = send(&mut previous, 0);
    let status = ArqStatus {
        partial_echo: echo(&old),
        received_frames: [true; 26],
    };
    let mut restarted = Sender::after_restart(1, policy).unwrap();
    assert!(!restarted.status(&status, 0, 0).unwrap());
    assert!(!restarted.can_enqueue(policy.quiet_blocks() - 1, policy.quiet_ms * 10));
    assert!(
        !restarted
            .status(&status, policy.quiet_blocks() - 1, policy.quiet_ms * 10)
            .unwrap()
    );
    assert!(
        restarted
            .prepare(policy.quiet_blocks() - 1, policy.quiet_ms * 10)
            .unwrap()
            .is_none()
    );
    assert!(restarted.can_enqueue(policy.quiet_blocks(), policy.quiet_ms * 10));
    restarted
        .enqueue(
            Request {
                identity: MobileIdentity::Min34(2),
                channel: Channel::Smsch,
                message: vec![2; 20],
                completion: 2,
                deadline_ms: policy.quiet_ms * 10 + super::super::timers::SPACH_MS,
            },
            policy.quiet_blocks(),
            policy.quiet_ms * 10,
        )
        .unwrap();
    assert!(
        !restarted
            .transmitted(&old.token, policy.quiet_blocks(), policy.quiet_ms * 10)
            .unwrap()
    );
    let current = restarted
        .prepare(policy.quiet_blocks(), policy.quiet_ms * 10)
        .unwrap()
        .unwrap();
    assert_eq!(echo(&current), echo(&old));
    assert!(restarted.pop_event().is_none());
    assert_eq!(restarted.pending(), 1);
}

#[test]
fn interruption_restarts_before_the_tdmas_inactivity_restart_and_preserves_the_owner() {
    let mut sender = new_sender();
    let owner = sender
        .enqueue(
            Request {
                identity: MobileIdentity::Min34(1),
                channel: Channel::Smsch,
                message: vec![1; 40],
                completion: 1,
                deadline_ms: 6400,
            },
            0,
            0,
        )
        .unwrap();
    let old = send(&mut sender, 0);
    assert!(sender.interrupt(echo(&old), 34, 680).unwrap());
    let fresh = sender.prepare(34, 680).unwrap().unwrap();
    assert_ne!(echo(&old), echo(&fresh));
    assert!(matches!(
        arq::decode(&fresh.information).unwrap(),
        ArqFrame::Begin { .. }
    ));
    assert!(!sender.transmitted(&old.token, 34, 680).unwrap());
    assert!(
        !sender
            .status(
                &ArqStatus {
                    partial_echo: echo(&old),
                    received_frames: [true; 26]
                },
                34,
                680
            )
            .unwrap()
    );
    assert!(sender.cancel_transaction(&owner));
    assert!(matches!(sender.pop_event(), Some(Event::Cancelled(1))));
    assert!(sender.pop_event().is_none());
}
#[test]
fn priority_refresh_replaces_only_an_unsubmitted_selection_and_keeps_receipt_ownership() {
    let mut sender = new_sender();
    enqueue(&mut sender, 1, vec![1; 40]);
    enqueue(&mut sender, 2, vec![2; 40]);
    let one = send(&mut sender, 0);
    let two = send(&mut sender, 1);
    let deferred = sender.prepare(2, 40).unwrap().unwrap();
    assert_eq!(echo(&deferred), echo(&one));
    let priority = sender
        .prepare_prioritized(echo(&two), 2, 40)
        .unwrap()
        .unwrap();
    assert_eq!(echo(&priority), echo(&two));
    assert!(matches!(
        sender.submitted(&deferred.token, 2, 40),
        Err(Error::Stale)
    ));
    sender.submitted(&priority.token, 2, 40).unwrap();
    assert!(sender.transmitted(&priority.token, 2, 40).unwrap());
    assert!(!sender.transmitted(&deferred.token, 2, 40).unwrap());
    sender.cancel(MobileIdentity::Min34(2));
    assert!(!sender.has_echo(echo(&two)));
    assert!(sender.has_echo(echo(&one)));
    assert!(matches!(sender.pop_event(), Some(Event::Cancelled(2))));
}
