use super::*;
use crate::{
    config::Config,
    core::registry::ProvisionedSubscriber,
    l3::{
        broadcast::ProtocolVersion,
        registration::{AccessMessage, RegistrationType},
    },
    link::rach::MessageSet,
};
fn fixture() -> (LocalRegistry, IdleQueries) {
    fixture_for(MobileIdentity::Min34(1))
}
fn fixture_for(identity: MobileIdentity) -> (LocalRegistry, IdleQueries) {
    let config = Config::from_yaml(
        "radio:\n  kind: noop\ncell:\n  band: cellular800\n  channel: 1\n  sid: 42\n  dvcc: 1\n",
    )
    .unwrap();
    let messages = crate::runtime::serving::messages(&config);
    let crate::l3::broadcast::MandatoryBroadcast::DcchStructure(structure) = &messages[0] else {
        panic!()
    };
    let allocation = PagingAllocation::try_new(structure).unwrap();
    let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
        identity,
        number: "1001".into(),
    }])
    .unwrap();
    registry
        .register(
            &MessageSet {
                identity,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            0,
        )
        .unwrap();
    (registry, IdleQueries::new(allocation))
}
#[test]
fn idle_poll_starts_at_completion_and_keeps_secondary_repeat_after_early_report() {
    let (registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    let phase = idle.allocation.phase(id).unwrap();
    idle.request(&registry, id, false, 0).unwrap();
    idle.received(id, 1).unwrap();
    assert_eq!(idle.state(id), Some(PollState::Queued));
    assert!(idle.frame(1, phase).is_none());
    let primary = *idle.frame(2, phase).unwrap();
    let completion = idle.completion().unwrap();
    idle.submitted().unwrap();
    idle.received(id, 2).unwrap();
    assert_eq!(idle.state(id), Some(PollState::InFlight));
    idle.transmitted(&completion, 3).unwrap();
    idle.received(id, 4).unwrap();
    assert_eq!(idle.state(id), Some(PollState::Received));
    assert!(idle.has_repeat(3, phase));
    assert_eq!(idle.frame(3, phase), Some(&primary));
    assert!(idle.completion().is_none());
    idle.submitted().unwrap();
    assert!(!idle.has_repeat(3, phase));
}
#[test]
fn idle_poll_expiry_duplicate_commands_and_stale_completion_preserve_new_exchange() {
    let (registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    let phase = idle.allocation.phase(id).unwrap();
    idle.request(&registry, id, false, 0).unwrap();
    idle.request(&registry, id, false, super::super::timers::SPACH_MS - 1)
        .unwrap();
    idle.advance(&registry, |_| false, super::super::timers::SPACH_MS)
        .unwrap();
    assert_eq!(idle.state(id), Some(PollState::TimedOut));
    idle.request(&registry, id, false, 7000).unwrap();
    idle.frame(2, phase).unwrap();
    let old = idle.completion().unwrap();
    idle.submitted().unwrap();
    idle.transmitted(&old, 7010).unwrap();
    idle.advance(&registry, |_| false, 7010 + RESPONSE_TIMEOUT_MS)
        .unwrap();
    assert_eq!(idle.state(id), Some(PollState::TimedOut));
    idle.request(&registry, id, false, 40000).unwrap();
    idle.transmitted(&old, 40001).unwrap();
    idle.received(id, 40002).unwrap();
    assert_eq!(idle.state(id), Some(PollState::Queued));
    idle.frame(4, phase).unwrap();
    let current = idle.completion().unwrap();
    idle.submitted().unwrap();
    idle.transmitted(&current, 40003).unwrap();
    idle.received(MobileIdentity::Min34(2), 40004).unwrap();
    assert!(matches!(
        idle.state(id),
        Some(PollState::AwaitingReport { .. })
    ));
    idle.shutdown();
    idle.transmitted(&current, 40005).unwrap();
    idle.received(id, 40006).unwrap();
    assert_eq!(idle.state(id), Some(PollState::Cancelled));
    assert_eq!(
        idle.advance(&registry, |_| false, 40005),
        Err(PollError::ClockRegression)
    );
}
#[test]
fn idle_poll_cancels_on_registration_provisioning_and_busy_state_changes() {
    let (mut registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    assert_eq!(idle.request(&registry, id, true, 0), Err(PollError::Busy));
    assert_eq!(
        idle.request(&registry, MobileIdentity::Min34(2), false, 0),
        Err(PollError::UnknownSubscriber)
    );
    idle.request(&registry, id, false, 0).unwrap();
    idle.advance(&registry, |_| true, 1).unwrap();
    assert_eq!(idle.state(id), Some(PollState::Cancelled));
    idle.request(&registry, id, false, 2).unwrap();
    registry
        .replace_provisioning(vec![ProvisionedSubscriber {
            identity: id,
            number: "2001".into(),
        }])
        .unwrap();
    idle.advance(&registry, |_| false, 3).unwrap();
    assert_eq!(idle.state(id), Some(PollState::Cancelled));
    idle.request(&registry, id, false, 4).unwrap();
    registry
        .register(
            &MessageSet {
                identity: id,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerDown,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            1,
        )
        .unwrap();
    idle.advance(&registry, |_| false, 5).unwrap();
    assert_eq!(idle.state(id), Some(PollState::Cancelled));
    assert_eq!(
        idle.request(&registry, id, false, 6),
        Err(PollError::Offline)
    );
}
#[test]
fn idle_poll_preserves_multiple_pch_repeats_and_bounds_history_without_evicting_pending_work() {
    let (mut registry, mut idle) = fixture();
    registry
        .replace_provisioning(
            (1..=MAX_POLLS + 1)
                .map(|number| ProvisionedSubscriber {
                    identity: MobileIdentity::Min34(number as u64),
                    number: format!("{number}"),
                })
                .collect(),
        )
        .unwrap();
    let reg = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 15,
        protocol_version: ProtocolVersion::Is136RevA,
    }
    .encode()
    .unwrap();
    for number in 1..=MAX_POLLS + 1 {
        registry
            .register(
                &MessageSet {
                    identity: MobileIdentity::Min34(number as u64),
                    messages: vec![reg.clone()],
                },
                0,
            )
            .unwrap();
    }
    for number in 1..=MAX_POLLS {
        idle.request(&registry, MobileIdentity::Min34(number as u64), false, 0)
            .unwrap();
    }
    assert_eq!(
        idle.request(
            &registry,
            MobileIdentity::Min34((MAX_POLLS + 1) as u64),
            false,
            0
        ),
        Err(PollError::Full)
    );
    let one = idle.allocation.phase(MobileIdentity::Min34(1)).unwrap();
    let two = idle.allocation.phase(MobileIdentity::Min34(2)).unwrap();
    assert_ne!(one, two);
    idle.frame(0, one).unwrap();
    idle.submitted().unwrap();
    idle.frame(0, two).unwrap();
    idle.submitted().unwrap();
    assert!(idle.frame(0, two).is_none());
    assert!(idle.has_repeat(1, one) && idle.has_repeat(1, two));
    idle.frame(1, one).unwrap();
    idle.submitted().unwrap();
    idle.frame(1, two).unwrap();
    idle.submitted().unwrap();
    idle.advance(&registry, |id| id == MobileIdentity::Min34(1), 1)
        .unwrap();
    idle.request(
        &registry,
        MobileIdentity::Min34((MAX_POLLS + 1) as u64),
        false,
        2,
    )
    .unwrap();
    assert!(idle.state(MobileIdentity::Min34(1)).is_none());
    assert_eq!(idle.polls.len(), MAX_POLLS);
}
fn audit_request(
    idle: &mut IdleQueries,
    registry: &LocalRegistry,
    force: bool,
    now: u64,
) -> Completion {
    let id = MobileIdentity::Min34(1);
    idle.request_audit(
        registry,
        id,
        crate::l3::audit::AuditOrder {
            force_registration: force,
            ..Default::default()
        },
        false,
        now,
    )
    .unwrap();
    let phase = idle.allocation.phase(id).unwrap();
    idle.frame(now / 1000 * 2, phase).unwrap();
    let completion = idle.completion().unwrap();
    idle.submitted().unwrap();
    idle.transmitted(&completion, now + 1).unwrap();
    completion
}
fn forced_registration(registry: &mut LocalRegistry, superframe: u64) {
    registry
        .register(
            &MessageSet {
                identity: MobileIdentity::Min34(1),
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::Forced,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            superframe,
        )
        .unwrap();
}
#[test]
fn audit_force_requires_confirmation_and_forced_registration_in_either_order() {
    for registration_first in [false, true] {
        let (mut registry, mut idle) = fixture();
        let id = MobileIdentity::Min34(1);
        audit_request(&mut idle, &registry, true, 0);
        if registration_first {
            forced_registration(&mut registry, 1);
            idle.registration_received(&registry, id, RegistrationType::Forced, false, 2)
                .unwrap();
        }
        idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 3)
            .unwrap();
        if !registration_first {
            assert!(matches!(
                idle.audit_state(id),
                Some(PollState::AwaitingReport { .. })
            ));
            assert!(idle.audit_confirmation(id).is_some());
            forced_registration(&mut registry, 1);
            idle.registration_received(&registry, id, RegistrationType::Forced, false, 4)
                .unwrap();
        }
        assert_eq!(idle.audit_state(id), Some(PollState::Received));
        assert_eq!(idle.forced_registration_received(id), Some(true));
        idle.advance(&registry, |_| false, 5).unwrap();
        assert_eq!(idle.audit_state(id), Some(PollState::Received));
    }
}
#[test]
fn audit_isolated_from_capability_replies_and_enforces_response_scope() {
    let (registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    audit_request(&mut idle, &registry, false, 0);
    assert_eq!(idle.request(&registry, id, false, 2), Err(PollError::Busy));
    idle.received(id, 3).unwrap();
    assert!(matches!(
        idle.audit_state(id),
        Some(PollState::AwaitingReport { .. })
    ));
    assert_eq!(
        idle.received_audit(
            id,
            crate::l3::audit::AuditConfirmation {
                display: Some("debug".into()),
                ..Default::default()
            },
            4
        ),
        Err(PollError::ResponseFields)
    );
    assert!(idle.audit_confirmation(id).is_none());
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 5)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Received));
    assert_eq!(
        registry.subscribers()[0]
            .registration
            .unwrap()
            .last_registration_superframe,
        0
    );
    idle.request(&registry, id, false, 6).unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Received));
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 7)
        .unwrap();
    assert_eq!(idle.state(id), Some(PollState::Queued));
}
#[test]
fn forced_registration_before_transmission_and_missing_stage_cannot_complete_audit() {
    let (mut registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    idle.request_audit(
        &registry,
        id,
        crate::l3::audit::AuditOrder {
            force_registration: true,
            ..Default::default()
        },
        false,
        0,
    )
    .unwrap();
    forced_registration(&mut registry, 1);
    idle.registration_received(&registry, id, RegistrationType::Forced, false, 1)
        .unwrap();
    idle.advance(&registry, |_| false, 2).unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Cancelled));
    audit_request(&mut idle, &registry, true, 3);
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 5)
        .unwrap();
    idle.advance(&registry, |_| false, 4 + RESPONSE_TIMEOUT_MS)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::TimedOut));
    assert_eq!(idle.forced_registration_received(id), Some(false));
}
#[test]
fn audit_debug_permission_and_same_batch_busy_cancellation_are_enforced() {
    let (mut registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    idle.request_audit(
        &registry,
        id,
        crate::l3::audit::AuditOrder {
            debug_display_allowed: true,
            ..Default::default()
        },
        false,
        0,
    )
    .unwrap();
    let phase = idle.allocation.phase(id).unwrap();
    idle.frame(0, phase).unwrap();
    let tx = idle.completion().unwrap();
    assert_eq!(tx.bytes, [0x12, 0x40]);
    idle.submitted().unwrap();
    idle.transmitted(&tx, 1).unwrap();
    idle.received_audit(
        id,
        crate::l3::audit::AuditConfirmation {
            display: Some("debug".into()),
            ..Default::default()
        },
        2,
    )
    .unwrap();
    assert_eq!(
        idle.audit_confirmation(id).unwrap().display.as_deref(),
        Some("debug")
    );
    audit_request(&mut idle, &registry, true, 2000);
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 2002)
        .unwrap();
    forced_registration(&mut registry, 2);
    idle.registration_received(&registry, id, RegistrationType::Forced, true, 2003)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Cancelled));
    assert_eq!(idle.forced_registration_received(id), Some(false));
}
#[test]
fn audit_can_reach_provisioned_offline_phone_without_inventing_registration() {
    let (mut registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    registry
        .register(
            &MessageSet {
                identity: id,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerDown,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            1,
        )
        .unwrap();
    assert_eq!(
        idle.request(&registry, id, false, 0),
        Err(PollError::Offline)
    );
    audit_request(&mut idle, &registry, false, 0);
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 2)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Received));
    assert!(registry.subscribers()[0].registration.is_none());
    audit_request(&mut idle, &registry, true, 2000);
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 2002)
        .unwrap();
    assert!(matches!(
        idle.audit_state(id),
        Some(PollState::AwaitingReport { .. })
    ));
    forced_registration(&mut registry, 3);
    idle.registration_received(&registry, id, RegistrationType::Forced, false, 2003)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Received));
    assert!(registry.subscribers()[0].registration.is_some());
}
#[test]
fn audit_confirmation_before_final_transmission_or_from_other_identity_is_not_receipt() {
    let (registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    idle.request_audit(
        &registry,
        id,
        crate::l3::audit::AuditOrder::default(),
        false,
        0,
    )
    .unwrap();
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 1)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Queued));
    assert!(idle.audit_confirmation(id).is_none());
    let phase = idle.allocation.phase(id).unwrap();
    idle.frame(0, phase).unwrap();
    let tx = idle.completion().unwrap();
    idle.submitted().unwrap();
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 2)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::InFlight));
    assert!(idle.audit_confirmation(id).is_none());
    idle.transmitted(&tx, 3).unwrap();
    idle.received_audit(
        MobileIdentity::Min34(2),
        crate::l3::audit::AuditConfirmation::default(),
        4,
    )
    .unwrap();
    assert!(matches!(
        idle.audit_state(id),
        Some(PollState::AwaitingReport { .. })
    ));
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 5)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Received));
}

fn waiting_message() -> crate::l3::message_waiting::MessageWaiting {
    crate::l3::message_waiting::MessageWaiting {
        entries: vec![
            crate::l3::message_waiting::Waiting { kind: 0, count: 63 },
            crate::l3::message_waiting::Waiting { kind: 1, count: 0 },
        ],
        display: Some("D".repeat(82)),
        ..Default::default()
    }
}
fn waiting_confirmation() -> crate::l3::notification::SpachConfirmation {
    crate::l3::notification::SpachConfirmation {
        confirmed_type: crate::l3::message_waiting::MESSAGE_TYPE,
        ..Default::default()
    }
}
#[test]
fn waiting_primary_fragments_skip_secondary_superframes_and_keep_exact_secondary_copies() {
    let (_, probe) = fixture();
    let id = (1..128)
        .map(MobileIdentity::Min34)
        .find(|id| probe.allocation.phase(*id).unwrap() == 31)
        .unwrap();
    let (registry, mut idle) = fixture_for(id);
    let message = waiting_message();
    idle.request_message_waiting(&registry, id, message.clone(), false, 0)
        .unwrap();
    let mut primary = Vec::new();
    let mut copies = Vec::new();
    let mut completions = Vec::new();
    let mut receiver = crate::link::spach::SpachReceiver::default();
    for sf in 2..8 {
        for phase in 0..32 {
            if let Some(frame) = idle.frame(sf, phase).copied() {
                if let Some(completion) = idle.completion() {
                    primary.push((sf, phase, frame));
                    if primary.len() == 1 {
                        assert!(idle.frame_count() > 32);
                        assert!(idle.adds_repeats() > 1);
                    }
                    if let Some(decoded) = receiver.push(&frame).unwrap() {
                        assert_eq!(decoded.message, message.encode().unwrap());
                    }
                    completions.push(completion);
                } else {
                    copies.push((sf, phase, frame));
                }
                idle.submitted().unwrap();
            }
        }
    }
    assert!(primary.len() > 2);
    assert_eq!(primary[0].0, 2);
    assert_eq!(primary[0].1, 31);
    for pair in primary.windows(2) {
        assert_eq!(
            idle.allocation.continuation(pair[0].0, pair[0].1),
            Some((pair[1].0, pair[1].1))
        );
    }
    assert_eq!(primary.len(), copies.len());
    for (sf, phase, frame) in &primary {
        assert!(copies.contains(&(sf + 1, *phase, *frame)));
    }
    assert_eq!(idle.message_waiting_state(id), Some(PollState::InFlight));
    for completion in completions.iter().rev().take(completions.len() - 1) {
        idle.transmitted(completion, 2).unwrap();
    }
    assert_eq!(idle.message_waiting_state(id), Some(PollState::InFlight));
    idle.transmitted(&completions[0], 3).unwrap();
    assert_eq!(
        idle.message_waiting_state(id),
        Some(PollState::AwaitingReport { deadline_ms: 30003 })
    );
    idle.received_indication(id, &waiting_confirmation(), 4)
        .unwrap();
    assert_eq!(idle.message_waiting_state(id), Some(PollState::Received));
    assert_eq!(
        idle.request_message_waiting(&registry, id, message, false, 5),
        Err(PollError::Busy)
    );
}
#[test]
fn waiting_missing_continuation_and_busy_cancellation_ignore_stale_receipts() {
    let (registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    idle.request_message_waiting(&registry, id, waiting_message(), false, 0)
        .unwrap();
    let phase = idle.allocation.phase(id).unwrap();
    idle.frame(2, phase).unwrap();
    let stale = idle.completion().unwrap();
    idle.submitted().unwrap();
    assert!(idle.frame(2, phase + 3).is_none());
    assert_eq!(idle.message_waiting_state(id), Some(PollState::Cancelled));
    idle.transmitted(&stale, 1).unwrap();
    assert_eq!(idle.message_waiting_state(id), Some(PollState::Cancelled));
    assert_eq!(
        idle.request_message_waiting(&registry, id, waiting_message(), false, 2),
        Err(PollError::Busy)
    );
    idle.request_message_waiting(&registry, id, waiting_message(), false, RESPONSE_TIMEOUT_MS)
        .unwrap();
    idle.advance(&registry, |_| true, RESPONSE_TIMEOUT_MS + 1)
        .unwrap();
    idle.transmitted(&stale, RESPONSE_TIMEOUT_MS + 2).unwrap();
    assert_eq!(idle.message_waiting_state(id), Some(PollState::Cancelled));
    assert!(idle.has_repeat(3, phase));
}
#[test]
fn waiting_confirmation_requires_matching_type_scope_identity_and_live_deadline() {
    let (registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    let message = crate::l3::message_waiting::MessageWaiting {
        entries: vec![crate::l3::message_waiting::Waiting { kind: 1, count: 0 }],
        ..Default::default()
    };
    idle.request_message_waiting(&registry, id, message, false, 0)
        .unwrap();
    let phase = idle.allocation.phase(id).unwrap();
    idle.frame(2, phase).unwrap();
    let completion = idle.completion().unwrap();
    idle.submitted().unwrap();
    idle.transmitted(&completion, 10).unwrap();
    let mut wrong = waiting_confirmation();
    wrong.confirmed_type = 26;
    assert_eq!(
        idle.received_indication(id, &wrong, 11),
        Err(PollError::ResponseFields)
    );
    idle.received_indication(MobileIdentity::Min34(2), &waiting_confirmation(), 12)
        .unwrap();
    assert_eq!(
        idle.message_waiting_state(id),
        Some(PollState::AwaitingReport { deadline_ms: 30010 })
    );
    idle.received_indication(id, &waiting_confirmation(), 30010)
        .unwrap();
    assert_eq!(idle.message_waiting_state(id), Some(PollState::TimedOut));
}

#[test]
fn waiting_startup_quarantine_uses_radio_opportunities_instead_of_host_elapsed_time() {
    let (registry, probe) = fixture();
    let id = MobileIdentity::Min34(1);
    let mut idle = IdleQueries::after_restart(
        probe.allocation,
        super::super::spach_arq::reuse::ReusePolicy { quiet_ms: 5200 },
    );
    idle.request_message_waiting(&registry, id, waiting_message(), false, 100_000)
        .unwrap();
    let phase = idle.allocation.phase(id).unwrap();
    assert!(idle.frame(6, phase).is_none());
    assert!(idle.frame(8, phase).is_some());
    let set = MessageSet {
        identity: id,
        messages: vec![waiting_confirmation().encode().unwrap()],
    };
    assert_eq!(
        crate::runtime::access::classify(&set).unwrap(),
        crate::runtime::access::Procedure::MessageWaiting
    );
    assert_eq!(
        crate::runtime::access::classify(&MessageSet {
            identity: id,
            messages: vec![vec![15, 0x68]]
        })
        .unwrap(),
        crate::runtime::access::Procedure::Sms
    );
}

#[test]
fn waiting_confirmation_after_all_submissions_can_precede_host_receipts_without_reviving_cancelled_work()
 {
    let (registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    idle.request_message_waiting(&registry, id, waiting_message(), false, 0)
        .unwrap();
    let mut completions = Vec::new();
    for sf in 2..5 {
        for phase in 0..32 {
            if idle.frame(sf, phase).is_some() {
                if let Some(completion) = idle.completion() {
                    completions.push(completion);
                }
                idle.submitted().unwrap();
                if completions.len() == 1 {
                    assert!(
                        !idle
                            .received_indication(id, &waiting_confirmation(), 1)
                            .unwrap()
                    );
                }
            }
        }
    }
    assert!(
        idle.received_indication(id, &waiting_confirmation(), 2)
            .unwrap()
    );
    for completion in completions {
        idle.transmitted(&completion, 3).unwrap();
    }
    assert_eq!(idle.message_waiting_state(id), Some(PollState::Received));
    assert!(
        !idle
            .received_indication(id, &waiting_confirmation(), 4)
            .unwrap()
    );
}

#[test]
fn user_alert_confirmation_is_separate_from_message_waiting_and_retains_required_copy() {
    let (registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    let phase = idle.allocation.phase(id).unwrap();
    idle.request_user_alert(
        &registry,
        id,
        crate::l3::user_alert::UserAlert::default(),
        false,
        0,
    )
    .unwrap();
    let frame = *idle.frame(2, phase).unwrap();
    let old = idle.completion().unwrap();
    idle.submitted().unwrap();
    assert!(
        !idle
            .received_indication(id, &waiting_confirmation(), 1)
            .unwrap()
    );
    assert_eq!(idle.user_alert_state(id), Some(PollState::InFlight));
    let confirmation = crate::l3::notification::SpachConfirmation {
        confirmed_type: crate::l3::user_alert::MESSAGE_TYPE,
        ..Default::default()
    };
    assert!(idle.received_indication(id, &confirmation, 2).unwrap());
    idle.transmitted(&old, 3).unwrap();
    assert_eq!(idle.user_alert_state(id), Some(PollState::Received));
    assert_eq!(idle.frame(3, phase), Some(&frame));
    idle.submitted().unwrap();
    assert_eq!(
        idle.request_user_alert(
            &registry,
            id,
            crate::l3::user_alert::UserAlert::default(),
            false,
            4
        ),
        Err(PollError::Busy)
    );
    let message = crate::l3::message_waiting::MessageWaiting {
        entries: vec![crate::l3::message_waiting::Waiting { kind: 1, count: 0 }],
        ..Default::default()
    };
    idle.request_message_waiting(&registry, id, message, false, 5)
        .unwrap();
    idle.frame(4, phase).unwrap();
    idle.submitted().unwrap();
    idle.transmitted(&old, 6).unwrap();
    assert!(!idle.received_indication(id, &confirmation, 7).unwrap());
    assert_eq!(idle.message_waiting_state(id), Some(PollState::InFlight));
    assert!(
        idle.received_indication(id, &waiting_confirmation(), 8)
            .unwrap()
    );
    let set = MessageSet {
        identity: id,
        messages: vec![vec![15, 0x94]],
    };
    assert_eq!(
        crate::runtime::access::classify(&set).unwrap(),
        crate::runtime::access::Procedure::UserAlert
    );
}
#[test]
fn user_alert_requires_registered_idle_public_scope_and_preserves_queue_deadline() {
    let (registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    let alert = crate::l3::user_alert::UserAlert::default();
    assert_eq!(
        idle.request_user_alert(&registry, id, alert.clone(), true, 0),
        Err(PollError::Busy)
    );
    let offline = LocalRegistry::try_new(vec![ProvisionedSubscriber {
        identity: id,
        number: "1001".into(),
    }])
    .unwrap();
    assert_eq!(
        idle.request_user_alert(&offline, id, alert.clone(), false, 0),
        Err(PollError::Offline)
    );
    let mut multilingual = alert.clone();
    multilingual.multilingual_display = Some(crate::l3::audit::MultilingualDisplay {
        encoding: 4,
        bytes: vec![0, 0x41],
    });
    assert_eq!(
        idle.request_user_alert(&registry, id, multilingual, false, 0),
        Err(PollError::Framing)
    );
    idle.request_user_alert(&registry, id, alert.clone(), false, 1)
        .unwrap();
    idle.request_user_alert(&registry, id, alert.clone(), false, 2)
        .unwrap();
    idle.advance(&registry, |_| false, super::super::timers::SPACH_MS + 1)
        .unwrap();
    assert_eq!(idle.user_alert_state(id), Some(PollState::TimedOut));
    idle.request_user_alert(
        &registry,
        id,
        alert,
        false,
        super::super::timers::SPACH_MS + 2,
    )
    .unwrap();
    let empty = LocalRegistry::try_new(vec![]).unwrap();
    idle.advance(&empty, |_| false, super::super::timers::SPACH_MS + 3)
        .unwrap();
    assert_eq!(idle.user_alert_state(id), Some(PollState::Cancelled));
}

#[test]
fn identical_registration_in_one_superframe_cancels_old_query_and_its_callback() {
    let (mut registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    let snapshot = registry.subscribers()[0].registration;
    idle.request(&registry, id, false, 0).unwrap();
    let phase = idle.allocation.phase(id).unwrap();
    idle.frame(0, phase).unwrap();
    let old = idle.completion().unwrap();
    idle.submitted().unwrap();
    registry
        .register(
            &MessageSet {
                identity: id,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            0,
        )
        .unwrap();
    assert_eq!(registry.subscribers()[0].registration, snapshot);
    idle.registration_received(&registry, id, RegistrationType::PowerUp, false, 1)
        .unwrap();
    assert_eq!(idle.state(id), Some(PollState::Cancelled));
    idle.request(&registry, id, false, 2).unwrap();
    idle.transmitted(&old, 3).unwrap();
    idle.received(id, 4).unwrap();
    assert_eq!(idle.state(id), Some(PollState::Queued));
}

#[test]
fn offline_audit_cannot_survive_subscriber_deletion_and_identical_recreation() {
    let (mut registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    registry
        .register(
            &MessageSet {
                identity: id,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerDown,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            0,
        )
        .unwrap();
    audit_request(&mut idle, &registry, false, 0);
    registry
        .replace_provisioning(vec![ProvisionedSubscriber {
            identity: id,
            number: "1001".into(),
        }])
        .unwrap();
    idle.advance(&registry, |_| false, 2).unwrap();
    assert!(matches!(
        idle.audit_state(id),
        Some(PollState::AwaitingReport { .. })
    ));
    registry.replace_provisioning(vec![]).unwrap();
    registry
        .replace_provisioning(vec![ProvisionedSubscriber {
            identity: id,
            number: "1001".into(),
        }])
        .unwrap();
    idle.advance(&registry, |_| false, 3).unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Cancelled));
}

#[test]
fn intervening_registration_cannot_be_hidden_by_a_later_forced_response() {
    let (mut registry, mut idle) = fixture();
    let id = MobileIdentity::Min34(1);
    audit_request(&mut idle, &registry, true, 0);
    registry
        .register(
            &MessageSet {
                identity: id,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            0,
        )
        .unwrap();
    idle.registration_received(&registry, id, RegistrationType::PowerUp, false, 2)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Cancelled));
    forced_registration(&mut registry, 0);
    idle.registration_received(&registry, id, RegistrationType::Forced, false, 3)
        .unwrap();
    idle.received_audit(id, crate::l3::audit::AuditConfirmation::default(), 4)
        .unwrap();
    assert_eq!(idle.audit_state(id), Some(PollState::Cancelled));
    assert_eq!(idle.forced_registration_received(id), Some(false));
}

#[test]
fn automatic_registration_waits_for_idle_and_deduplicates_unknown_callers() {
    let (registry, mut idle) = fixture();
    let identity = MobileIdentity::Min34(3410151054);
    idle.registration_needed(&registry, identity);
    idle.registration_needed(&registry, identity);
    assert_eq!(idle.registration_requests.len(), 1);
    idle.solicit_registrations(&registry, |candidate| candidate == identity, 0)
        .unwrap();
    assert_eq!(idle.audit_state(identity), None);
    idle.solicit_registrations(&registry, |_| false, 1).unwrap();
    assert!(idle.registration_requests.is_empty());
    assert_eq!(idle.audit_state(identity), Some(PollState::Queued));
    let poll = idle.polls.back().unwrap();
    let order = crate::l3::audit::AuditOrder::decode(&poll.completion.bytes).unwrap();
    assert!(order.force_registration);
    assert!(!order.debug_display_allowed);
    idle.advance(&registry, |_| false, 2).unwrap();
    assert_eq!(idle.audit_state(identity), Some(PollState::Queued));
    assert!(registry.by_identity(identity).is_none());
}

#[test]
fn automatic_registration_skips_registered_and_cancels_when_registration_arrives() {
    let (registry, mut idle) = fixture();
    let identity = MobileIdentity::Min34(1);
    idle.registration_needed(&registry, identity);
    assert!(idle.registration_requests.is_empty());
    let mut offline = LocalRegistry::try_new(vec![ProvisionedSubscriber {
        identity,
        number: "1001".into(),
    }])
    .unwrap();
    idle.registration_needed(&offline, identity);
    idle.solicit_registrations(&offline, |_| true, 0).unwrap();
    offline
        .register(
            &MessageSet {
                identity,
                messages: vec![
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: ProtocolVersion::Is136RevA,
                    }
                    .encode()
                    .unwrap(),
                ],
            },
            0,
        )
        .unwrap();
    idle.solicit_registrations(&offline, |_| false, 1).unwrap();
    assert!(idle.registration_requests.is_empty());
    assert_eq!(idle.audit_state(identity), None);
}
