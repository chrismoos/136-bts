use super::*;
use crate::core::messaging::LocalMessaging;
use crate::core::store::StateStore;

const MIN: MobileIdentity = MobileIdentity::Min34(2022510374);
const ESN: u32 = 0x345315df;

fn call() -> MessageSet {
    MessageSet {
        identity: MIN,
        messages: vec![
            vec![0x07, 0x50, 0x18, 0x09, 0x11, 0x01, 0xe6, 0x28, 0x00],
            AccessMessage::SerialNumber { esn: ESN }.encode().unwrap(),
        ],
    }
}

fn core() -> LocalMessaging {
    let mut core = LocalMessaging::from_store(StateStore::open(":memory:").unwrap()).unwrap();
    core.save_subscriber(ProvisionedSubscriber {
        identity: MIN,
        number: "1004".into(),
    })
    .unwrap();
    core
}

#[test]
fn origination_restores_reachability_without_fabricating_registration_options() {
    let mut core = core();
    assert!(
        core.registry()
            .by_number("1004")
            .unwrap()
            .registration
            .is_none()
    );
    assert!(core.observe_call_access(&call(), 10).unwrap());
    let subscriber = core.registry().by_number("1004").unwrap();
    let registration = subscriber.registration.unwrap();
    assert_eq!(registration.esn, Some(ESN));
    assert_eq!(registration.protocol_version, ProtocolVersion::Pv1);
    assert_eq!(
        registration.access_voice_coder_map.unwrap(),
        Codec::AcelpCc1.capability_mask()
    );
    assert!(subscriber.registration_options.is_none());
    assert!(subscriber.capabilities.is_none());
    let owner = core.registry().registration_handle(MIN).unwrap();
    core.observe_call_access(&call(), 20).unwrap();
    assert!(core.registry().registration_current(&owner));
    assert_eq!(
        core.registry()
            .by_number("1004")
            .unwrap()
            .registration
            .unwrap()
            .last_observed_access_superframe
            .unwrap(),
        20
    );
    assert!(core.power_down(&owner));
    assert!(core.registry().registration_handle(MIN).is_none());
}

#[test]
fn malformed_call_and_conflicting_serial_cannot_restore_presence() {
    let mut core = core();
    let mut malformed = call();
    malformed.messages[0].truncate(2);
    assert!(core.observe_call_access(&malformed, 10).is_err());
    let mut duplicate = call();
    duplicate.messages.push(duplicate.messages[1].clone());
    assert!(core.observe_call_access(&duplicate, 10).is_err());
    assert!(core.registry().registration_handle(MIN).is_none());
    core.observe_call_access(&call(), 20).unwrap();
    let previous = core.registry().by_number("1004").unwrap().clone();
    let mut conflict = call();
    conflict.messages[1] = AccessMessage::SerialNumber { esn: ESN + 1 }
        .encode()
        .unwrap();
    assert!(core.observe_call_access(&conflict, 30).is_err());
    assert_eq!(core.registry().by_number("1004").unwrap(), &previous);
}

#[test]
fn observed_presence_expires_but_busy_phone_and_explicit_registration_remain() {
    let mut core = core();
    core.observe_call_access(&call(), 10).unwrap();
    let lifetime = OBSERVED_REACHABILITY_LIFETIME_SUPERFRAMES;
    assert!(
        core.expire_observed_access(10 + lifetime, |_| true)
            .is_empty()
    );
    assert!(
        core.expire_observed_access(10 + lifetime * 2 - 1, |_| false)
            .is_empty()
    );
    assert_eq!(
        core.expire_observed_access(10 + lifetime * 2, |_| false),
        [MIN]
    );
    assert!(core.registry().registration_handle(MIN).is_none());
    let registration = MessageSet {
        identity: MIN,
        messages: vec![
            AccessMessage::Registration {
                registration_type: RegistrationType::PowerUp,
                scm: 15,
                protocol_version: ProtocolVersion::Pv1,
            }
            .encode()
            .unwrap(),
        ],
    };
    core.observe_call_access(&call(), 29).unwrap();
    core.register(&registration, 30).unwrap();
    assert_eq!(
        core.registry()
            .by_number("1004")
            .unwrap()
            .registration
            .unwrap()
            .access_voice_coder_map,
        Some(Codec::AcelpCc1.capability_mask())
    );
    core.observe_call_access(&call(), 40).unwrap();
    assert!(
        core.registry()
            .by_number("1004")
            .unwrap()
            .registration
            .unwrap()
            .last_observed_access_superframe
            .is_none()
    );
    assert!(
        core.expire_observed_access(40 + lifetime * 3, |_| false)
            .is_empty()
    );
}

#[test]
fn esn_bound_subscriber_requires_matching_serial_and_can_learn_identity_on_access() {
    let mut core = LocalMessaging::from_store(StateStore::open(":memory:").unwrap()).unwrap();
    core.save_device(ESN, "1004".into(), None).unwrap();
    assert!(core.observe_call_access(&call(), 10).unwrap());
    assert_eq!(core.devices()[0].identity, Some(MIN));
    let owner = core.registry().registration_handle(MIN).unwrap();
    assert!(core.power_down(&owner));
    let mut missing = call();
    missing.messages.pop();
    assert!(core.observe_call_access(&missing, 20).is_err());
    let mut wrong = call();
    wrong.messages[1] = AccessMessage::SerialNumber { esn: ESN + 1 }
        .encode()
        .unwrap();
    assert!(core.observe_call_access(&wrong, 20).is_err());
    let mut stolen = call();
    stolen.identity = MobileIdentity::Min34(1);
    assert!(core.observe_call_access(&stolen, 20).is_err());
    assert!(core.registry().registration_handle(MIN).is_none());
    assert!(core.observe_call_access(&call(), 30).unwrap());
}

#[test]
fn unknown_call_access_does_not_assign_a_number_or_subscriber() {
    let mut core = LocalMessaging::from_store(StateStore::open(":memory:").unwrap()).unwrap();
    assert!(!core.observe_call_access(&call(), 10).unwrap());
    assert!(core.registry().subscribers().is_empty());
}

#[test]
fn page_response_restores_matching_handset_presence() {
    let mut core = core();
    let mut set = call();
    set.messages[0] = vec![0x08, 0x53, 0xc4, 0x50];
    assert!(core.observe_call_access(&set, 10).unwrap());
    assert!(core.registry().registration_handle(MIN).is_some());
}

#[test]
fn observed_phone_can_be_paged_with_its_reported_codec_and_receive_queued_sms() {
    use crate::core::calls::CallReservations;
    use crate::l3::{
        address::{AddressInfo, AddressValue},
        sms::{Metadata, SmsMessage, UserData},
    };
    use crate::phy::channel::Band;
    use crate::resources::TrafficResources;

    let mut core = core();
    let sender = MobileIdentity::Min34(1);
    core.save_subscriber(ProvisionedSubscriber {
        identity: sender,
        number: "1001".into(),
    })
    .unwrap();
    let destination = AddressInfo {
        number_type: 0,
        numbering_plan: 1,
        address: AddressValue::Tbcd("1004".into()),
    };
    let payload = SmsMessage::Submit {
        metadata: Metadata {
            message_reference: 1,
            privacy: 0,
            urgency: 1,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        user_data: UserData::Ira(b"test".to_vec()),
    }
    .encode()
    .unwrap();
    let sms = core
        .submit_complete_sms(sender, &destination, &payload, "observed-access", 1)
        .unwrap();
    assert!(sms.delivery.is_none());
    assert!(core.deliveries_for(MIN).unwrap().is_empty());
    let mut originating = call();
    originating.identity = sender;
    originating.messages.pop();
    core.observe_call_access(&originating, 10).unwrap();
    let mut calls =
        CallReservations::new(TrafficResources::try_new(Band::Cellular800, 25, &[]).unwrap());
    assert!(matches!(
        calls.reserve(core.registry(), sender, "1004"),
        Err(crate::core::calls::CallError::DestinationOffline)
    ));
    core.observe_call_access(&call(), 11).unwrap();
    let reserved = calls.reserve(core.registry(), sender, "1004").unwrap();
    assert_eq!(reserved.callee.codec, Codec::AcelpCc1);
    assert_eq!(core.deliveries_for(MIN).unwrap().len(), 1);
    assert_eq!(core.deliveries_for(MIN).unwrap()[0].queued.id, sms.id);
}

#[test]
fn invalid_identity_does_not_bind_a_provisioned_esn() {
    let mut core = LocalMessaging::from_store(StateStore::open(":memory:").unwrap()).unwrap();
    core.save_device(ESN, "1004".into(), None).unwrap();
    let mut invalid = call();
    invalid.identity = MobileIdentity::Min34(0);
    assert!(core.observe_call_access(&invalid, 10).is_err());
    assert_eq!(core.devices()[0].identity, None);
    assert!(core.registry().subscribers().is_empty());
}
