use super::*;
use crate::l3::{
    broadcast::ProtocolVersion,
    registration::{AccessMessage, RegistrationType},
};

const ESN: u32 = 0x5ec4dbb7;
const MIN: MobileIdentity = MobileIdentity::Min34(3410151054);

fn core() -> LocalMessaging {
    LocalMessaging::from_store(StateStore::open(":memory:").unwrap()).unwrap()
}
fn registration(identity: MobileIdentity, esn: Option<u32>) -> MessageSet {
    let mut messages = vec![
        AccessMessage::Registration {
            registration_type: RegistrationType::PowerUp,
            scm: 11,
            protocol_version: ProtocolVersion::Is136RevA,
        }
        .encode()
        .unwrap(),
    ];
    if let Some(esn) = esn {
        messages.push(AccessMessage::SerialNumber { esn }.encode().unwrap());
    }
    MessageSet { identity, messages }
}

#[test]
fn esn_only_provisioning_learns_min_and_survives_restart() {
    let mut core = core();
    core.save_device(ESN, "1500".into(), None).unwrap();
    assert!(core.registry.subscribers().is_empty());
    assert_eq!(core.devices()[0].identity, None);
    assert_eq!(
        core.register(&registration(MIN, Some(ESN)), 1)
            .unwrap()
            .reply
            .message,
        RegistrationResponse::Accept
    );
    assert_eq!(
        core.registry
            .by_number("1500")
            .unwrap()
            .provisioning
            .identity,
        MIN
    );
    let mut restarted = LocalMessaging::from_store(core.store).unwrap();
    assert_eq!(restarted.devices()[0].identity, Some(MIN));
    assert!(
        restarted
            .registry
            .by_number("1500")
            .unwrap()
            .registration
            .is_none()
    );
    assert_eq!(
        restarted
            .register(&registration(MIN, Some(ESN)), 2)
            .unwrap()
            .reply
            .message,
        RegistrationResponse::Accept
    );
}

#[test]
fn bound_device_requires_matching_esn_and_rejects_another_mobile_identity() {
    let mut core = core();
    core.save_device(ESN, "1500".into(), None).unwrap();
    core.register(&registration(MIN, Some(ESN)), 1).unwrap();
    for request in [
        registration(MIN, None),
        registration(MIN, Some(ESN + 1)),
        registration(MobileIdentity::Min34(7), Some(ESN)),
    ] {
        assert!(matches!(
            core.register(&request, 2).unwrap().reply.message,
            RegistrationResponse::Reject { .. }
        ));
    }
    assert_eq!(
        core.registry
            .by_number("1500")
            .unwrap()
            .provisioning
            .identity,
        MIN
    );
    assert_eq!(core.devices()[0].identity, Some(MIN));
}

#[test]
fn malformed_or_unsupported_access_cannot_create_a_binding() {
    let mut core = core();
    core.save_device(ESN, "1500".into(), None).unwrap();
    let mut malformed = registration(MIN, Some(ESN));
    malformed.messages.reverse();
    assert!(core.register(&malformed, 1).is_err());
    assert!(core.devices()[0].identity.is_none());
    assert!(core.store.enabled_subscribers().unwrap().is_empty());
}

#[test]
fn esn_cannot_claim_existing_identity_or_number() {
    let mut core = core();
    core.save_subscriber(ProvisionedSubscriber {
        identity: MIN,
        number: "1001".into(),
    })
    .unwrap();
    assert!(core.save_device(ESN, "1001".into(), None).is_err());
    core.save_device(ESN, "1500".into(), None).unwrap();
    assert!(matches!(
        core.register(&registration(MIN, Some(ESN)), 1)
            .unwrap()
            .reply
            .message,
        RegistrationResponse::Reject { .. }
    ));
    assert_eq!(
        core.registry
            .by_number("1001")
            .unwrap()
            .provisioning
            .identity,
        MIN
    );
    assert!(core.devices()[0].identity.is_none());
}

#[test]
fn explicit_legacy_conversion_retains_identity_and_requires_reported_esn() {
    let mut core = core();
    core.save_subscriber(ProvisionedSubscriber {
        identity: MIN,
        number: "1500".into(),
    })
    .unwrap();
    core.register(&registration(MIN, Some(ESN)), 1).unwrap();
    assert!(core.save_device(ESN + 1, "1500".into(), Some(MIN)).is_err());
    core.save_device(ESN, "1500".into(), Some(MIN)).unwrap();
    assert_eq!(
        core.registry
            .by_number("1500")
            .unwrap()
            .registration
            .unwrap()
            .esn,
        Some(ESN)
    );
    core.save_device(ESN, "1600".into(), None).unwrap();
    assert_eq!(
        core.registry
            .by_number("1600")
            .unwrap()
            .provisioning
            .identity,
        MIN
    );
    assert!(core.save_device(ESN + 1, "1700".into(), Some(MIN)).is_err());
    core.delete_device(ESN).unwrap();
    assert!(core.devices().is_empty());
    assert!(core.registry.subscribers().is_empty());
}

#[test]
fn pending_devices_are_editable_removable_and_reserve_numbers() {
    let mut core = core();
    core.save_device(ESN, "1500".into(), None).unwrap();
    assert!(
        core.save_subscriber(ProvisionedSubscriber {
            identity: MIN,
            number: "1500".into()
        })
        .is_err()
    );
    assert!(core.save_device(ESN + 1, "1500".into(), None).is_err());
    assert!(core.save_device(0, "1600".into(), None).is_err());
    core.save_device(ESN, "1600".into(), None).unwrap();
    assert_eq!(core.devices()[0].number, "1600");
    core.delete_device(ESN).unwrap();
    assert!(core.devices().is_empty());
}

#[test]
fn converting_a_legacy_subscriber_preserves_queued_sms() {
    use crate::l3::sms::{DisplayTime, Metadata, NORMAL_URGENCY, SmsMessage, UserData, Validity};
    let mut core = core();
    core.save_subscriber(ProvisionedSubscriber {
        identity: MIN,
        number: "1500".into(),
    })
    .unwrap();
    let other = MobileIdentity::Min34(7);
    core.save_subscriber(ProvisionedSubscriber {
        identity: other,
        number: "1600".into(),
    })
    .unwrap();
    let message = SmsMessage::Deliver {
        metadata: Metadata {
            message_reference: 0,
            privacy: 0,
            urgency: NORMAL_URGENCY,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        validity: Validity::Indefinite,
        display_time: DisplayTime::Invoke,
        user_data: UserData::Ira(b"hello".to_vec()),
    };
    let id = core
        .store
        .enqueue(NewSms {
            dedup_key: "before-esn-conversion",
            sender: other,
            recipient: MIN,
            message: &message,
            created_at_unix_ms: 1,
        })
        .unwrap();
    core.save_device(ESN, "1500".into(), Some(MIN)).unwrap();
    let queued = core.store.pending_for(MIN).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, id);
    assert_eq!(queued[0].sms_pdu, message.encode().unwrap());
    let restarted = LocalMessaging::from_store(core.store).unwrap();
    assert_eq!(restarted.store.pending_for(MIN).unwrap()[0].id, id);
    assert_eq!(restarted.devices()[0].identity, Some(MIN));
}

#[test]
fn unknown_registration_can_be_provisioned_by_esn_without_registering_again() {
    let mut core = core();
    core.set_allow_unknown_registration(true);
    let dispatch = core.register(&registration(MIN, Some(ESN)), 7).unwrap();
    assert_eq!(dispatch.reply.message, RegistrationResponse::Accept);
    assert!(dispatch.pending.is_empty());
    assert!(core.registry.subscribers().is_empty());
    assert!(core.registry.by_identity(MIN).is_none());
    assert!(core.store.enabled_subscribers().unwrap().is_empty());
    assert!(core.devices().is_empty());
    assert!(core.registry.registration_handle(MIN).is_none());
    core.save_device(ESN, "1500".into(), None).unwrap();
    let phone = core.registry.by_number("1500").unwrap();
    assert_eq!(phone.provisioning.identity, MIN);
    assert_eq!(phone.registration.unwrap().last_registration_superframe, 7);
    assert_eq!(phone.registration.unwrap().esn, Some(ESN));
    assert_eq!(core.devices()[0].identity, Some(MIN));
    assert!(core.registry.unprovisioned_phone(MIN).is_none());
    assert!(core.registry.registration_handle(MIN).is_some());
}

#[test]
fn unknown_registration_can_be_disabled_without_blocking_provisioned_phones() {
    let mut core = core();
    core.set_allow_unknown_registration(true);
    core.save_subscriber(ProvisionedSubscriber {
        identity: MIN,
        number: "1500".into(),
    })
    .unwrap();
    core.set_allow_unknown_registration(false);
    assert_eq!(
        core.register(&registration(MIN, Some(ESN)), 1)
            .unwrap()
            .reply
            .message,
        RegistrationResponse::Accept
    );
    let unknown = MobileIdentity::Min34(7);
    assert_eq!(
        core.register(&registration(unknown, Some(ESN + 1)), 2)
            .unwrap()
            .reply
            .message,
        RegistrationResponse::Reject {
            cause: RejectCause::UnknownMsid
        }
    );
    assert!(core.registry.unprovisioned_phone(unknown).is_none());
}

#[test]
fn open_registration_still_rejects_malformed_requests_and_esn_conflicts() {
    let mut core = core();
    core.set_allow_unknown_registration(true);
    let mut malformed = registration(MIN, Some(ESN));
    malformed.messages.push(malformed.messages[0].clone());
    assert!(core.register(&malformed, 1).is_err());
    assert!(core.registry.unprovisioned_phone(MIN).is_none());
    core.save_device(ESN, "1500".into(), None).unwrap();
    core.register(&registration(MIN, Some(ESN)), 2).unwrap();
    assert_eq!(
        core.register(&registration(MIN, Some(ESN + 1)), 3)
            .unwrap()
            .reply
            .message,
        RegistrationResponse::Reject {
            cause: RejectCause::UnknownMsid
        }
    );
}

#[test]
fn unknown_registration_can_be_provisioned_by_identity_without_losing_state() {
    let mut core = core();
    core.set_allow_unknown_registration(true);
    core.register(&registration(MIN, Some(ESN)), 7).unwrap();
    core.save_subscriber(ProvisionedSubscriber {
        identity: MIN,
        number: "1500".into(),
    })
    .unwrap();
    assert!(
        core.registry
            .by_number("1500")
            .unwrap()
            .registration
            .is_some()
    );
    let other = MobileIdentity::Min34(7);
    assert_eq!(
        core.register(&registration(other, Some(ESN + 1)), 8)
            .unwrap()
            .reply
            .message,
        RegistrationResponse::Accept
    );
    assert!(core.registry.by_identity(other).is_none());
}
