use super::*;
use crate::core::{messaging::LocalMessaging, registry::ProvisionedSubscriber};
use crate::l3::sms::{Metadata, UserData};
const SENDER: MobileIdentity = MobileIdentity::Min34(1);
const RECIPIENT: MobileIdentity = MobileIdentity::Min34(2);
fn core() -> LocalMessaging {
    let mut store = StateStore::open(":memory:").unwrap();
    store
        .seed_once(&[
            ProvisionedSubscriber {
                identity: SENDER,
                number: "1500".into(),
            },
            ProvisionedSubscriber {
                identity: RECIPIENT,
                number: "1501".into(),
            },
        ])
        .unwrap();
    let mut core = LocalMessaging::from_store(store).unwrap();
    core.enable_sms_hub(true);
    core
}
fn submit(text: &str) -> Vec<u8> {
    SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 3,
            privacy: 0,
            urgency: sms::NORMAL_URGENCY,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::from_text(text).unwrap(),
    }
    .encode()
    .unwrap()
}
fn address(number: &str) -> AddressInfo {
    AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd(number.into()),
    }
}
fn incoming(text: &str) -> MessageBody {
    let (data_coding, text) = smpp_core::text::encode(text).unwrap();
    MessageBody {
        source: Address::unknown("2000"),
        destination: Address::unknown("1501"),
        data_coding,
        text,
        priority_flag: sms::NORMAL_URGENCY,
        ..Default::default()
    }
}
#[test]
fn external_submission_has_real_queue_id_and_durable_replay_binding() {
    let mut core = core();
    let payload = submit("héllo 世界");
    let result = core
        .submit_complete_sms(SENDER, &address("2000"), &payload, "stable", 1000)
        .unwrap();
    assert!(result.delivery.is_none());
    let job = core
        .store_mut()
        .next_hub_sms(1000, false, "peer")
        .unwrap()
        .unwrap();
    assert_eq!(job.body.source.digits, "1500");
    assert_eq!(job.body.destination.digits, "2000");
    assert_eq!(smpp_core::text::decode(&job.body).unwrap(), "héllo 世界");
    assert_eq!(job.body.priority_flag, sms::NORMAL_URGENCY);
    core.enable_sms_hub(false);
    let bound = crate::core::messaging::SmsDestination::external("2000".into()).unwrap();
    let replay = core
        .submit_bound_sms(SENDER, &bound, &payload, "stable", 1001)
        .unwrap();
    assert_eq!(result.id, replay.id);
    assert!(
        core.submit_bound_sms(SENDER, &bound, &submit("different"), "stable", 1002)
            .is_err()
    );
    let visible = core.store().operation_messages(20).unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].recipient, "2000");
    assert!(core.store().pending_for(SENDER).unwrap().is_empty());
}
#[test]
fn disabled_and_disabled_local_numbers_never_route_externally() {
    let mut core = core();
    core.enable_sms_hub(false);
    assert!(
        core.submit_complete_sms(SENDER, &address("2000"), &submit("test"), "a", 1000)
            .is_err()
    );
    core.enable_sms_hub(true);
    core.delete_subscriber(RECIPIENT).unwrap();
    assert!(
        core.submit_complete_sms(SENDER, &address("1501"), &submit("test"), "b", 1000)
            .is_err()
    );
    assert_eq!(core.store().hub_queue_depth().unwrap(), 0);
}
#[test]
fn incoming_unicode_is_queued_with_normal_display_and_receipt_after_handset_acceptance() {
    let mut core = core();
    let mut message = incoming("hello 世界");
    message.registered_delivery = 1;
    let peer_id = core
        .store_mut()
        .receive_hub_sms("peer", &message, 1000)
        .unwrap();
    let queued = core.store().pending_for(RECIPIENT).unwrap();
    assert_eq!(queued.len(), 1);
    let SmsMessage::Deliver {
        user_data,
        display_time,
        metadata,
        ..
    } = sms::decode_prefix(&queued[0].sms_pdu, Direction::Forward)
        .unwrap()
        .message
    else {
        panic!()
    };
    assert_eq!(user_data.text().as_deref(), Some("hello 世界"));
    assert_eq!(display_time, sms::DisplayTime::Invoke);
    assert!(!metadata.delivery_acknowledgment);
    core.store_mut().queue_hub_reports(1001).unwrap();
    assert!(
        core.store_mut()
            .next_hub_sms(1001, true, "peer")
            .unwrap()
            .is_none()
    );
    core.store_mut()
        .record_relay_accept(queued[0].id, RECIPIENT)
        .unwrap();
    core.store_mut().queue_hub_reports(1002).unwrap();
    let job = core
        .store_mut()
        .next_hub_sms(1002, true, "peer")
        .unwrap()
        .unwrap();
    assert_eq!(
        job.body.receipted_message_id.as_deref(),
        Some(peer_id.as_str())
    );
    assert_eq!(
        job.body.message_state,
        Some(smpp_core::pdu::message_state::DELIVERED)
    );
    assert_eq!(job.body.source.digits, "1501");
    assert_eq!(job.body.destination.digits, "2000");
}
#[test]
fn sar_parts_reassemble_once_and_each_report_keeps_its_submit_id() {
    let mut core = core();
    let mut ids = Vec::new();
    for (sequence, text) in [(2, "世界"), (1, "hello ")] {
        let mut part = incoming(text);
        part.sar_reference = Some(42);
        part.sar_total = Some(2);
        part.sar_sequence = Some(sequence);
        part.data_coding = smpp_core::text::UCS2;
        part.text = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        part.registered_delivery = 1;
        let id = core
            .store_mut()
            .receive_hub_sms("peer", &part, 1000)
            .unwrap();
        assert_eq!(
            core.store_mut()
                .receive_hub_sms("peer", &part, 1001)
                .unwrap(),
            id
        );
        ids.push(id);
    }
    let queued = core.store().pending_for(RECIPIENT).unwrap();
    assert_eq!(queued.len(), 1);
    let SmsMessage::Deliver { user_data, .. } =
        sms::decode_prefix(&queued[0].sms_pdu, Direction::Forward)
            .unwrap()
            .message
    else {
        panic!()
    };
    assert_eq!(user_data.text().as_deref(), Some("hello 世界"));
    core.store_mut()
        .record_relay_accept(queued[0].id, RECIPIENT)
        .unwrap();
    core.store_mut().queue_hub_reports(1002).unwrap();
    let mut got = Vec::new();
    while let Some(job) = core.store_mut().next_hub_sms(1002, true, "peer").unwrap() {
        got.push(job.body.receipted_message_id.unwrap());
        core.store_mut()
            .hub_sms_result(job.id, smpp_core::CommandStatus::Ok, "", "peer", 1002)
            .unwrap();
    }
    got.sort();
    ids.sort();
    assert_eq!(got, ids);
}
#[test]
fn legitimate_identical_messages_without_reference_are_not_deduplicated() {
    let mut core = core();
    let message = incoming("same");
    let a = core
        .store_mut()
        .receive_hub_sms("peer", &message, 1000)
        .unwrap();
    let b = core
        .store_mut()
        .receive_hub_sms("peer", &message, 1001)
        .unwrap();
    assert_ne!(a, b);
    assert_eq!(core.store().pending_for(RECIPIENT).unwrap().len(), 2);
}
#[test]
fn peer_acceptance_is_not_delivery_and_receipts_require_matching_addresses() {
    let mut core = core();
    let result = core
        .submit_complete_sms(SENDER, &address("2000"), &submit("text"), "a", 1000)
        .unwrap();
    let job = core
        .store_mut()
        .next_hub_sms(1000, false, "peer")
        .unwrap()
        .unwrap();
    core.store_mut()
        .hub_sms_result(
            job.id,
            smpp_core::CommandStatus::Ok,
            "remote-id",
            "peer",
            1001,
        )
        .unwrap();
    assert_eq!(core.store().state(result.id).unwrap(), QueueState::Queued);
    let mut receipt = MessageBody {
        source: Address::unknown("2000"),
        destination: Address::unknown("1500"),
        esm_class: smpp_core::esm_class::DELIVERY_RECEIPT,
        data_coding: smpp_core::text::IA5,
        text: b"id:remote-id sub:001 dlvrd:001 stat:DELIVRD err:000 text:text".to_vec(),
        ..Default::default()
    };
    receipt.source.digits = "forged".into();
    assert!(
        core.store_mut()
            .receive_hub_receipt("peer", &receipt)
            .is_err()
    );
    receipt.source.digits = "2000".into();
    assert!(
        core.store_mut()
            .receive_hub_receipt("other-peer", &receipt)
            .is_err()
    );
    core.store_mut()
        .receive_hub_receipt("peer", &receipt)
        .unwrap();
    assert_eq!(
        core.store().state(result.id).unwrap(),
        QueueState::Delivered
    );
    receipt.receipted_message_id = Some("remote-id".into());
    receipt.message_state = Some(smpp_core::pdu::message_state::DELIVERED);
    receipt.text.clear();
    core.store_mut()
        .receive_hub_receipt("peer", &receipt)
        .unwrap();
    receipt.message_state = Some(smpp_core::pdu::message_state::UNDELIVERABLE);
    assert!(
        core.store_mut()
            .receive_hub_receipt("peer", &receipt)
            .is_err()
    );
}

#[test]
fn lost_response_recovery_keeps_payload_and_expires_only_unaccepted_messages() {
    let mut core = core();
    let result = core
        .submit_complete_sms(SENDER, &address("2000"), &submit("retry"), "retry", 1000)
        .unwrap();
    let first = core
        .store_mut()
        .next_hub_sms(1000, false, "peer")
        .unwrap()
        .unwrap();
    assert!(
        core.store_mut()
            .next_hub_sms(1000, false, "peer")
            .unwrap()
            .is_none()
    );
    core.store_mut().recover_hub_sms().unwrap();
    let recovered = core
        .store_mut()
        .next_hub_sms(1001, false, "peer")
        .unwrap()
        .unwrap();
    assert_eq!(first.id, recovered.id);
    assert_eq!(wire(first.body).unwrap(), wire(recovered.body).unwrap());
    core.store_mut()
        .hub_sms_result(
            recovered.id,
            smpp_core::CommandStatus::Throttled,
            "",
            "peer",
            1001,
        )
        .unwrap();
    assert!(
        core.store_mut()
            .next_hub_sms(1002, false, "peer")
            .unwrap()
            .is_none()
    );
    assert!(
        core.store_mut()
            .next_hub_sms(1000 + MESSAGE_TTL_MS, false, "peer")
            .unwrap()
            .is_none()
    );
    assert_eq!(core.store().state(result.id).unwrap(), QueueState::Failed);
}
#[test]
fn incomplete_multipart_expires_with_correlated_failure_report() {
    let mut core = core();
    let mut message = incoming("part");
    message.sar_reference = Some(10);
    message.sar_total = Some(2);
    message.sar_sequence = Some(1);
    message.registered_delivery = smpp_core::pdu::DELIVERY_RECEIPT_FAILURE_ONLY;
    let id = core
        .store_mut()
        .receive_hub_sms("peer", &message, 1000)
        .unwrap();
    core.store_mut().queue_hub_reports(1001).unwrap();
    assert!(
        core.store_mut()
            .next_hub_sms(1001, true, "peer")
            .unwrap()
            .is_none()
    );
    core.store_mut()
        .queue_hub_reports(1000 + MESSAGE_TTL_MS)
        .unwrap();
    let report = core
        .store_mut()
        .next_hub_sms(1000 + MESSAGE_TTL_MS, true, "peer")
        .unwrap()
        .unwrap();
    assert_eq!(report.body.receipted_message_id, Some(id));
    assert_eq!(
        report.body.message_state,
        Some(smpp_core::pdu::message_state::EXPIRED)
    );
    assert!(core.store().pending_for(RECIPIENT).unwrap().is_empty());
}
#[test]
fn conflicting_multipart_metadata_and_oversized_text_are_rejected() {
    let mut core = core();
    let mut message = incoming("part");
    message.sar_reference = Some(10);
    message.sar_total = Some(2);
    message.sar_sequence = Some(1);
    core.store_mut()
        .receive_hub_sms("peer", &message, 1000)
        .unwrap();
    message.sar_sequence = Some(2);
    message.priority_flag = 3;
    assert!(
        core.store_mut()
            .receive_hub_sms("peer", &message, 1001)
            .is_err()
    );
    message.priority_flag = sms::NORMAL_URGENCY;
    message.sar_total = Some(3);
    assert!(
        core.store_mut()
            .receive_hub_sms("peer", &message, 1001)
            .is_err()
    );
    let message = incoming(&"a".repeat(255));
    assert!(
        core.store_mut()
            .receive_hub_sms("peer", &message, 1002)
            .is_err()
    );
    let message = MessageBody {
        data_coding: smpp_core::text::UCS2,
        text: vec![0xd8, 0x3d, 0xde, 0x00],
        ..incoming("placeholder")
    };
    assert!(
        core.store_mut()
            .receive_hub_sms("peer", &message, 1002)
            .is_err()
    );
    assert!(core.store().pending_for(RECIPIENT).unwrap().is_empty());
}
#[test]
fn network_delivery_receipt_does_not_replace_user_read_or_privacy_options() {
    let mut core = core();
    for (read, manual, privacy) in [(true, false, 0), (false, true, 0), (false, false, 1)] {
        let payload = SmsMessage::Submit {
            metadata: Metadata {
                message_reference: 3,
                privacy,
                urgency: sms::NORMAL_URGENCY,
                delivery_acknowledgment: read,
                manual_acknowledgment: manual,
                new_message: true,
            },
            user_data: UserData::from_text("private").unwrap(),
        }
        .encode()
        .unwrap();
        assert!(
            core.submit_complete_sms(SENDER, &address("2000"), &payload, "unsupported", 1000)
                .is_err()
        );
    }
    assert_eq!(core.store().hub_queue_depth().unwrap(), 0);
}

#[test]
fn multipart_admission_cannot_follow_number_reassignment() {
    let mut core = core();
    let mut message = incoming("part");
    message.sar_reference = Some(10);
    message.sar_total = Some(2);
    message.sar_sequence = Some(1);
    core.store_mut()
        .receive_hub_sms("peer", &message, 1000)
        .unwrap();
    core.store_mut()
        .reconcile(&[
            ProvisionedSubscriber {
                identity: SENDER,
                number: "1500".into(),
            },
            ProvisionedSubscriber {
                identity: RECIPIENT,
                number: "1502".into(),
            },
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(3),
                number: "1501".into(),
            },
        ])
        .unwrap();
    message.sar_sequence = Some(2);
    assert!(
        core.store_mut()
            .receive_hub_sms("peer", &message, 1001)
            .is_err()
    );
    assert!(
        core.store()
            .pending_for(MobileIdentity::Min34(3))
            .unwrap()
            .is_empty()
    );
}
#[test]
fn reports_are_not_sent_to_a_replacement_peer_and_failure_only_suppresses_success() {
    let mut core = core();
    for request in [
        smpp_core::REGISTERED_DELIVERY_RECEIPT,
        smpp_core::pdu::DELIVERY_RECEIPT_FAILURE_ONLY,
    ] {
        let mut message = incoming("delivered");
        message.registered_delivery = request;
        core.store_mut()
            .receive_hub_sms("peer", &message, 1000)
            .unwrap();
    }
    for message in core.store().pending_for(RECIPIENT).unwrap() {
        core.store_mut()
            .record_relay_accept(message.id, RECIPIENT)
            .unwrap();
    }
    core.store_mut().queue_hub_reports(1001).unwrap();
    assert!(
        core.store_mut()
            .next_hub_sms(1001, true, "different-peer")
            .unwrap()
            .is_none()
    );
    let job = core
        .store_mut()
        .next_hub_sms(1001, true, "peer")
        .unwrap()
        .unwrap();
    assert_eq!(
        job.body.message_state,
        Some(smpp_core::pdu::message_state::DELIVERED)
    );
    assert!(
        core.store_mut()
            .next_hub_sms(1001, true, "peer")
            .unwrap()
            .is_none()
    );
}

#[test]
fn submission_with_optional_elements_is_still_routed() {
    let mut core = core();
    let mut payload = submit("options");
    payload.push(0xff);
    core.submit_complete_sms(SENDER, &address("2000"), &payload, "optional", 1000)
        .unwrap();
    let job = core
        .store_mut()
        .next_hub_sms(1000, false, "peer")
        .unwrap()
        .unwrap();
    assert_eq!(smpp_core::text::decode(&job.body).unwrap(), "options");
}
