use super::*;
use crate::{
    config::Config, l3::designation::AssignedSlots, phy::channel::Band,
    scheduler::broadcast::Position,
};
const UNIX_MS: i64 = 1_000_000;
fn desired(sid: u16) -> BroadcastContent {
    let config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let mut messages = crate::runtime::serving::messages(&config);
    let MandatoryBroadcast::SystemIdentity(identity) = &mut messages[4] else {
        panic!()
    };
    identity.sid = sid;
    BroadcastContent::try_new(AssignedSlots::Full14, messages, vec![]).unwrap()
}
fn clock() -> BroadcastClock {
    BroadcastClock::try_new(Position {
        hyperframe_counter: 0,
        superframe: Superframe::Primary,
    })
    .unwrap()
}
fn cell() -> CellKey {
    CellKey::try_new(Band::Cellular800, 991).unwrap()
}
fn half(session: &mut BroadcastSession, store: &mut StateStore, sf: u64, unix_origin: i64) -> bool {
    let mut confirmed = false;
    for phase in 0..crate::scheduler::broadcast::CONTROL_SLOTS {
        let slot = sf * PHYSICAL_SLOTS + phase * CONTROL_PERIOD;
        let now_ms = slot * SUPERFRAME_MS / PHYSICAL_SLOTS;
        confirmed |= session
            .completed(store, slot, now_ms, unix_origin + now_ms as i64)
            .unwrap()
            .confirmed;
    }
    confirmed
}
#[test]
fn startup_and_recovery_require_fresh_complete_paired_coverage() {
    let mut store = StateStore::open(":memory:").unwrap();
    let mut session =
        BroadcastSession::try_new(&mut store, cell(), clock(), desired(42), UNIX_MS, 0).unwrap();
    assert!(session.serving_allowed());
    assert!(!half(&mut session, &mut store, 0, UNIX_MS));
    assert!(store.broadcast_recovery(cell()).unwrap().pending.is_some());
    drop(session);
    let restart = UNIX_MS + 5000;
    let mut session =
        BroadcastSession::try_new(&mut store, cell(), clock(), desired(42), restart, 0).unwrap();
    assert!(!half(&mut session, &mut store, 0, restart));
    assert!(half(&mut session, &mut store, 1, restart));
    let confirmed = store.broadcast_recovery(cell()).unwrap().confirmed.unwrap();
    drop(session);
    let mut session =
        BroadcastSession::try_new(&mut store, cell(), clock(), desired(42), restart + 5000, 0)
            .unwrap();
    assert!(!half(&mut session, &mut store, 0, restart + 5000));
    assert!(!half(&mut session, &mut store, 1, restart + 5000));
    assert!(
        store
            .broadcast_recovery(cell())
            .unwrap()
            .confirmed
            .unwrap()
            .same_revision(&confirmed)
    );
}
#[test]
fn changed_configuration_announces_then_activates_at_the_fixed_boundary() {
    let mut store = StateStore::open(":memory:").unwrap();
    let mut initial =
        BroadcastSession::try_new(&mut store, cell(), clock(), desired(42), UNIX_MS, 0).unwrap();
    half(&mut initial, &mut store, 0, UNIX_MS);
    half(&mut initial, &mut store, 1, UNIX_MS);
    drop(initial);
    let restart = UNIX_MS + 10000;
    let mut session =
        BroadcastSession::try_new(&mut store, cell(), clock(), desired(43), restart, 0).unwrap();
    assert!(!session.serving_allowed());
    assert!(session.plan.at(0).signaling.notification);
    assert!(!session.plan.at(1).signaling.changes.fbcch);
    assert!(session.plan.at(2).signaling.changes.fbcch);
    assert!(!half(&mut session, &mut store, 0, restart));
    assert!(!half(&mut session, &mut store, 1, restart));
    session.invalidate_through(2);
    assert!(!half(&mut session, &mut store, 2, restart));
    assert!(session.serving_allowed());
    assert!(!half(&mut session, &mut store, 3, restart));
    assert!(!half(&mut session, &mut store, 4, restart));
    assert!(half(&mut session, &mut store, 5, restart));
    assert!(store.broadcast_recovery(cell()).unwrap().pending.is_none());
}
#[test]
fn paging_class_periods_follow_the_normative_non_power_of_two_table() {
    let expected = [1, 2, 3, 6, 12, 24, 48, 96];
    for (index, period_hf) in expected.into_iter().enumerate() {
        assert_eq!(
            period_class(index as u8 + 1).unwrap(),
            period_hf * 2 * SUPERFRAME_MS
        );
    }
}

#[test]
fn recovered_pending_revision_finishes_before_a_new_desired_revision() {
    let mut store = StateStore::open(":memory:").unwrap();
    let initial =
        BroadcastSession::try_new(&mut store, cell(), clock(), desired(42), UNIX_MS, 0).unwrap();
    let pending = initial.revision(0).clone();
    drop(initial);
    let restart = UNIX_MS + 5000;
    let mut session =
        BroadcastSession::try_new(&mut store, cell(), clock(), desired(43), restart, 0).unwrap();
    assert!(session.revision(0).same_revision(&pending));
    assert!(!session.serving_allowed());
    assert!(!session.advance(&mut store, 0, 0, restart).unwrap());
    assert!(!half(&mut session, &mut store, 0, restart));
    assert!(half(&mut session, &mut store, 1, restart));
    let now_ms = session.last_link_ms;
    let eligible = now_ms + period_class(1).unwrap();
    assert!(
        !session
            .advance(
                &mut store,
                2 * PHYSICAL_SLOTS,
                eligible - 1,
                restart + eligible as i64 - 1
            )
            .unwrap()
    );
    assert!(store.broadcast_recovery(cell()).unwrap().pending.is_none());
    assert!(
        session
            .advance(
                &mut store,
                4 * PHYSICAL_SLOTS,
                eligible,
                restart + eligible as i64
            )
            .unwrap()
    );
    assert!(session.revision(3).same_revision(&pending));
    assert!(!session.revision(6).same_revision(&pending));
    assert!(!half(&mut session, &mut store, 4, restart));
    assert!(!half(&mut session, &mut store, 5, restart));
    assert!(!half(&mut session, &mut store, 6, restart));
    assert!(half(&mut session, &mut store, 7, restart));
    assert!(session.serving_allowed());
    let latest = store.broadcast_recovery(cell()).unwrap().confirmed.unwrap();
    assert!(
        store
            .broadcast_predecessor(&latest)
            .unwrap()
            .unwrap()
            .same_revision(&pending)
    );
}

#[test]
fn clock_regression_does_not_prepare_a_new_revision() {
    let mut store = StateStore::open(":memory:").unwrap();
    let mut session =
        BroadcastSession::try_new(&mut store, cell(), clock(), desired(42), UNIX_MS, 0).unwrap();
    let pending = session.revision(0).clone();
    session.advance(&mut store, 0, 10, UNIX_MS + 10).unwrap();
    assert!(matches!(
        session.advance(&mut store, 0, 9, UNIX_MS + 11),
        Err(Error::Clock)
    ));
    assert!(matches!(
        session.advance(&mut store, 0, 11, UNIX_MS + 9),
        Err(Error::Clock)
    ));
    assert!(
        store
            .broadcast_recovery(cell())
            .unwrap()
            .pending
            .unwrap()
            .same_revision(&pending)
    );
}

#[test]
fn restart_preserves_remaining_cadence_with_a_nonzero_radio_epoch() {
    let mut store = StateStore::open(":memory:").unwrap();
    let first = store
        .prepare_broadcast_revision(
            cell(),
            &desired(42).storage_bytes().unwrap(),
            BroadcastSignaling {
                changes: BroadcastChanges {
                    fbcch: false,
                    ebcch: false,
                },
                notification: false,
            },
            UNIX_MS,
        )
        .unwrap();
    store.confirm_broadcast_revision(&first, UNIX_MS).unwrap();
    let restart = UNIX_MS + 1;
    let mut session =
        BroadcastSession::try_new(&mut store, cell(), clock(), desired(43), restart, 500).unwrap();
    let eligible = period_class(1).unwrap() - 1;
    assert!(
        !session
            .advance(
                &mut store,
                PHYSICAL_SLOTS,
                eligible - 1,
                restart + eligible as i64 - 1
            )
            .unwrap()
    );
    assert!(store.broadcast_recovery(cell()).unwrap().pending.is_none());
    assert!(
        session
            .advance(
                &mut store,
                PHYSICAL_SLOTS,
                eligible,
                restart + eligible as i64
            )
            .unwrap()
    );
    assert!(!session.plan.at(1).signaling.notification);
    assert!(session.plan.at(2).signaling.notification);
    assert!(!session.plan.at(3).signaling.changes.fbcch);
    assert!(session.plan.at(4).signaling.changes.fbcch);
}

#[test]
fn timezone_changes_notify_then_toggle_only_ec_at_the_fixed_boundary() {
    use crate::l3::time_date::TimeZoneOffset;
    let old = TimeZoneOffset::try_new(-300, false).unwrap();
    let new = TimeZoneOffset::try_new(-240, true).unwrap();
    let mut store = StateStore::open(":memory:").unwrap();
    let mut session = BroadcastSession::try_new(
        &mut store,
        cell(),
        clock(),
        desired(42).with_time_zone(old).unwrap(),
        UNIX_MS,
        0,
    )
    .unwrap();
    half(&mut session, &mut store, 0, UNIX_MS);
    assert!(half(&mut session, &mut store, 1, UNIX_MS));
    let eligible = session.next_change_ms;
    assert!(session.set_time_zone(new).unwrap());
    assert!(
        session
            .advance(
                &mut store,
                4 * PHYSICAL_SLOTS,
                eligible,
                UNIX_MS + eligible as i64
            )
            .unwrap()
    );
    assert_eq!(session.content(5).time_zone(), Some(old));
    assert_eq!(session.content(6).time_zone(), Some(new));
    assert!(session.plan.at(4).signaling.notification);
    assert!(!session.plan.at(5).signaling.changes.ebcch);
    assert!(session.plan.at(6).signaling.changes.ebcch);
    assert!(!session.plan.at(6).signaling.changes.fbcch);
    assert!(!half(&mut session, &mut store, 4, UNIX_MS));
    assert!(!half(&mut session, &mut store, 5, UNIX_MS));
    assert!(!half(&mut session, &mut store, 6, UNIX_MS));
    assert!(half(&mut session, &mut store, 7, UNIX_MS));
    assert!(session.serving_allowed());
}

#[test]
fn multi_superframe_neighbor_cycle_cannot_confirm_after_partial_or_missing_frames() {
    let neighbors: Vec<_> = (1..=24)
        .map(crate::l3::neighbors::tests::neighbor)
        .collect();
    let target = desired(42)
        .with_time_zone(crate::l3::time_date::TimeZoneOffset::try_new(0, false).unwrap())
        .unwrap()
        .with_neighbors(&neighbors)
        .unwrap();
    let mut store = StateStore::open(":memory:").unwrap();
    let mut session =
        BroadcastSession::try_new(&mut store, cell(), clock(), target, UNIX_MS, 0).unwrap();
    session.invalidate_through(0);
    assert!(!half(&mut session, &mut store, 1, UNIX_MS));
    assert!(store.broadcast_recovery(cell()).unwrap().pending.is_some());
    assert!(!half(&mut session, &mut store, 2, UNIX_MS));
    assert!(half(&mut session, &mut store, 3, UNIX_MS));
    let recovered = store.broadcast_recovery(cell()).unwrap();
    assert!(recovered.pending.is_none());
    let stored =
        BroadcastContent::from_storage_bytes(recovered.confirmed.unwrap().contents()).unwrap();
    assert_eq!(
        crate::l3::neighbors::decode(stored.neighbor_packet().unwrap()).unwrap(),
        neighbors
    );
}
