use super::*;
use crate::l3::legacy_traffic::CallingNumber;
fn information() -> CallInformation {
    CallInformation {
        calling_numbers: vec![CallingNumber::network("1001").unwrap()],
        ..Default::default()
    }
}
fn ack(number: u8, last: Option<u8>) -> FlashAcknowledgment {
    FlashAcknowledgment {
        request_number: number,
        last_decoded_parameter: last,
        unparsed_bits: vec![],
    }
}
#[test]
fn flash_timer_starts_on_final_tx_and_retries_keep_number_but_replace_completion_token() {
    let mut endpoint = Endpoint::default();
    endpoint.request(information(), 0).unwrap();
    let first = endpoint.transmission().unwrap().clone();
    assert_eq!(
        endpoint.acknowledge(&ack(0, Some(4)), 1),
        Err(FlashError::NotTransmitted)
    );
    endpoint.advance(1000).unwrap();
    assert_eq!(endpoint.state(), Some(FlashState::Prepared));
    endpoint.transmitted(&first, 1001).unwrap();
    endpoint.advance(1200).unwrap();
    assert!(endpoint.transmission().is_none());
    endpoint.advance(1201).unwrap();
    let retry = endpoint.transmission().unwrap().clone();
    assert!(!retry.matches(&first));
    assert_eq!(retry.message(), first.message());
    assert_eq!(
        endpoint.transmitted(&first, 1202),
        Err(FlashError::StaleTransmission)
    );
    endpoint.transmitted(&retry, 1203).unwrap();
    assert_eq!(
        endpoint.acknowledge(&ack(1, Some(4)), 1204),
        Err(FlashError::RequestNumber)
    );
    endpoint.acknowledge(&ack(0, Some(4)), 1205).unwrap();
    assert_eq!(endpoint.state(), Some(FlashState::Acknowledged));
}
#[test]
fn partial_acknowledgments_cancellation_bounded_retries_and_wrapped_numbers_are_retained() {
    let mut endpoint = Endpoint::default();
    let mut fields = information();
    fields.calling_name = Some(crate::l3::legacy_traffic::CallingName::network("Alice").unwrap());
    for number in 0..18 {
        let now = number as u64 * 10;
        endpoint.request(fields.clone(), now).unwrap();
        assert_eq!(endpoint.number(), Some(number % 16));
        let tx = endpoint.transmission().unwrap().clone();
        endpoint.transmitted(&tx, now + 1).unwrap();
        endpoint
            .acknowledge(&ack(number % 16, Some(4)), now + 2)
            .unwrap();
        assert_eq!(
            endpoint.state(),
            Some(FlashState::Partial {
                last_parameter: Some(4)
            })
        );
        endpoint
            .acknowledge(&ack(number % 16, Some(6)), now + 3)
            .unwrap();
        assert_eq!(
            endpoint.state(),
            Some(FlashState::Partial {
                last_parameter: Some(4)
            })
        );
    }
    endpoint.request(information(), 200).unwrap();
    for attempt in 0..3 {
        let tx = endpoint.transmission().unwrap().clone();
        let sent = 201 + attempt * 201;
        endpoint.transmitted(&tx, sent).unwrap();
        endpoint.advance(sent + 200).unwrap();
    }
    assert_eq!(endpoint.state(), Some(FlashState::TimedOut));
    endpoint.request(information(), 1000).unwrap();
    endpoint.cancel();
    assert_eq!(endpoint.state(), Some(FlashState::Cancelled));
    assert_eq!(
        endpoint.acknowledge(&ack(3, None), 1001),
        Err(FlashError::Closed)
    );
}
#[test]
fn duplicate_requests_do_not_replace_pending_work_and_reverse_requests_are_independent() {
    let mut endpoint = Endpoint::default();
    endpoint.request(information(), 0).unwrap();
    let tx = endpoint.transmission().unwrap().clone();
    endpoint.request(information(), 1).unwrap();
    assert!(endpoint.transmission().unwrap().matches(&tx));
    let mut changed = information();
    changed.voice_messages = Some(vec![0]);
    assert_eq!(endpoint.request(changed, 2), Err(FlashError::Busy));
    let fields = ReverseFlashFields {
        feature: Some(vec![7]),
        ..Default::default()
    };
    assert_eq!(endpoint.reverse_is_new(0, &fields), Ok(true));
    endpoint.record_reverse(0, fields.clone());
    assert_eq!(endpoint.reverse_is_new(0, &fields), Ok(false));
    assert_eq!(endpoint.received_count(), 1);
    assert_eq!(endpoint.reverse_is_new(1, &fields), Ok(true));
    assert_eq!(endpoint.number(), Some(0));
}
