use super::*;
use crate::{
    link::spach::paging::{self, PageBody, PagingFlags},
    phy::{
        control::{self, LogicalChannel, SharedFeedback},
        sync::SyncWord,
    },
    scheduler::broadcast::Position,
};
fn structure() -> DcchStructure {
    DcchStructure {
        fbcch_slots: 3,
        ebcch_slots: 1,
        sbcch_slots: 0,
        reserved_slots: 0,
        hyperframe_counter: 0,
        secondary_superframe: false,
        slot_configuration: SlotConfiguration::OneFullRate,
        dvcc: 1,
        max_supported_pfc: 1,
        pch_displacement: 0,
        pfm_direction: false,
        non_pch_subchannel_code: 0,
    }
}
#[test]
fn permanent_identity_hash_divides_by_dcch_slots_and_excludes_nonpaging_phases() {
    let allocation = PagingAllocation::try_new(&structure()).unwrap();
    for (identity, phase) in [(1, 4), (2, 5), (55, 31), (56, 4), (65537, 4)] {
        assert_eq!(
            allocation.phase(MobileIdentity::Min34(identity)).unwrap(),
            phase
        );
        assert_eq!(
            allocation.phase(MobileIdentity::Imsi50(identity)).unwrap(),
            phase
        );
    }
    assert!(matches!(
        allocation.phase(MobileIdentity::Tmsi20(1)),
        Err(Error::Identity)
    ));
    assert!(matches!(
        allocation.phase(MobileIdentity::Min34(1 << MIN_BITS)),
        Err(Error::Identity)
    ));
    let mut adjusted = structure();
    adjusted.non_pch_subchannel_code = 3;
    let allocation = PagingAllocation::try_new(&adjusted).unwrap();
    for identity in 1..=65535 {
        let phase = allocation.phase(MobileIdentity::Min34(identity)).unwrap();
        assert!((4..26).contains(&phase));
    }
    adjusted.pch_displacement = 1;
    assert!(matches!(
        PagingAllocation::try_new(&adjusted),
        Err(Error::Arrangement)
    ));
}
#[test]
fn primary_page_repeats_exact_information_and_decodes_at_same_secondary_phase() {
    let clock = BroadcastClock::try_new(Position {
        hyperframe_counter: 11,
        superframe: Superframe::Primary,
    })
    .unwrap();
    let allocation = PagingAllocation::try_new(&structure()).unwrap();
    let identity = MobileIdentity::Min34(2);
    let phase = allocation.phase(identity).unwrap();
    let page = PageFrame {
        flags: PagingFlags::default(),
        body: PageBody::TripleMin34([Some(2), None, None]),
    };
    let repeat = PageRepeat::try_new(&clock, 0, allocation, phase, &page).unwrap();
    let feedback = SharedFeedback {
        leading: [false; 12],
        trailing: [false; 10],
    };
    for index in [0, 1] {
        let information = repeat.information(index, phase).unwrap();
        assert_eq!(information, &page.encode().unwrap());
        let bits = control::encode_forward(
            SyncWord::One,
            phase,
            &feedback,
            information,
            LogicalChannel::Spach,
            1,
        )
        .unwrap();
        let decoded = control::decode_forward(&bits, LogicalChannel::Spach, 1).unwrap();
        let decoded_page = paging::decode(&decoded.block.information).unwrap();
        assert_eq!(decoded_page, page);
        assert!(decoded_page.addresses(identity));
    }
    let mixed = PageFrame {
        flags: PagingFlags::default(),
        body: PageBody::TripleMin34([Some(2), Some(4), None]),
    };
    assert!(matches!(
        PageRepeat::try_new(&clock, 0, allocation, phase, &mixed),
        Err(Error::Phase)
    ));
    assert!(repeat.information(2, phase).is_none());
    assert!(repeat.information(1, phase + 1).is_none());
    assert!(matches!(
        PageRepeat::try_new(&clock, 1, allocation, phase, &page),
        Err(Error::Primary)
    ));
    assert!(matches!(
        PageRepeat::try_new(&clock, 0, allocation, 3, &page),
        Err(Error::Phase)
    ));
}

#[test]
fn full_rate_pch_continuation_uses_restricted_slots_skips_bcch_and_secondary_superframes() {
    let mut structure = structure();
    structure.non_pch_subchannel_code = 3;
    let allocation = PagingAllocation::try_new(&structure).unwrap();
    assert_eq!(allocation.continuation(2, 24), Some((2, 26)));
    assert_eq!(allocation.continuation(2, 30), Some((4, 4)));
    assert_eq!(allocation.continuation(2, 31), Some((4, 5)));
    assert_eq!(allocation.continuation_span(2, 24, 3), Some(5));
    assert_eq!(allocation.continuation_span(2, 31, 2), Some(31));
    assert_eq!(allocation.continuation(3, 24), None);
    assert_eq!(allocation.continuation(2, 3), None);
    assert_eq!(allocation.continuation(u64::MAX - 1, 31), None);
}
