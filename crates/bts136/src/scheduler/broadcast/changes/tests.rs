use super::*;
use crate::{
    config::Config,
    l3::designation::AssignedSlots,
    link::{
        rach::MobileIdentity,
        spach::{self, Channel, HeaderFlags, SingleMessage},
    },
    phy::control::{
        self, INFORMATION_BITS, LEADING_FEEDBACK_BITS, LogicalChannel, SharedFeedback,
        TRAILING_FEEDBACK_BITS,
    },
    scheduler::broadcast::Position,
};
const BCN_BIT: usize = 4;
const FC_BIT: usize = 1;
const EC_BIT: usize = 2;

fn clock(counter: u8, superframe: Superframe) -> BroadcastClock {
    BroadcastClock::try_new(Position {
        hyperframe_counter: counter,
        superframe,
    })
    .unwrap()
}

#[test]
fn change_flags_advance_once_at_the_next_hyperframe_and_keep_their_new_values() {
    let clock = clock(11, Superframe::Primary);
    for fbcch in [false, true] {
        for ebcch in [false, true] {
            for notification in [false, true] {
                let previous = BroadcastSignaling {
                    changes: BroadcastChanges { fbcch, ebcch },
                    notification,
                };
                for scope in [
                    BroadcastScope::Fbcch,
                    BroadcastScope::Ebcch,
                    BroadcastScope::Both,
                ] {
                    let change = BroadcastChange::try_new(&clock, 2, previous, scope).unwrap();
                    assert_eq!(change.activation_superframe(), 4);
                    assert_eq!(
                        change.at(1),
                        BroadcastChangeState {
                            phase: ChangePhase::Current,
                            signaling: previous
                        }
                    );
                    for sf in [2, 3] {
                        let state = change.at(sf);
                        assert_eq!(state.phase, ChangePhase::Announced);
                        assert_eq!(state.signaling.changes, previous.changes);
                        assert_eq!(state.signaling.notification, !notification);
                    }
                    for sf in [4, 5, 24, u64::MAX] {
                        let state = change.at(sf);
                        assert_eq!(state.phase, ChangePhase::Applied);
                        assert_eq!(state.signaling.notification, !notification);
                        assert_eq!(
                            state.signaling.changes.fbcch,
                            fbcch ^ matches!(scope, BroadcastScope::Fbcch | BroadcastScope::Both)
                        );
                        assert_eq!(
                            state.signaling.changes.ebcch,
                            ebcch ^ matches!(scope, BroadcastScope::Ebcch | BroadcastScope::Both)
                        );
                    }
                }
            }
        }
    }
    let change = BroadcastChange::try_new(
        &clock,
        0,
        BroadcastSignaling {
            changes: BroadcastChanges {
                fbcch: false,
                ebcch: false,
            },
            notification: false,
        },
        BroadcastScope::Both,
    )
    .unwrap();
    assert_eq!(
        clock.at(change.activation_superframe()).hyperframe_counter,
        0
    );
}

#[test]
fn secondary_boundaries_and_overflow_cannot_schedule_a_partial_announcement() {
    let previous = BroadcastSignaling {
        changes: BroadcastChanges {
            fbcch: false,
            ebcch: false,
        },
        notification: false,
    };
    let primary = clock(0, Superframe::Primary);
    assert!(matches!(
        BroadcastChange::try_new(&primary, 1, previous, BroadcastScope::Both),
        Err(BroadcastChangeError::Boundary)
    ));
    assert!(matches!(
        BroadcastChange::try_new(&primary, u64::MAX - 1, previous, BroadcastScope::Both),
        Err(BroadcastChangeError::CounterOverflow)
    ));
    let secondary = clock(0, Superframe::Secondary);
    assert!(matches!(
        BroadcastChange::try_new(&secondary, 0, previous, BroadcastScope::Fbcch),
        Err(BroadcastChangeError::Boundary)
    ));
    assert_eq!(
        BroadcastChange::try_new(&secondary, 1, previous, BroadcastScope::Ebcch)
            .unwrap()
            .activation_superframe(),
        3
    );
}

#[test]
fn final_spach_encoding_updates_bcn_on_filler_and_cached_begin_and_continue_frames() {
    let config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let messages = crate::runtime::serving::messages(&config);
    let clock = clock(11, Superframe::Primary);
    let previous = BroadcastSignaling {
        changes: BroadcastChanges {
            fbcch: false,
            ebcch: true,
        },
        notification: false,
    };
    let mut frames = vec![[false; INFORMATION_BITS]];
    frames.extend(
        SingleMessage {
            channel: Channel::Pch,
            flags: HeaderFlags::default(),
            identity: MobileIdentity::Min34(1),
            message: vec![0x55; 40],
        }
        .encode_fragmented()
        .unwrap(),
    );
    frames.extend(
        spach::arq::fragment_message(
            spach::arq::ArqHeader {
                channel: Channel::Arch,
                flags: HeaderFlags::default(),
                partial_echo: 7,
                polling: true,
                response_mode: spach::arq::ResponseMode::Contention,
            },
            MobileIdentity::Min34(1),
            &[0x55; 40],
        )
        .unwrap()
        .iter()
        .map(|frame| frame.encode().unwrap()),
    );
    let feedback = SharedFeedback {
        leading: [false; LEADING_FEEDBACK_BITS],
        trailing: [false; TRAILING_FEEDBACK_BITS],
    };
    for notification in [false, true] {
        let change = BroadcastChange::try_new(
            &clock,
            0,
            BroadcastSignaling {
                notification,
                ..previous
            },
            BroadcastScope::Both,
        )
        .unwrap();
        for sf in [0, 1, 2, 3, 24] {
            let state = change.at(sf);
            let snapshot = clock
                .snapshot_with_signaling(sf, AssignedSlots::Full14, &messages, &[], state.signaling)
                .unwrap();
            for slot in 0..super::super::PHYSICAL_SLOTS {
                let Some(channel) = snapshot.channel(slot).unwrap() else {
                    continue;
                };
                if channel == LogicalChannel::Spach {
                    for source in &frames {
                        let mut cached = *source;
                        cached[BCN_BIT] = !state.signaling.notification;
                        let encoded = snapshot
                            .encode_slot(slot, &feedback, Some(&cached))
                            .unwrap()
                            .unwrap();
                        let decoded =
                            control::decode_forward(&encoded.bits, channel, config.cell.dvcc)
                                .unwrap();
                        let mut expected = cached;
                        expected[BCN_BIT] = state.signaling.notification;
                        assert_eq!(decoded.block.information, expected);
                        assert_eq!(cached[BCN_BIT], !state.signaling.notification);
                    }
                } else if channel == LogicalChannel::Fbcch {
                    let encoded = snapshot
                        .encode_slot(slot, &feedback, None)
                        .unwrap()
                        .unwrap();
                    let decoded =
                        control::decode_forward(&encoded.bits, channel, config.cell.dvcc).unwrap();
                    assert_eq!(
                        decoded.block.information[FC_BIT],
                        state.signaling.changes.fbcch
                    );
                    assert_eq!(
                        decoded.block.information[EC_BIT],
                        state.signaling.changes.ebcch
                    );
                }
            }
        }
    }
}

#[test]
fn bcn_transition_preserves_fragmented_paging_and_arq_reassembly() {
    let config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let messages = crate::runtime::serving::messages(&config);
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) = &messages[0] else {
        panic!()
    };
    let first_spach =
        u64::from(structure.fbcch_slots + structure.ebcch_slots) * super::super::CONTROL_PERIOD;
    let clock = clock(11, Superframe::Primary);
    let change = BroadcastChange::try_new(
        &clock,
        2,
        BroadcastSignaling {
            changes: BroadcastChanges {
                fbcch: false,
                ebcch: false,
            },
            notification: false,
        },
        BroadcastScope::Fbcch,
    )
    .unwrap();
    let feedback = SharedFeedback {
        leading: [false; LEADING_FEEDBACK_BITS],
        trailing: [false; TRAILING_FEEDBACK_BITS],
    };
    let transmit = |index: usize, frame: &[bool; INFORMATION_BITS]| {
        let (sf, slot) = if index == 0 {
            (
                1,
                (super::super::CONTROL_SLOTS - 1) * super::super::CONTROL_PERIOD,
            )
        } else {
            (
                2,
                first_spach + (index as u64 - 1) * super::super::CONTROL_PERIOD,
            )
        };
        let snapshot = clock
            .snapshot_with_signaling(
                sf,
                AssignedSlots::Full14,
                &messages,
                &[],
                change.at(sf).signaling,
            )
            .unwrap();
        let encoded = snapshot
            .encode_slot(slot, &feedback, Some(frame))
            .unwrap()
            .unwrap();
        let decoded =
            control::decode_forward(&encoded.bits, LogicalChannel::Spach, config.cell.dvcc)
                .unwrap();
        assert_eq!(decoded.block.information[BCN_BIT], index != 0);
        decoded.block.information
    };
    let identity = MobileIdentity::Min34(1);
    let payload = vec![0x55; 80];
    let frames = SingleMessage {
        channel: Channel::Pch,
        flags: HeaderFlags::default(),
        identity,
        message: payload.clone(),
    }
    .encode_fragmented()
    .unwrap();
    let mut paging = spach::SpachReceiver::default();
    let mut delivered = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        if let Some(message) = paging.push(&transmit(index, frame)).unwrap() {
            delivered.push(message);
        }
    }
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].message, payload);
    let frames = spach::arq::fragment_message(
        spach::arq::ArqHeader {
            channel: Channel::Smsch,
            flags: HeaderFlags::default(),
            partial_echo: 7,
            polling: true,
            response_mode: spach::arq::ResponseMode::Reservation,
        },
        identity,
        &payload,
    )
    .unwrap();
    let mut arq = spach::arq::ArqReassembler::try_new(identity).unwrap();
    delivered.clear();
    for (index, frame) in frames.iter().enumerate() {
        if let spach::arq::Reception::Accepted {
            message: Some(message),
            ..
        } = arq
            .push(&transmit(index, &frame.encode().unwrap()))
            .unwrap()
        {
            delivered.push(message);
        }
    }
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].message, payload);
}
