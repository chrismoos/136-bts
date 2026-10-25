use super::*;
use crate::{
    phy::control::{LEADING_FEEDBACK_BITS, SharedFeedback, TRAILING_FEEDBACK_BITS},
    scheduler::broadcast::{
        BroadcastChanges, BroadcastClock, PHYSICAL_SLOTS, Position, Superframe,
    },
};

fn content(optional: Vec<Vec<u8>>) -> BroadcastContent {
    BroadcastContent::try_new(
        AssignedSlots::Full14,
        super::super::super::tests::messages(),
        optional,
    )
    .unwrap()
}
fn raw(messages: &[MandatoryBroadcast; MANDATORY_PACKETS], optional: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![
        STORAGE_VERSION,
        AssignedSlots::Full14 as u8,
        optional.len().try_into().unwrap(),
    ];
    for message in messages {
        append(&mut bytes, &message.encode().unwrap()).unwrap();
    }
    for packet in optional {
        append(&mut bytes, packet).unwrap();
    }
    bytes
}

#[test]
fn recovered_contents_preserve_every_packet_layout_and_outgoing_slot() {
    let clock = BroadcastClock::try_new(Position {
        hyperframe_counter: 11,
        superframe: Superframe::Primary,
    })
    .unwrap();
    let feedback = SharedFeedback {
        leading: [false; LEADING_FEEDBACK_BITS],
        trailing: [false; TRAILING_FEEDBACK_BITS],
    };
    let changes = BroadcastChanges {
        fbcch: true,
        ebcch: false,
    };
    for optional in [
        vec![],
        super::super::super::tests::optional(),
        vec![vec![63, 0xaa, 0x55]],
    ] {
        let original = content(optional.clone());
        let bytes = original.storage_bytes().unwrap();
        let restored = BroadcastContent::from_storage_bytes(&bytes).unwrap();
        assert_eq!(restored.storage_bytes().unwrap(), bytes);
        assert_eq!(restored.messages(), original.messages());
        assert_eq!(restored.optional_messages(), optional);
        assert_eq!(restored.pair(), original.pair());
        for sf in [0, 1, 2, 23, 24] {
            let before = clock
                .snapshot_with_optional(
                    sf,
                    original.pair(),
                    original.messages(),
                    original.optional_messages(),
                    changes,
                )
                .unwrap();
            let after = clock
                .snapshot_with_optional(
                    sf,
                    restored.pair(),
                    restored.messages(),
                    restored.optional_messages(),
                    changes,
                )
                .unwrap();
            for slot in 0..PHYSICAL_SLOTS {
                let before = before
                    .encode_slot(slot, &feedback, None)
                    .unwrap()
                    .map(|slot| slot.bits);
                let after = after
                    .encode_slot(slot, &feedback, None)
                    .unwrap()
                    .map(|slot| slot.bits);
                assert_eq!(before, after);
            }
        }
    }
}

#[test]
fn truncated_trailing_wrong_version_and_invalid_packet_boundaries_reject() {
    let bytes = content(super::super::super::tests::optional())
        .storage_bytes()
        .unwrap();
    for end in 0..bytes.len() {
        assert!(
            BroadcastContent::from_storage_bytes(&bytes[..end]).is_err(),
            "prefix={end}"
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(matches!(
        BroadcastContent::from_storage_bytes(&trailing),
        Err(BroadcastContentStorageError::Format)
    ));
    let mut invalid = bytes.clone();
    invalid[0] = STORAGE_WITH_NEIGHBORS + 1;
    assert!(matches!(
        BroadcastContent::from_storage_bytes(&invalid),
        Err(BroadcastContentStorageError::Version(_))
    ));
    invalid = bytes.clone();
    invalid[1] = AssignedSlots::Full25 as u8;
    assert!(matches!(
        BroadcastContent::from_storage_bytes(&invalid),
        Err(BroadcastContentStorageError::Format)
    ));
    invalid = bytes.clone();
    invalid[2] += 1;
    assert!(matches!(
        BroadcastContent::from_storage_bytes(&invalid),
        Err(BroadcastContentStorageError::Truncated)
    ));
    invalid = bytes;
    invalid[3] = 0;
    assert!(matches!(
        BroadcastContent::from_storage_bytes(&invalid),
        Err(BroadcastContentStorageError::Format)
    ));
}

#[test]
fn recovery_rejects_clock_or_layout_normalization_and_wrong_mandatory_order() {
    let mut messages = super::super::super::tests::messages();
    let MandatoryBroadcast::DcchStructure(structure) = &mut messages[0] else {
        panic!()
    };
    structure.hyperframe_counter = 11;
    assert!(matches!(
        BroadcastContent::from_storage_bytes(&raw(&messages, &[])),
        Err(BroadcastContentStorageError::NonCanonical)
    ));
    messages = super::super::super::tests::messages();
    assert!(matches!(
        BroadcastContent::from_storage_bytes(&raw(
            &messages,
            &super::super::super::tests::optional()
        )),
        Err(BroadcastContentStorageError::NonCanonical)
    ));
    messages.swap(0, 1);
    assert!(matches!(
        BroadcastContent::from_storage_bytes(&raw(&messages, &[])),
        Err(BroadcastContentStorageError::Content(Error::Messages))
    ));
}

#[test]
fn time_policy_storage_preserves_zone_and_accepts_legacy_images() {
    use crate::l3::time_date::TimeZoneOffset;
    let legacy = content(vec![]);
    let legacy_bytes = legacy.storage_bytes().unwrap();
    assert_eq!(legacy_bytes[0], STORAGE_VERSION);
    assert!(
        BroadcastContent::from_storage_bytes(&legacy_bytes)
            .unwrap()
            .time_zone()
            .is_none()
    );
    let zone = TimeZoneOffset::try_new(-420, false).unwrap();
    let current = legacy.with_time_zone(zone).unwrap();
    let bytes = current.storage_bytes().unwrap();
    assert_eq!(bytes[0], STORAGE_WITH_TIME);
    let recovered = BroadcastContent::from_storage_bytes(&bytes).unwrap();
    assert_eq!(recovered.time_zone(), Some(zone));
    assert_eq!(recovered.storage_bytes().unwrap(), bytes);
    let mut bad = bytes.clone();
    bad[3] = 0xff;
    bad[4] = 0xff;
    assert!(BroadcastContent::from_storage_bytes(&bad).is_err());
    for len in 0..bytes.len() {
        assert!(BroadcastContent::from_storage_bytes(&bytes[..len]).is_err());
    }
}
