use super::*;
use crate::{
    link::{broadcast::FbcchReceiver, rach::MobileIdentity},
    phy::control::{
        self, LEADING_FEEDBACK_BITS, LogicalChannel, SharedFeedback, TRAILING_FEEDBACK_BITS,
    },
    scheduler::{
        broadcast::{PHYSICAL_SLOTS, Position, Superframe},
        paging::PagingAllocation,
    },
};
const FC_BIT: usize = 1;
const EC_BIT: usize = 2;
const BCN_BIT: usize = 4;
const CONTROL_PERIOD: u64 = 3;

fn clock() -> BroadcastClock {
    BroadcastClock::try_new(Position {
        hyperframe_counter: 11,
        superframe: Superframe::Primary,
    })
    .unwrap()
}
fn signaling() -> BroadcastSignaling {
    BroadcastSignaling {
        changes: BroadcastChanges {
            fbcch: false,
            ebcch: true,
        },
        notification: false,
    }
}
fn content(optional: Vec<Vec<u8>>) -> BroadcastContent {
    BroadcastContent::try_new(
        AssignedSlots::Full14,
        super::super::tests::messages(),
        optional,
    )
    .unwrap()
}

#[test]
fn transition_owns_fitted_content_and_changes_packets_flags_and_paging_together() {
    let old_packets = crate::runtime::serving::optional_messages().unwrap();
    let updated_packets = super::super::tests::optional();
    let transition = BroadcastTransition::try_new(
        clock(),
        2,
        signaling(),
        content(old_packets.clone()),
        content(updated_packets.clone()),
    )
    .unwrap();
    assert_eq!(transition.activation_superframe(), 4);
    let mut old_count = None;
    let feedback = SharedFeedback {
        leading: [false; LEADING_FEEDBACK_BITS],
        trailing: [false; TRAILING_FEEDBACK_BITS],
    };
    for sf in [0, 1, 2, 3, 4, 5, 24] {
        let snapshot = transition.snapshot(sf).unwrap();
        let active = transition.content(sf);
        let MandatoryBroadcast::DcchStructure(structure) = &active.messages()[0] else {
            panic!()
        };
        if sf == 0 {
            old_count = Some(structure.fbcch_slots);
        }
        if sf >= 4 {
            assert!(structure.fbcch_slots > old_count.unwrap());
        }
        let allocation = PagingAllocation::try_new(structure).unwrap();
        let phase = allocation.phase(MobileIdentity::Min34(1)).unwrap();
        assert_eq!(
            snapshot.channel(u64::from(phase) * CONTROL_PERIOD).unwrap(),
            Some(LogicalChannel::Spach)
        );
        let mut receiver = FbcchReceiver::default();
        let mut packets = Vec::new();
        for slot in 0..PHYSICAL_SLOTS {
            let Some(channel) = snapshot.channel(slot).unwrap() else {
                continue;
            };
            let encoded = snapshot
                .encode_slot(slot, &feedback, None)
                .unwrap()
                .unwrap();
            let decoded = control::decode_forward(&encoded.bits, channel, structure.dvcc).unwrap();
            if channel == LogicalChannel::Fbcch {
                assert_eq!(decoded.block.information[FC_BIT], sf >= 4);
                assert!(decoded.block.information[EC_BIT]);
                packets.extend(
                    receiver
                        .push(decoded.block.information.as_slice().try_into().unwrap())
                        .unwrap(),
                );
            } else if channel == LogicalChannel::Spach {
                assert_eq!(decoded.block.information[BCN_BIT], sf >= 2);
            }
        }
        let mut expected = active.messages().clone();
        let MandatoryBroadcast::DcchStructure(structure) = &mut expected[0] else {
            panic!()
        };
        let position = clock().at(sf);
        structure.hyperframe_counter = position.hyperframe_counter;
        structure.secondary_superframe = position.superframe == Superframe::Secondary;
        let mut expected: Vec<_> = expected
            .iter()
            .map(|message| message.encode().unwrap())
            .collect();
        expected.extend_from_slice(if sf < 4 {
            &old_packets
        } else {
            &updated_packets
        });
        assert_eq!(packets, expected);
    }
}

#[test]
fn clock_metadata_alone_is_not_a_content_change_but_system_identity_is() {
    let original = super::super::tests::messages();
    let mut updated = original.clone();
    let MandatoryBroadcast::DcchStructure(structure) = &mut updated[0] else {
        panic!()
    };
    structure.hyperframe_counter = 11;
    structure.secondary_superframe = true;
    let make =
        |messages| BroadcastContent::try_new(AssignedSlots::Full14, messages, vec![]).unwrap();
    assert!(matches!(
        BroadcastTransition::try_new(
            clock(),
            0,
            signaling(),
            make(original.clone()),
            make(updated.clone())
        ),
        Err(BroadcastTransitionError::Unchanged)
    ));
    let MandatoryBroadcast::SystemIdentity(identity) = &mut updated[4] else {
        panic!()
    };
    identity.sid += 1;
    let transition =
        BroadcastTransition::try_new(clock(), 0, signaling(), make(original), make(updated))
            .unwrap();
    assert!(!transition.state(1).signaling.changes.fbcch);
    assert!(transition.state(2).signaling.changes.fbcch);
    assert!(transition.state(2).signaling.changes.ebcch);
    let MandatoryBroadcast::SystemIdentity(before) = &transition.content(1).messages()[4] else {
        panic!()
    };
    let MandatoryBroadcast::SystemIdentity(after) = &transition.content(2).messages()[4] else {
        panic!()
    };
    assert_eq!(before.sid + 1, after.sid);
}

#[test]
fn invalid_content_or_activation_cannot_create_a_transition() {
    let mut invalid = super::super::tests::messages();
    let MandatoryBroadcast::DcchStructure(structure) = &mut invalid[0] else {
        panic!()
    };
    structure.ebcch_slots = 9;
    assert!(matches!(
        BroadcastContent::try_new(AssignedSlots::Full14, invalid, vec![]),
        Err(Error::Arrangement)
    ));
    assert!(matches!(
        BroadcastContent::try_new(
            AssignedSlots::Full14,
            super::super::tests::messages(),
            vec![vec![]]
        ),
        Err(Error::Framing(super::super::BroadcastError::MessageLength))
    ));
    assert!(matches!(
        BroadcastTransition::try_new(
            clock(),
            1,
            signaling(),
            content(vec![]),
            content(super::super::tests::optional())
        ),
        Err(BroadcastTransitionError::Change(
            BroadcastChangeError::Boundary
        ))
    ));
}
