use super::*;
use crate::runtime::voice::paging::{Transmission, message};
fn handle() -> CallHandle {
    let (mut voice, registry) = setup();
    voice.receive_access(&registry, &origin(), 0, 0).unwrap();
    voice.page().unwrap().request.handle.clone()
}
fn allocation(restricted: u8) -> PagingAllocation {
    let config = Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(mut structure) =
        crate::runtime::serving::messages(&config)
            .into_iter()
            .next()
            .unwrap()
    else {
        panic!()
    };
    structure.non_pch_subchannel_code = restricted;
    PagingAllocation::try_new(&structure).unwrap()
}
#[test]
fn fragmented_pages_cross_superframes_and_restricted_slots_with_exact_secondary_copies() {
    let handle = handle();
    for restricted in [0, 3] {
        let allocation = allocation(restricted);
        let first_phase = (1..64)
            .map(|identity| allocation.phase(MobileIdentity::Min34(identity)).unwrap())
            .max()
            .unwrap();
        for identity in [MobileIdentity::Min34(2), MobileIdentity::Imsi50(2)] {
            let bytes = message(&"1".repeat(500)).unwrap();
            assert_eq!(bytes.len(), 255);
            let expected = SingleMessage {
                channel: Channel::Pch,
                flags: HeaderFlags::default(),
                identity,
                message: bytes.clone(),
            }
            .encode_fragmented()
            .unwrap();
            let mut tx = Transmission::new(identity, bytes.clone()).unwrap();
            let span = allocation
                .continuation_span(0, first_phase, expected.len())
                .unwrap();
            assert!(span > expected.len());
            let mut opportunity = (0, first_phase);
            let mut receipts = Vec::new();
            let mut primary = crate::link::spach::SpachReceiver::default();
            let mut secondary = crate::link::spach::SpachReceiver::default();
            for (index, information) in expected.iter().enumerate() {
                assert_eq!(
                    tx.select(opportunity.0, opportunity.1, first_phase),
                    Some(information)
                );
                assert_eq!(
                    tx.frame_count(allocation, opportunity.0, opportunity.1),
                    allocation
                        .continuation_span(opportunity.0, opportunity.1, expected.len() - index)
                        .unwrap()
                );
                let receipt = tx.receipt(&handle);
                assert!(!tx.transmitted(&receipt, &handle));
                let repeat = tx
                    .submitted(allocation, opportunity.0, opportunity.1)
                    .unwrap();
                assert_eq!(
                    (repeat.superframe, repeat.phase),
                    (opportunity.0 + 1, opportunity.1)
                );
                assert_eq!(repeat.information, *information);
                let decoded = primary.push(information).unwrap();
                let repeated = secondary.push(&repeat.information).unwrap();
                if index + 1 == expected.len() {
                    assert_eq!(decoded.unwrap().message, bytes);
                    assert_eq!(repeated.unwrap().message, bytes);
                } else {
                    assert!(decoded.is_none());
                    assert!(repeated.is_none());
                    assert!(tx.in_progress());
                    assert!(
                        tx.select(opportunity.0, opportunity.1 + 1, first_phase)
                            .is_none()
                    );
                    opportunity = allocation
                        .continuation(opportunity.0, opportunity.1)
                        .unwrap();
                }
                receipts.push(receipt);
            }
            assert!(!tx.in_progress());
            assert!(opportunity.0 >= 2);
            for receipt in receipts.iter().rev().take(receipts.len() - 1) {
                assert!(!tx.transmitted(receipt, &handle));
            }
            assert!(tx.transmitted(&receipts[0], &handle));
            assert!(!tx.transmitted(&receipts[0], &handle));
        }
    }
}
#[test]
fn missed_continuations_restart_from_begin_and_stale_receipts_cannot_complete_a_retry() {
    let handle = handle();
    let foreign = self::handle();
    let allocation = allocation(0);
    let phase = allocation.phase(MobileIdentity::Min34(2)).unwrap();
    let mut tx = Transmission::new(MobileIdentity::Min34(2), message("1001").unwrap()).unwrap();
    let first = *tx.select(0, phase, phase).unwrap();
    let stale = tx.receipt(&handle);
    tx.submitted(allocation, 0, phase).unwrap();
    let due = allocation.continuation(0, phase).unwrap();
    assert!(tx.select(due.0, due.1 + 1, phase).is_none());
    assert!(!tx.in_progress());
    assert!(!tx.transmitted(&stale, &handle));
    assert_eq!(tx.select(2, phase, phase), Some(&first));
    let current = tx.receipt(&handle);
    tx.submitted(allocation, 2, phase).unwrap();
    assert!(!tx.transmitted(&current, &foreign));
    assert!(!tx.transmitted(&current, &handle));
    let due = allocation.continuation(2, phase).unwrap();
    tx.select(due.0, due.1, phase).unwrap();
    let last = tx.receipt(&handle);
    tx.submitted(allocation, due.0, due.1).unwrap();
    assert!(!tx.transmitted(&stale, &handle));
    assert!(tx.transmitted(&last, &handle));
    assert_eq!(tx.select(4, phase, phase), Some(&first));
    assert!(!tx.transmitted(&last, &handle));
}
#[test]
fn configured_page_numbers_have_control_bounds_and_unavailable_fallback_without_truncation() {
    for number in ["1001".to_owned(), "1".repeat(32), "1".repeat(500)] {
        let page = crate::l3::page::Page::decode(&message(&number).unwrap()).unwrap();
        assert_eq!(
            page.calling_number.unwrap().address,
            AddressValue::Tbcd(number)
        );
        assert_eq!(
            page.presentation.unwrap(),
            crate::l3::access_call::Presentation {
                presentation: 0,
                screening: 3
            }
        );
    }
    let page = crate::l3::page::Page::decode(&message(&"1".repeat(501)).unwrap()).unwrap();
    assert!(page.calling_number.is_none());
    assert_eq!(
        page.presentation.unwrap(),
        crate::l3::access_call::Presentation {
            presentation: 2,
            screening: 3
        }
    );
}
#[test]
fn final_page_receipt_at_deadline_expires_the_attempt_and_releases_both_reservations() {
    let (mut voice, registry) = setup();
    voice.receive_access(&registry, &origin(), 0, 0).unwrap();
    let deadline = voice.page().unwrap().request.deadline_monotonic_ms;
    let mut receipts = submit_page(&mut voice, 0);
    let final_receipt = receipts.pop().unwrap();
    assert!(!receipts.is_empty());
    for receipt in receipts {
        voice.transmitted(receipt, deadline - 1).unwrap();
    }
    assert!(
        voice
            .receive_access(&registry, &response(), deadline - 1, 3)
            .is_err()
    );
    voice.transmitted(final_receipt, deadline).unwrap();
    assert!(voice.page().is_none());
    assert!(voice.active().is_none());
    assert_eq!(voice.paging.reservations().count(), 0);
    assert_eq!(voice.rejections.len(), 2);
    assert_eq!(voice.recent_calls()[0].end_reason, "Paging expired");
    assert!(!voice.repeats.is_empty());
}

#[test]
fn response_at_the_exact_page_deadline_expires_and_notifies_both_phones() {
    let (mut voice, registry) = setup();
    voice.receive_access(&registry, &origin(), 0, 0).unwrap();
    let deadline = voice.page().unwrap().request.deadline_monotonic_ms;
    complete_page(&mut voice, 0, deadline - 1);
    voice
        .receive_access(&registry, &response(), deadline, 3)
        .unwrap();
    assert!(voice.page().is_none());
    assert!(voice.active().is_none());
    assert_eq!(voice.paging.reservations().count(), 0);
    assert_eq!(voice.recent_calls()[0].end_reason, "Paging expired");
    assert_eq!(voice.rejections.len(), 2);
    assert!(!voice.repeats.is_empty());
}
