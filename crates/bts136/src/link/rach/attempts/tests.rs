use super::*;
use crate::link::rach::{self, MobileIdentity};
#[test]
fn interleaved_granted_attempts_reassemble_separately_and_keep_duplicate_state() {
    let identities = [MobileIdentity::Min34(1), MobileIdentity::Min34(2)];
    let messages = [vec![1u8; 60], vec![2u8; 60]];
    let frames: Vec<_> = identities
        .iter()
        .zip(&messages)
        .map(|(identity, message)| {
            rach::encode(BurstFormat::Normal, *identity, &[message]).unwrap()
        })
        .collect();
    let mut attempts = Attempts::try_new(2).unwrap();
    let handles = [
        attempts.open(0, 100).unwrap(),
        attempts.open(0, 100).unwrap(),
    ];
    let mut completed = Vec::new();
    assert_eq!(frames[0].len(), frames[1].len());
    for (index, (first, second)) in frames[0].iter().zip(&frames[1]).enumerate() {
        for (handle, frame) in handles.iter().zip([first, second]) {
            let outcome = attempts
                .push(handle, index as u64, BurstFormat::Normal, frame)
                .unwrap();
            if let Outcome::Complete(set) = outcome {
                completed.push(set);
            }
        }
    }
    assert_eq!(completed.len(), 2);
    for (set, (identity, message)) in completed.iter().zip(identities.iter().zip(&messages)) {
        assert_eq!(set.identity, *identity);
        assert_eq!(set.messages.as_slice(), std::slice::from_ref(message));
    }
    for party in 0..2 {
        assert_eq!(
            attempts
                .push(
                    &handles[party],
                    99,
                    BurstFormat::Normal,
                    frames[party].last().unwrap()
                )
                .unwrap(),
            Outcome::Duplicate(identities[party])
        );
    }
    assert_eq!(attempts.expire(100).unwrap(), 2);
    assert_eq!(attempts.active_attempts(), 0);
}
#[test]
fn capacity_expiry_and_reused_slots_reject_stale_or_foreign_handles() {
    let mut attempts = Attempts::try_new(1).unwrap();
    let first = attempts.open(0, 10).unwrap();
    assert!(matches!(attempts.open(1, 20), Err(Error::Capacity)));
    let second = attempts.open(10, 20).unwrap();
    assert!(matches!(attempts.retire(&first, 10), Err(Error::Handle)));
    let mut foreign = Attempts::try_new(1).unwrap();
    let other = foreign.open(10, 20).unwrap();
    assert!(matches!(attempts.retire(&other, 10), Err(Error::Handle)));
    assert!(matches!(
        attempts.push(&other, 1000, BurstFormat::Normal, &[]),
        Err(Error::Handle)
    ));
    assert_eq!(attempts.active_attempts(), 1);
    attempts.retire(&second, 10).unwrap();
    assert_eq!(attempts.active_attempts(), 0);
    assert!(matches!(attempts.open(9, 20), Err(Error::ClockRegression)));
    assert!(matches!(attempts.open(10, 10), Err(Error::Deadline)));
}
#[test]
fn invalid_frame_does_not_clear_another_attempt_and_discontinuity_invalidates_all_handles() {
    let mut attempts = Attempts::try_new(2).unwrap();
    let handles = [
        attempts.open(0, 100).unwrap(),
        attempts.open(0, 100).unwrap(),
    ];
    let identity = MobileIdentity::Min34(2);
    let message = vec![2u8; 60];
    let frames = rach::encode(BurstFormat::Abbreviated, identity, &[&message]).unwrap();
    assert!(matches!(
        attempts
            .push(&handles[1], 0, BurstFormat::Abbreviated, &frames[0])
            .unwrap(),
        Outcome::Pending(_)
    ));
    assert!(matches!(
        attempts.push(&handles[0], 1, BurstFormat::Normal, &[]),
        Err(Error::Link(RachLinkError::Length))
    ));
    let mut complete = None;
    for frame in &frames[1..] {
        if let Outcome::Complete(set) = attempts
            .push(&handles[1], 1, BurstFormat::Abbreviated, frame)
            .unwrap()
        {
            complete = Some(set);
        }
    }
    assert_eq!(complete.unwrap().messages, [message]);
    attempts.discontinuity();
    assert_eq!(attempts.active_attempts(), 0);
    assert!(matches!(
        attempts.retire(&handles[1], 1),
        Err(Error::Handle)
    ));
}
