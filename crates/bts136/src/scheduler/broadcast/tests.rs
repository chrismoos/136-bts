use super::*;
use crate::{
    config::Config,
    core::maca::MacaPlan,
    l3::{
        broadcast::ProtocolVersion,
        legacy_traffic::{ControlChannel, Hyperband},
        maca::{
            HyperbandChannels,
            broadcast::{MacaStatus, ReportTriggers},
        },
    },
    link::{broadcast::FbcchReceiver, rach::MobileIdentity},
    scheduler::paging::PagingAllocation,
};
const CHANGES: BroadcastChanges = BroadcastChanges {
    fbcch: true,
    ebcch: false,
};
pub(super) fn messages() -> [MandatoryBroadcast; 5] {
    let config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    crate::runtime::serving::messages(&config)
}
pub(super) fn optional() -> Vec<Vec<u8>> {
    let plan = MacaPlan::try_new(
        ControlChannel {
            hyperband: Hyperband::Cellular800,
            channel: 991,
            dvcc: 1,
        },
        ProtocolVersion::Pv2,
        MacaStatus::Both,
        ReportTriggers {
            registration: true,
            origination: true,
            page_response: true,
            audit: true,
        },
        vec![992, 993, 994, 995],
        vec![
            HyperbandChannels {
                hyperband: Hyperband::Pcs1900,
                channels: vec![100, 101, 102, 103, 104],
            },
            HyperbandChannels {
                hyperband: Hyperband::Cellular700,
                channels: vec![200, 201, 202],
            },
            HyperbandChannels {
                hyperband: Hyperband::Imt2000,
                channels: vec![300, 301, 302],
            },
        ],
    )
    .unwrap();
    let mut packets = crate::runtime::serving::optional_messages().unwrap();
    packets.extend_from_slice(plan.messages());
    packets
}
#[test]
fn full_maca_lists_expand_the_announced_layout_without_losing_messages_or_paging() {
    let initial = messages();
    let packets = optional();
    assert!(matches!(
        BroadcastSuperframe::try_new_with_optional(
            AssignedSlots::Full14,
            &initial,
            &packets,
            CHANGES
        ),
        Err(Error::Framing(BroadcastError::Capacity))
    ));
    let fitted =
        BroadcastSuperframe::fit_optional(AssignedSlots::Full14, &initial, &packets, CHANGES)
            .unwrap();
    let MandatoryBroadcast::DcchStructure(structure) = &fitted[0] else {
        panic!()
    };
    assert!(structure.fbcch_slots > 4);
    let clock = BroadcastClock::try_new(Position {
        hyperframe_counter: 0,
        superframe: Superframe::Primary,
    })
    .unwrap();
    for index in [0, 1, 23, 24] {
        let snapshot = clock
            .snapshot_with_optional(index, AssignedSlots::Full14, &fitted, &packets, CHANGES)
            .unwrap();
        let mut receiver = FbcchReceiver::default();
        let decoded: Vec<_> = snapshot
            .fbcch
            .iter()
            .flat_map(|frame| receiver.push(frame).unwrap())
            .collect();
        assert_eq!(decoded.len(), fitted.len() + packets.len());
        let declared = crate::l3::broadcast::decode_prefix(&decoded[0])
            .unwrap()
            .message;
        let MandatoryBroadcast::DcchStructure(declared) = declared else {
            panic!()
        };
        assert_eq!(declared.fbcch_slots, structure.fbcch_slots);
        assert_eq!(declared.secondary_superframe, index % 2 != 0);
        assert_eq!(&decoded[fitted.len()..], &packets);
        let phase = PagingAllocation::try_new(&declared)
            .unwrap()
            .phase(MobileIdentity::Min34(1))
            .unwrap();
        assert_eq!(phase, declared.fbcch_slots + declared.ebcch_slots);
        assert_eq!(
            snapshot.channel(u64::from(phase) * CONTROL_PERIOD).unwrap(),
            Some(LogicalChannel::Spach)
        );
        assert_eq!(
            snapshot
                .channel(u64::from(declared.fbcch_slots) * CONTROL_PERIOD)
                .unwrap(),
            Some(LogicalChannel::Ebcch)
        );
    }
    assert_eq!(initial, messages());
}
#[test]
fn fit_uses_the_smallest_sufficient_count_and_keeps_the_default_layout() {
    let initial = messages();
    let menu = crate::runtime::serving::optional_messages().unwrap();
    assert_eq!(
        BroadcastSuperframe::fit_optional(AssignedSlots::Full14, &initial, &menu, CHANGES).unwrap(),
        initial
    );
    let fitted =
        BroadcastSuperframe::fit_optional(AssignedSlots::Full14, &initial, &optional(), CHANGES)
            .unwrap();
    let mut smaller = fitted.clone();
    let MandatoryBroadcast::DcchStructure(structure) = &mut smaller[0] else {
        panic!()
    };
    structure.fbcch_slots -= 1;
    assert!(matches!(
        BroadcastSuperframe::try_new_with_optional(
            AssignedSlots::Full14,
            &smaller,
            &optional(),
            CHANGES
        ),
        Err(Error::Framing(BroadcastError::Capacity))
    ));
}
#[test]
fn fit_rejects_invalid_allocations_bad_messages_and_overflow_without_changing_input() {
    let initial = messages();
    let huge = vec![vec![1; u8::MAX as usize]; 2];
    assert!(matches!(
        BroadcastSuperframe::fit_optional(AssignedSlots::Full14, &initial, &huge, CHANGES),
        Err(Error::Framing(BroadcastError::Capacity))
    ));
    assert!(matches!(
        BroadcastSuperframe::fit_optional(AssignedSlots::Full14, &initial, &[vec![]], CHANGES),
        Err(Error::Framing(BroadcastError::MessageLength))
    ));
    for count in [0, 2, 11] {
        let mut invalid = initial.clone();
        let MandatoryBroadcast::DcchStructure(structure) = &mut invalid[0] else {
            panic!()
        };
        structure.fbcch_slots = count;
        assert!(
            BroadcastSuperframe::fit_optional(AssignedSlots::Full14, &invalid, &[], CHANGES)
                .is_err()
        );
    }
    assert_eq!(initial, messages());
}

#[test]
fn time_broadcast_has_its_own_cycle_slot_and_counter_changes_leave_flags_unchanged() {
    use crate::l3::time_date::{TimeAndDate, TimeZoneOffset};
    let messages = messages();
    let MandatoryBroadcast::DcchStructure(structure) = &messages[0] else {
        panic!()
    };
    let mut frame =
        BroadcastSuperframe::try_new(AssignedSlots::Full14, &messages, CHANGES).unwrap();
    let time = TimeAndDate {
        seconds_since_1980: 1_400_000_000,
        time_zone: TimeZoneOffset::try_new(-420, false).unwrap(),
    };
    frame.set_time_date(time).unwrap();
    let feedback = SharedFeedback {
        leading: [false; control::LEADING_FEEDBACK_BITS],
        trailing: [false; control::TRAILING_FEEDBACK_BITS],
    };
    let mut packets = Vec::new();
    for index in 0..2 {
        let slot = frame
            .encode_slot(
                (u64::from(structure.fbcch_slots) + index) * CONTROL_PERIOD,
                &feedback,
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(slot.channel, LogicalChannel::Ebcch);
        let decoded =
            control::decode_forward(&slot.bits, LogicalChannel::Ebcch, structure.dvcc).unwrap();
        let (cycle, mut messages) = crate::link::broadcast::decode_ebcch_begin(
            decoded.block.information.as_slice().try_into().unwrap(),
        )
        .unwrap();
        assert_eq!(cycle, 2);
        packets.append(&mut messages);
    }
    assert_eq!(packets.len(), 3);
    assert_eq!(TimeAndDate::decode(&packets[2]).unwrap(), time);
    let old = frame.fbcch.clone();
    frame
        .set_time_date(TimeAndDate {
            seconds_since_1980: time.seconds_since_1980 + 1,
            ..time
        })
        .unwrap();
    assert_eq!(frame.fbcch, old);
}

#[test]
fn full_neighbor_cycle_crosses_superframes_and_survives_storage_with_time() {
    let neighbors: Vec<_> = (1..=24)
        .map(crate::l3::neighbors::tests::neighbor)
        .collect();
    let mut config = crate::config::Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    config.cell.neighbors = neighbors.clone();
    let content = BroadcastContent::try_new(
        AssignedSlots::Full14,
        crate::runtime::serving::messages(&config),
        vec![],
    )
    .unwrap()
    .with_time_zone(crate::l3::time_date::TimeZoneOffset::try_new(-420, false).unwrap())
    .unwrap()
    .with_neighbors(&neighbors)
    .unwrap();
    let stored = content.storage_bytes().unwrap();
    let recovered = BroadcastContent::from_storage_bytes(&stored).unwrap();
    assert_eq!(recovered.storage_bytes().unwrap(), stored);
    assert_eq!(recovered.cycle_superframes().unwrap(), 2);
    let packet = recovered.neighbor_packet().unwrap();
    let frames = super::neighbor_cycle(packet).unwrap();
    assert_eq!(frames.len(), 16);
    let mut receiver = crate::link::broadcast::EbcchReceiver::default();
    let mut decoded = Vec::new();
    for frame in &frames {
        decoded.extend(receiver.push(frame).unwrap());
    }
    assert!(
        decoded
            .iter()
            .any(|packet| crate::l3::neighbors::decode(packet).ok().as_ref() == Some(&neighbors))
    );
    receiver.discontinuity();
    let mut after_gap = Vec::new();
    for frame in frames.iter().skip(1) {
        after_gap.extend(receiver.push(frame).unwrap());
    }
    assert!(
        !after_gap
            .iter()
            .any(|packet| crate::l3::neighbors::decode(packet).is_ok())
    );
    for frame in &frames {
        after_gap.extend(receiver.push(frame).unwrap());
    }
    assert!(
        after_gap
            .iter()
            .any(|packet| crate::l3::neighbors::decode(packet).is_ok())
    );
}
