use super::*;
fn segmented(message: &[u8], budget: usize, window: AcknowledgmentWindow) -> SegmentedMessage {
    let MessagePlan::Segmented(plan) =
        plan_message(message, budget, 5, window, ProtocolVersion::Pv1).unwrap()
    else {
        panic!("message requires TSAR")
    };
    plan
}
fn check(plan: &SegmentedMessage, original: &[u8]) {
    assert_eq!(plan.message_octets(), original.len());
    assert!((2..=256).contains(&plan.segments().len()));
    let mut combined = Vec::new();
    for (index, segment) in plan.segments().iter().enumerate() {
        assert_eq!(segment.sequence_number, index as u8);
        assert_eq!(segment.transaction_id, 5);
        assert!(
            segment.encode(ProtocolVersion::Pv1).unwrap().len() <= plan.payload_budget_octets()
        );
        if index == 0 {
            let SegmentKind::Begin { services, .. } = segment.kind else {
                panic!("first segment must be Begin")
            };
            assert_eq!(
                services.unwrap_or_default().acknowledgment_window,
                plan.acknowledgment_window()
            );
        } else if index == plan.segments().len() - 1 {
            assert!(matches!(segment.kind, SegmentKind::End { .. }));
        } else {
            assert!(matches!(segment.kind, SegmentKind::Continue { .. }));
        }
        combined.extend_from_slice(&segment.segment_data);
    }
    assert_eq!(combined, original);
    assert_eq!(
        SegmentedMessage::try_from_frozen(
            plan.segments().to_vec(),
            original,
            plan.payload_budget_octets(),
            ProtocolVersion::Pv1,
        )
        .unwrap(),
        *plan,
    );
}
#[test]
fn fitting_messages_bypass_tsar_without_changing_payload_or_requiring_tsar_version() {
    for message in [vec![], vec![0xaa], vec![0xff; 254]] {
        assert_eq!(
            plan_message(
                &message,
                message.len(),
                255,
                AcknowledgmentWindow::Every4,
                ProtocolVersion::Is136RevA
            )
            .unwrap(),
            MessagePlan::Unsegmented(message)
        );
    }
    assert_eq!(
        plan_message(
            &[0; 10],
            255,
            5,
            AcknowledgmentWindow::Prohibited,
            ProtocolVersion::Pv1
        ),
        Err(PlanningError::PayloadBudget)
    );
}
#[test]
fn fixed_budget_vectors_include_header_and_optional_ie_overhead() {
    let message: Vec<_> = (0..12).collect();
    let plan = segmented(&message, 8, AcknowledgmentWindow::Prohibited);
    check(&plan, &message);
    let encoded: Vec<_> = plan
        .segments()
        .iter()
        .map(|segment| segment.encode(ProtocolVersion::Pv1).unwrap())
        .collect();
    assert_eq!(
        encoded,
        [
            vec![5, 0, 3, 0, 1, 2, 0x10, 0x0c],
            vec![0x0d, 1, 5, 3, 4, 5, 6, 7],
            vec![0x15, 2, 4, 8, 9, 10, 11]
        ]
    );
    let plan = segmented(&message, 8, AcknowledgmentWindow::Every8);
    assert_eq!(plan.segments().len(), 4);
    assert_eq!(
        plan.segments()[0].encode(ProtocolVersion::Pv1).unwrap(),
        [5, 0, 1, 0, 0x10, 0x0c, 0x20, 0x10]
    );
    check(&plan, &message);
}
#[test]
fn optional_length_is_omitted_when_unrepresentable_or_when_it_would_prevent_first_data() {
    for length in [4095, 4096, 10_000] {
        let message = vec![0xab; length];
        let plan = segmented(&message, 64, AcknowledgmentWindow::Last);
        check(&plan, &message);
        let SegmentKind::Begin { message_length, .. } = plan.segments()[0].kind else {
            unreachable!()
        };
        assert_eq!(message_length, (length <= 4095).then_some(length as u16));
    }
    let plan = segmented(&[1; 6], 4, AcknowledgmentWindow::Prohibited);
    check(&plan, &[1; 6]);
    assert!(matches!(
        plan.segments()[0].kind,
        SegmentKind::Begin {
            message_length: None,
            ..
        }
    ));
}
#[test]
fn sequence_space_header_capacity_and_version_failures_are_explicit() {
    let message = vec![0; 256];
    let plan = segmented(&message, 4, AcknowledgmentWindow::Prohibited);
    assert_eq!(plan.segments().len(), 256);
    check(&plan, &message);
    assert_eq!(
        plan_message(
            &[0; 257],
            4,
            5,
            AcknowledgmentWindow::Prohibited,
            ProtocolVersion::Pv1
        ),
        Err(PlanningError::SegmentCount)
    );
    assert_eq!(
        plan_message(
            &[0; 4],
            3,
            5,
            AcknowledgmentWindow::Prohibited,
            ProtocolVersion::Pv1
        ),
        Err(PlanningError::HeaderCapacity)
    );
    assert_eq!(
        plan_message(
            &[0; 5],
            4,
            5,
            AcknowledgmentWindow::Every4,
            ProtocolVersion::Pv1
        ),
        Err(PlanningError::HeaderCapacity)
    );
    assert_eq!(
        plan_message(
            &[0; 6],
            5,
            8,
            AcknowledgmentWindow::Prohibited,
            ProtocolVersion::Pv1
        ),
        Err(PlanningError::Codec(CodecError::Field("transaction_id")))
    );
    assert_eq!(
        plan_message(
            &[0; 6],
            5,
            5,
            AcknowledgmentWindow::Prohibited,
            ProtocolVersion::Is136RevA
        ),
        Err(PlanningError::Codec(CodecError::Version))
    );
    let message = vec![0; 510];
    let plan = segmented(&message, 5, AcknowledgmentWindow::Every4);
    assert!(plan.segments()[0].segment_data.is_empty());
    assert_eq!(plan.segments().len(), 256);
    check(&plan, &message);
    assert_eq!(
        plan_message(
            &[0; 511],
            5,
            5,
            AcknowledgmentWindow::Every4,
            ProtocolVersion::Pv1
        ),
        Err(PlanningError::SegmentCount)
    );
}
#[test]
fn the_full_rdata_budget_handles_large_messages_without_a_false_twelve_bit_limit() {
    let message = vec![0x55; 64_256];
    let plan = segmented(&message, 254, AcknowledgmentWindow::Prohibited);
    assert_eq!(plan.segments().len(), 256);
    check(&plan, &message);
    use crate::l3::{
        legacy_traffic::{self, TrafficMessage, TrafficRelay},
        relay::{Direction, HigherLayerProtocol, RelayError, RelayMessage, Teleservice},
    };
    let relay = RelayMessage::Data {
        transaction_id: 42,
        protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
        payload: plan.segments()[0].encode(ProtocolVersion::Pv1).unwrap(),
    };
    assert_eq!(relay.encode(Direction::Reverse), Err(RelayError::Length));
    let traffic = TrafficMessage::Relay(TrafficRelay {
        message: relay,
        optional_octets: vec![],
    });
    assert_eq!(
        legacy_traffic::decode_prefix_in(&traffic.encode().unwrap(), Direction::Reverse)
            .unwrap()
            .message,
        traffic
    );
    assert_eq!(
        plan_message(
            &vec![0; 64_257],
            254,
            5,
            AcknowledgmentWindow::Prohibited,
            ProtocolVersion::Pv1
        ),
        Err(PlanningError::SegmentCount)
    );
}
#[test]
fn all_acknowledgment_windows_and_transport_budgets_preserve_data_and_frame_limits() {
    for budget in [4, 5, 6, 7, 8, 9, 16, 31, 64, 128, 240, 254] {
        for window in [
            AcknowledgmentWindow::Prohibited,
            AcknowledgmentWindow::Every4,
            AcknowledgmentWindow::Every8,
            AcknowledgmentWindow::Every16,
            AcknowledgmentWindow::Last,
            AcknowledgmentWindow::Exceptions,
        ] {
            for length in [0, 1, budget, budget + 1, budget * 3 + 17, 3000] {
                let message = vec![0xab; length];
                match plan_message(&message, budget, 5, window, ProtocolVersion::Pv1) {
                    Ok(MessagePlan::Unsegmented(bytes)) => {
                        assert!(length <= budget);
                        assert_eq!(bytes, message);
                    }
                    Ok(MessagePlan::Segmented(plan)) => {
                        assert!(length > budget);
                        assert_eq!(plan.acknowledgment_window(), window);
                        check(&plan, &message);
                    }
                    Err(PlanningError::HeaderCapacity) => {
                        assert!(budget == 4 && window != AcknowledgmentWindow::Prohibited)
                    }
                    Err(PlanningError::SegmentCount) => assert!(length > (256 - 1) * (budget - 3)),
                    Err(error) => panic!("unexpected {error:?}"),
                }
            }
        }
    }
}

#[test]
fn a_full_cmt_sms_pdu_exceeding_one_relay_is_preserved_through_both_transports() {
    use crate::l3::{
        legacy_traffic::{self, TrafficMessage, TrafficRelay},
        relay::{self, Direction, HigherLayerProtocol, RelayMessage, Teleservice},
        sms::{self, Metadata, SmsMessage, UserData},
    };
    let sms = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 42,
            privacy: 0,
            urgency: 0,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(vec![b'A'; 254]),
    };
    let pdu = sms.encode().unwrap();
    assert!(pdu.len() > MAX_DATA_PAYLOAD_OCTETS);
    let plan = segmented(&pdu, 240, AcknowledgmentWindow::Last);
    check(&plan, &pdu);
    let mut recovered = Vec::new();
    for segment in plan.segments() {
        let message = RelayMessage::Data {
            transaction_id: segment.sequence_number,
            protocol: HigherLayerProtocol::Segmented(Teleservice::Sms),
            payload: segment.encode(ProtocolVersion::Pv1).unwrap(),
        };
        for direction in [Direction::Forward, Direction::Reverse] {
            assert_eq!(
                relay::decode_prefix(&message.encode(direction).unwrap(), direction)
                    .unwrap()
                    .message,
                message
            );
            let traffic = TrafficMessage::Relay(TrafficRelay {
                message: message.clone(),
                optional_octets: vec![],
            });
            assert_eq!(
                legacy_traffic::decode_prefix_in_version(
                    &traffic.encode().unwrap(),
                    direction,
                    ProtocolVersion::Pv1
                )
                .unwrap()
                .message,
                traffic
            );
        }
        let decoded = super::super::decode_segment(
            &segment.encode(ProtocolVersion::Pv1).unwrap(),
            ProtocolVersion::Pv1,
        )
        .unwrap();
        assert!(decoded.unparsed_octets.is_empty());
        recovered.extend(decoded.segment.segment_data);
    }
    assert_eq!(recovered, pdu);
    assert_eq!(
        sms::decode_prefix(&recovered, Direction::Reverse)
            .unwrap()
            .message,
        sms
    );
}

#[test]
fn frozen_plan_preserves_noncanonical_boundaries_and_checks_original_payload() {
    let message: Vec<u8> = (0..20).collect();
    let original = segmented(&message, 10, AcknowledgmentWindow::Last);
    let mut segments = original.segments().to_vec();
    let moved = segments[1].segment_data.remove(0);
    segments[0].segment_data.push(moved);
    let restored =
        SegmentedMessage::try_from_frozen(segments.clone(), &message, 12, ProtocolVersion::Pv1)
            .unwrap();
    assert_eq!(restored.segments(), segments);
    assert_ne!(
        restored,
        segmented(&message, 12, AcknowledgmentWindow::Last)
    );
    check(&restored, &message);
    let mut corrupt = message.clone();
    corrupt[0] ^= 1;
    assert_eq!(
        SegmentedMessage::try_from_frozen(segments.clone(), &corrupt, 12, ProtocolVersion::Pv1),
        Err(PlanningError::FrozenPlan)
    );
    assert_eq!(
        SegmentedMessage::try_from_frozen(
            segments.clone(),
            &message[..19],
            12,
            ProtocolVersion::Pv1
        ),
        Err(PlanningError::FrozenPlan)
    );
    assert_eq!(
        SegmentedMessage::try_from_frozen(segments.clone(), &message, 4, ProtocolVersion::Pv1),
        Err(PlanningError::PayloadBudget)
    );
    assert_eq!(
        SegmentedMessage::try_from_frozen(segments, &message, 12, ProtocolVersion::Is136RevA),
        Err(PlanningError::Codec(CodecError::Version))
    );
}

#[test]
fn frozen_plan_rejects_inconsistent_transaction_sequence_kind_and_services() {
    let message = vec![42; 40];
    let plan = segmented(&message, 12, AcknowledgmentWindow::Last);
    for corruption in 0..7 {
        let mut segments = plan.segments().to_vec();
        match corruption {
            0 => segments[1].transaction_id ^= 1,
            1 => segments[1].sequence_number = 0,
            2 => {
                segments[1].kind = SegmentKind::End {
                    message_length: None,
                    crc: None,
                }
            }
            3 => {
                segments.last_mut().unwrap().kind = SegmentKind::Continue {
                    message_length: None,
                }
            }
            4 => {
                segments[1].kind = SegmentKind::Continue {
                    message_length: Some(41),
                }
            }
            5 => {
                if let SegmentKind::Begin {
                    services: Some(ref mut services),
                    ..
                } = segments[0].kind
                {
                    services.compression = Compression::V42Bis
                }
            }
            6 => {
                if let SegmentKind::Begin {
                    services: Some(ref mut services),
                    ..
                } = segments[0].kind
                {
                    services.crc_status = CrcStatus::Crc16
                }
            }
            _ => unreachable!(),
        }
        assert_eq!(
            SegmentedMessage::try_from_frozen(segments, &message, 14, ProtocolVersion::Pv1),
            Err(PlanningError::FrozenPlan),
            "corruption {corruption}"
        );
    }
}
