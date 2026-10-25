use super::*;
use crate::{
    link::{
        feedback::{AccessState, ObservedFeedback},
        rach::{self, MobileIdentity},
    },
    phy::{
        rach::{self as physical, BurstFormat},
        sync::SyncWord,
    },
};
const DVCC: u8 = 23;
fn fields(frame: &[bool]) -> ReverseControlFields {
    physical::decode(
        &physical::encode(BurstFormat::Normal, SyncWord::One, frame, DVCC).unwrap(),
        BurstFormat::Normal,
        DVCC,
    )
    .unwrap()
}
#[test]
fn six_interleaved_paths_keep_fragments_and_echoes_separate() {
    let mut channels = AccessChannels::default();
    let identities: Vec<_> = (1..=SUBCHANNELS)
        .map(|value| MobileIdentity::Min34(value as u64))
        .collect();
    let messages: Vec<_> = (1..=SUBCHANNELS)
        .map(|value| vec![value as u8; 30])
        .collect();
    let frames: Vec<_> = identities
        .iter()
        .zip(&messages)
        .map(|(id, msg)| rach::encode(BurstFormat::Normal, *id, &[msg]).unwrap())
        .collect();
    let mut complete = Vec::new();
    for (round, _) in frames[0].iter().enumerate() {
        for path in 0..SUBCHANNELS {
            let block = (round * SUBCHANNELS + path) as u64;
            let result = channels
                .receive(
                    block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                    &fields(&frames[path][round]),
                    block * 3,
                )
                .unwrap();
            let feedback =
                ObservedFeedback::from(&channels.feedback((block + RESPONSE_BLOCK_OFFSET) * 3));
            assert_eq!(feedback.reception(), Reception::Received);
            assert!(
                feedback
                    .matches_partial_echo(identities[path].partial_echo())
                    .unwrap()
            );
            if let Outcome::Complete(set) = result {
                assert_eq!(set.identity, identities[path]);
                assert_eq!(set.messages, vec![messages[path].clone()]);
                complete.push(set);
            }
        }
    }
    assert_eq!(complete.len(), SUBCHANNELS);
}
#[test]
fn feedback_has_one_exact_response_slot_and_late_bursts_cannot_mutate_context() {
    let mut channels = AccessChannels::default();
    let frame = rach::encode(BurstFormat::Normal, MobileIdentity::Min34(3), &[&[7]])
        .unwrap()
        .remove(0);
    let field = fields(&frame);
    assert!(matches!(
        channels.receive(-REVERSE_OFFSET_SYMBOLS, &field, 7),
        Err(Error::LateFeedback)
    ));
    assert!(matches!(
        channels.receive(-REVERSE_OFFSET_SYMBOLS + 20.0, &field, 0),
        Err(Error::Timing {
            error_symbols: 20.0,
            ..
        })
    ));
    assert!(matches!(
        channels
            .receive(-REVERSE_OFFSET_SYMBOLS, &field, 0)
            .unwrap(),
        Outcome::Complete(_)
    ));
    let expected = channels.feedback(6);
    let feedback = ObservedFeedback::from(&expected);
    assert_eq!(feedback.reception(), Reception::Received);
    assert!(feedback.matches_partial_echo(3).unwrap());
    let idle = channels.feedback(24);
    let feedback = ObservedFeedback::from(&idle);
    assert_eq!(feedback.reception(), Reception::NotReceived);
    assert_eq!(
        feedback.availability(AccessState::StartRandomAccess),
        Some(Availability::Idle)
    );
}

#[test]
fn completed_retry_is_deduplicated_but_a_later_access_can_register_again() {
    let mut channels = AccessChannels::default();
    let frame = rach::encode(BurstFormat::Normal, MobileIdentity::Min34(3), &[&[12]])
        .unwrap()
        .remove(0);
    let field = fields(&frame);
    assert!(matches!(
        channels
            .receive(-REVERSE_OFFSET_SYMBOLS, &field, 0)
            .unwrap(),
        Outcome::Complete(_)
    ));
    assert!(matches!(
        channels
            .receive(6.0 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS, &field, 0)
            .unwrap(),
        Outcome::Duplicate(_)
    ));
    assert!(matches!(
        channels
            .receive(18.0 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS, &field, 0)
            .unwrap(),
        Outcome::Complete(_)
    ));
}

#[test]
fn repeated_physical_block_cannot_advance_reassembly_or_replace_feedback() {
    let mut channels = AccessChannels::default();
    let identity = MobileIdentity::Min34(3);
    let message = vec![12; 30];
    let frames = rach::encode(BurstFormat::Normal, identity, &[&message]).unwrap();
    let first = fields(&frames[0]);
    assert!(matches!(
        channels
            .receive(-REVERSE_OFFSET_SYMBOLS, &first, 0)
            .unwrap(),
        Outcome::Pending(_)
    ));
    let other = rach::encode(BurstFormat::Normal, MobileIdentity::Min34(4), &[&[7]])
        .unwrap()
        .remove(0);
    for repeated in [&first, &fields(&frames[1]), &fields(&other)] {
        assert!(matches!(
            channels.receive(-REVERSE_OFFSET_SYMBOLS, repeated, 0),
            Err(Error::RepeatedBlock {
                block: 0,
                previous_block: 0
            })
        ));
    }
    let feedback = ObservedFeedback::from(&channels.feedback(RESPONSE_BLOCK_OFFSET * 3));
    assert_eq!(feedback.reception(), Reception::Received);
    assert!(
        feedback
            .matches_partial_echo(identity.partial_echo())
            .unwrap()
    );
    let mut complete = None;
    for (index, frame) in frames.iter().enumerate().skip(1) {
        let block = (index * SUBCHANNELS) as u64;
        let outcome = channels
            .receive(
                block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                &fields(frame),
                block * 3,
            )
            .unwrap();
        if let Outcome::Complete(set) = outcome {
            assert!(complete.replace(set).is_none());
        }
    }
    let complete = complete.unwrap();
    assert_eq!(complete.identity, identity);
    assert_eq!(complete.messages, vec![message]);
}

#[test]
fn maximum_length_access_survives_permitted_repetition_gaps() {
    const MAX_MESSAGES: usize = 8;
    const MAX_MESSAGE_OCTETS: usize = 255;
    const MAX_REPETITIONS: usize = 3;
    let messages = vec![vec![12; MAX_MESSAGE_OCTETS]; MAX_MESSAGES];
    let slices: Vec<_> = messages.iter().map(Vec::as_slice).collect();
    let identity = MobileIdentity::Imsi50(3);
    let frames = rach::encode(BurstFormat::Normal, identity, &slices).unwrap();
    for gap_cycles in [1, MAX_REPETITIONS + 1] {
        let mut channels = AccessChannels::default();
        let mut complete = None;
        for (index, frame) in frames.iter().enumerate() {
            let block = (index * SUBCHANNELS * gap_cycles) as u64;
            let outcome = channels
                .receive(
                    block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                    &fields(frame),
                    block * 3,
                )
                .unwrap();
            if let Outcome::Complete(set) = outcome {
                assert!(complete.replace(set).is_none());
            }
        }
        let complete = complete.unwrap();
        assert_eq!(complete.identity, identity);
        assert_eq!(complete.messages, messages);
    }
}

#[test]
fn abandoned_access_expires_after_all_permitted_burst_occurrences() {
    const EXPIRED_CYCLES: usize = 5;
    let mut channels = AccessChannels::default();
    let frames = rach::encode(BurstFormat::Normal, MobileIdentity::Min34(3), &[&[12; 30]]).unwrap();
    assert!(matches!(
        channels
            .receive(-REVERSE_OFFSET_SYMBOLS, &fields(&frames[0]), 0)
            .unwrap(),
        Outcome::Pending(_)
    ));
    let block = (EXPIRED_CYCLES * SUBCHANNELS) as u64;
    assert!(matches!(
        channels.receive(
            block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
            &fields(&frames[1]),
            block * 3,
        ),
        Err(Error::Link(RachLinkError::MissingBegin))
    ));
    let restart = block + SUBCHANNELS as u64;
    assert!(matches!(
        channels
            .receive(
                restart as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                &fields(&frames[0]),
                restart * 3,
            )
            .unwrap(),
        Outcome::Pending(_)
    ));
}

#[test]
fn completed_multiburst_retries_keep_the_original_expiry() {
    const MAX_REPETITIONS: usize = 3;
    let identity = MobileIdentity::Min34(3);
    let message = vec![12; 30];
    let frames = rach::encode(BurstFormat::Normal, identity, &[&message]).unwrap();
    for first_repeat in 1..=MAX_REPETITIONS {
        let mut channels = AccessChannels::default();
        for (index, frame) in frames.iter().enumerate() {
            let block = (index * SUBCHANNELS) as u64;
            channels
                .receive(
                    block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                    &fields(frame),
                    block * 3,
                )
                .unwrap();
        }
        let completed = ((frames.len() - 1) * SUBCHANNELS) as u64;
        let last = fields(frames.last().unwrap());
        for repetition in first_repeat..=MAX_REPETITIONS {
            let block = completed + (repetition * SUBCHANNELS) as u64;
            assert!(matches!(channels.receive(
                block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                &last, block * 3,
            ).unwrap(), Outcome::Duplicate(id) if id == identity));
            let feedback =
                ObservedFeedback::from(&channels.feedback((block + RESPONSE_BLOCK_OFFSET) * 3));
            assert_eq!(feedback.reception(), Reception::Received);
            assert_eq!(
                feedback.availability(AccessState::StartRandomAccess),
                Some(Availability::Idle)
            );
            assert!(
                feedback
                    .matches_partial_echo(identity.partial_echo())
                    .unwrap()
            );
        }
        let expired = completed + ((MAX_REPETITIONS + 1) * SUBCHANNELS) as u64;
        assert!(matches!(
            channels.receive(
                expired as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                &last,
                expired * 3,
            ),
            Err(Error::Link(RachLinkError::MissingBegin))
        ));
        let restart = expired + SUBCHANNELS as u64;
        assert!(matches!(channels.receive(
            restart as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
            &fields(&frames[0]), restart * 3,
        ).unwrap(), Outcome::Pending(id) if id == identity));
    }
}

#[test]
fn single_burst_replacement_does_not_inherit_multiburst_retention() {
    let mut channels = AccessChannels::default();
    let partial =
        rach::encode(BurstFormat::Normal, MobileIdentity::Min34(3), &[&[12; 30]]).unwrap();
    let single = rach::encode(BurstFormat::Normal, MobileIdentity::Min34(4), &[&[7]]).unwrap();
    assert!(matches!(
        channels
            .receive(-REVERSE_OFFSET_SYMBOLS, &fields(&partial[0]), 0)
            .unwrap(),
        Outcome::Pending(_)
    ));
    for (cycle, duplicate) in [(1, false), (2, true), (4, false)] {
        let block = (cycle * SUBCHANNELS) as u64;
        let outcome = channels
            .receive(
                block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                &fields(&single[0]),
                block * 3,
            )
            .unwrap();
        if duplicate {
            assert!(matches!(
                outcome,
                Outcome::Duplicate(MobileIdentity::Min34(4))
            ));
        } else {
            let Outcome::Complete(set) = outcome else {
                panic!("new single-burst access was suppressed")
            };
            assert_eq!(set.identity, MobileIdentity::Min34(4));
            assert_eq!(set.messages, vec![vec![7]]);
        }
    }
}

#[test]
fn missing_intermediate_bursts_keep_busy_feedback_until_the_attempt_expires() {
    let mut channels = AccessChannels::default();
    let frames = rach::encode(BurstFormat::Normal, MobileIdentity::Min34(3), &[&[12; 30]]).unwrap();
    channels
        .receive(-REVERSE_OFFSET_SYMBOLS, &fields(&frames[0]), 0)
        .unwrap();
    let initial = ObservedFeedback::from(&channels.feedback(RESPONSE_BLOCK_OFFSET * 3));
    assert_eq!(initial.reception(), Reception::Received);
    assert_eq!(
        initial.availability(AccessState::MoreBursts),
        Some(Availability::Busy)
    );
    for occurrence in 1..=MAX_BURST_OCCURRENCES {
        let block = occurrence * SUBCHANNELS as u64;
        let missed =
            ObservedFeedback::from(&channels.feedback((block + RESPONSE_BLOCK_OFFSET) * 3));
        assert_eq!(missed.reception(), Reception::NotReceived);
        assert_eq!(
            missed.availability(AccessState::MoreBursts),
            Some(Availability::Busy)
        );
    }
    let expired = (MAX_BURST_OCCURRENCES + 1) * SUBCHANNELS as u64;
    let idle = ObservedFeedback::from(&channels.feedback((expired + RESPONSE_BLOCK_OFFSET) * 3));
    assert_eq!(idle.reception(), Reception::NotReceived);
    assert_eq!(
        idle.availability(AccessState::StartRandomAccess),
        Some(Availability::Idle)
    );
    let continuation = expired + SUBCHANNELS as u64;
    assert!(matches!(
        channels.receive(
            continuation as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
            &fields(&frames[1]),
            continuation * 3,
        ),
        Err(Error::Link(RachLinkError::MissingBegin))
    ));
    let restart = continuation + SUBCHANNELS as u64;
    assert!(matches!(
        channels
            .receive(
                restart as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                &fields(&frames[0]),
                restart * 3
            )
            .unwrap(),
        Outcome::Pending(_)
    ));
}

#[test]
fn configured_repetition_limits_survive_discontinuity_and_drive_tick_expiry() {
    assert!(matches!(
        AccessChannels::try_new(MAX_REPETITIONS + 1),
        Err(Error::Limits)
    ));
    let frames = rach::encode(BurstFormat::Normal, MobileIdentity::Min34(3), &[&[12; 30]]).unwrap();
    for repetitions in 0..=MAX_REPETITIONS {
        for reset in [false, true] {
            let mut channels = AccessChannels::try_new(repetitions).unwrap();
            if reset {
                channels.discontinuity();
            }
            channels
                .receive(-REVERSE_OFFSET_SYMBOLS, &fields(&frames[0]), 0)
                .unwrap();
            let occurrences = u64::from(repetitions) + 1;
            for occurrence in 1..=occurrences {
                let block = occurrence * SUBCHANNELS as u64;
                let feedback =
                    ObservedFeedback::from(&channels.feedback((block + RESPONSE_BLOCK_OFFSET) * 3));
                assert_eq!(feedback.reception(), Reception::NotReceived);
                assert_eq!(
                    feedback.availability(AccessState::MoreBursts),
                    Some(Availability::Busy)
                );
            }
            let expired = (occurrences + 1) * SUBCHANNELS as u64;
            let feedback =
                ObservedFeedback::from(&channels.feedback((expired + RESPONSE_BLOCK_OFFSET) * 3));
            assert_eq!(
                feedback.availability(AccessState::StartRandomAccess),
                Some(Availability::Idle)
            );
            let continuation = expired + SUBCHANNELS as u64;
            assert!(matches!(
                channels.receive(
                    continuation as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                    &fields(&frames[1]),
                    continuation * 3,
                ),
                Err(Error::Link(RachLinkError::MissingBegin))
            ));
            let restart = continuation + SUBCHANNELS as u64;
            for (index, frame) in frames.iter().enumerate() {
                let block = restart + (index * SUBCHANNELS) as u64;
                channels
                    .receive(
                        block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                        &fields(frame),
                        block * 3,
                    )
                    .unwrap();
            }
            let completed = restart + ((frames.len() - 1) * SUBCHANNELS) as u64;
            let last = fields(frames.last().unwrap());
            if repetitions > 0 {
                let repeat = completed + u64::from(repetitions) * SUBCHANNELS as u64;
                assert!(matches!(
                    channels
                        .receive(
                            repeat as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                            &last,
                            repeat * 3
                        )
                        .unwrap(),
                    Outcome::Duplicate(_)
                ));
            }
            let expired = completed + occurrences * SUBCHANNELS as u64;
            assert!(matches!(
                channels.receive(
                    expired as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                    &last,
                    expired * 3
                ),
                Err(Error::Link(RachLinkError::MissingBegin))
            ));
        }
    }
}

#[test]
fn a_missed_intermediate_burst_can_be_retried_and_complete_once() {
    const SERVING_REPETITIONS: u8 = 2;
    let mut channels = AccessChannels::try_new(SERVING_REPETITIONS).unwrap();
    let identity = MobileIdentity::Min34(3);
    let message = vec![12; 30];
    let frames = rach::encode(BurstFormat::Normal, identity, &[&message]).unwrap();
    channels
        .receive(-REVERSE_OFFSET_SYMBOLS, &fields(&frames[0]), 0)
        .unwrap();
    let missed_block = SUBCHANNELS as u64;
    let feedback =
        ObservedFeedback::from(&channels.feedback((missed_block + RESPONSE_BLOCK_OFFSET) * 3));
    assert_eq!(feedback.reception(), Reception::NotReceived);
    assert_eq!(
        feedback.availability(AccessState::MoreBursts),
        Some(Availability::Busy)
    );
    let mut complete = None;
    for (index, frame) in frames.iter().enumerate().skip(1) {
        let block = ((index + 1) * SUBCHANNELS) as u64;
        let outcome = channels
            .receive(
                block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                &fields(frame),
                block * 3,
            )
            .unwrap();
        if let Outcome::Complete(set) = outcome {
            assert!(complete.replace(set).is_none());
        }
        let feedback =
            ObservedFeedback::from(&channels.feedback((block + RESPONSE_BLOCK_OFFSET) * 3));
        assert_eq!(feedback.reception(), Reception::Received);
        assert!(
            feedback
                .matches_partial_echo(identity.partial_echo())
                .unwrap()
        );
    }
    let complete = complete.unwrap();
    assert_eq!(complete.identity, identity);
    assert_eq!(complete.messages, vec![message]);
}

#[test]
fn receive_assembly_ownership_clears_on_completion_and_passive_expiry() {
    let identity = MobileIdentity::Min34(3);
    let other = MobileIdentity::Min34(4);
    let frames = rach::encode(BurstFormat::Normal, identity, &[&[12; 30]]).unwrap();
    for finish in [false, true] {
        let mut channels = AccessChannels::default();
        assert!(!channels.retains(identity));
        channels
            .receive(-REVERSE_OFFSET_SYMBOLS, &fields(&frames[0]), 0)
            .unwrap();
        assert!(channels.retains(identity));
        assert!(!channels.retains(other));
        if finish {
            for (index, frame) in frames.iter().enumerate().skip(1) {
                let block = (index * SUBCHANNELS) as u64;
                channels
                    .receive(
                        block as f64 * SYMBOLS_PER_BLOCK - REVERSE_OFFSET_SYMBOLS,
                        &fields(frame),
                        block * 3,
                    )
                    .unwrap();
            }
        } else {
            let expired = (MAX_BURST_OCCURRENCES + 1) * SUBCHANNELS as u64;
            channels.feedback((expired + RESPONSE_BLOCK_OFFSET) * 3);
        }
        assert!(!channels.retains(identity));
    }
}

#[test]
fn timing_diagnostics_preserve_the_boundary_and_do_not_consume_a_rejected_block() {
    let frame = rach::encode(BurstFormat::Normal, MobileIdentity::Min34(3), &[&[7]])
        .unwrap()
        .remove(0);
    let field = fields(&frame);
    let mut channels = AccessChannels::default();
    let error = channels
        .receive(-REVERSE_OFFSET_SYMBOLS - 20.0, &field, 0)
        .unwrap_err();
    let Error::Timing {
        relative_symbols,
        nearest_block,
        error_symbols,
        tolerance_symbols,
    } = error
    else {
        panic!("{error}")
    };
    assert_eq!(relative_symbols, -REVERSE_OFFSET_SYMBOLS - 20.0);
    assert_eq!(nearest_block, 0);
    assert!((error_symbols + 20.0).abs() < 1e-9);
    assert_eq!(tolerance_symbols, ALIGNMENT_TOLERANCE_SYMBOLS);
    assert!(matches!(
        channels
            .receive(
                -REVERSE_OFFSET_SYMBOLS + ALIGNMENT_TOLERANCE_SYMBOLS,
                &field,
                0
            )
            .unwrap(),
        Outcome::Complete(_)
    ));
    assert!(matches!(
        channels.receive(-REVERSE_OFFSET_SYMBOLS, &field, 0),
        Err(Error::RepeatedBlock {
            block: 0,
            previous_block: 0
        })
    ));
}
