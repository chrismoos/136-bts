use super::*;
use crate::l3::{
    audit::AuditConfirmation,
    maca::{Measurements, ReportMeasurements},
    registration::AccessMessage,
};
use crate::link::rach::MobileIdentity;
fn primary(kind: RegistrationType) -> Vec<u8> {
    AccessMessage::Registration {
        registration_type: kind,
        scm: 15,
        protocol_version: ProtocolVersion::Pv2,
    }
    .encode()
    .unwrap()
}
fn report() -> Vec<u8> {
    MacaReport {
        measurements: ReportMeasurements::Extended(Measurements {
            current: Some(vec![20]),
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode()
    .unwrap()
}
fn set(messages: Vec<Vec<u8>>) -> MessageSet {
    MessageSet {
        identity: MobileIdentity::Min34(1),
        messages,
    }
}
#[test]
fn registration_companion_preserves_primary_and_secondary_order() {
    let main = primary(RegistrationType::PowerUp);
    let serial = AccessMessage::SerialNumber { esn: 1 }.encode().unwrap();
    for messages in [
        vec![main.clone(), report(), serial.clone()],
        vec![main.clone(), serial.clone(), report()],
    ] {
        let original = set(messages);
        let parsed = parse(&original).unwrap();
        assert_eq!(parsed.procedure, Procedure::Registration);
        assert_eq!(parsed.primary.messages, vec![main.clone(), serial.clone()]);
        let maca = parsed.maca.unwrap();
        assert_eq!(
            maca.source,
            MacaSource::Broadcast(AccessTrigger::Registration)
        );
        assert_eq!(maca.version, Some(ProtocolVersion::Pv2));
        assert_eq!(
            maca.report.measurements.format(),
            Some(crate::l3::maca::MeasurementFormat::Extended)
        );
    }
}
#[test]
fn malformed_repeated_leading_and_power_down_reports_reject() {
    let main = primary(RegistrationType::PowerUp);
    for messages in [
        vec![report()],
        vec![report(), main.clone()],
        vec![main.clone(), report(), report()],
        vec![main, vec![6, 255]],
        vec![primary(RegistrationType::PowerDown), report()],
    ] {
        assert!(parse(&set(messages)).is_err());
    }
}
#[test]
fn relay_messages_and_other_notification_confirmations_cannot_carry_maca() {
    for main in [
        vec![9],
        vec![10, 1],
        vec![11, 1, 44],
        vec![15, 0x68],
        vec![15, 0x5c],
        vec![15, 0x94],
    ] {
        assert!(parse(&set(vec![main, report()])).is_err());
    }
}
#[test]
fn unaccompanied_access_keeps_its_borrowed_message_set() {
    let original = set(vec![primary(RegistrationType::PowerUp)]);
    let parsed = parse(&original).unwrap();
    assert!(matches!(parsed.primary, Cow::Borrowed(_)));
    assert!(parsed.maca.is_none());
}
#[test]
fn queued_confirmation_requires_a_report_and_retains_separate_context() {
    let confirmation = crate::l3::notification::SpachConfirmation {
        confirmed_type: crate::l3::notification::MACA_TYPE,
        ..Default::default()
    }
    .encode()
    .unwrap();
    assert!(parse(&set(vec![confirmation.clone()])).is_err());
    let original = set(vec![confirmation, report()]);
    let parsed = parse(&original).unwrap();
    assert_eq!(parsed.procedure, Procedure::QueuedMaca);
    assert_eq!(parsed.maca.unwrap().source, MacaSource::QueueNotification);
}
#[test]
fn extended_reports_require_a_capable_primary_protocol_version() {
    let old = AccessMessage::Registration {
        registration_type: RegistrationType::PowerUp,
        scm: 15,
        protocol_version: ProtocolVersion::Pv1,
    }
    .encode()
    .unwrap();
    assert!(parse(&set(vec![old, report()])).is_err());
}
#[test]
fn audit_reports_remain_associated_with_their_primary_confirmation() {
    let confirmation = AuditConfirmation::default().encode().unwrap();
    let original = set(vec![confirmation.clone(), report()]);
    let parsed = parse(&original).unwrap();
    assert_eq!(parsed.primary.messages, vec![confirmation]);
    assert_eq!(
        parsed.maca.unwrap().source,
        MacaSource::Broadcast(AccessTrigger::Audit)
    );
}

#[test]
fn call_access_reports_retain_the_primary_trigger_and_version() {
    use crate::l3::{
        address::{AddressInfo, AddressValue},
        origination::Origination,
        page_response::{PageResponse, ServiceCode},
    };
    let origination = Origination {
        protocol_version: ProtocolVersion::Pv2,
        emergency_call: false,
        called_party: AddressInfo {
            number_type: 0,
            numbering_plan: 1,
            address: AddressValue::Tbcd("1002".into()),
        },
        last_try: false,
        scm: 15,
        service_code: ServiceCode::DigitalSpeech,
    }
    .encode()
    .unwrap();
    let response = PageResponse {
        protocol_version: ProtocolVersion::Pv2,
        last_try: false,
        scm: 15,
        service_code: ServiceCode::DigitalSpeech,
    }
    .encode()
    .unwrap();
    for (primary, procedure, trigger) in [
        (
            origination,
            Procedure::Origination,
            AccessTrigger::Origination,
        ),
        (
            response,
            Procedure::PageResponse,
            AccessTrigger::PageResponse,
        ),
    ] {
        let original = set(vec![primary.clone(), report()]);
        let parsed = parse(&original).unwrap();
        assert_eq!(parsed.procedure, procedure);
        assert_eq!(parsed.primary.messages, vec![primary]);
        let companion = parsed.maca.unwrap();
        assert_eq!(companion.source, MacaSource::Broadcast(trigger));
        assert_eq!(companion.version, Some(ProtocolVersion::Pv2));
    }
}

#[test]
fn admitted_registration_keeps_serial_and_rejected_power_down_keeps_owner() {
    use crate::{
        core::{
            call_paging::CallPaging,
            registry::{LocalRegistry, ProvisionedSubscriber},
        },
        l3::designation::AssignedSlots,
        phy::channel::Band,
        resources::TrafficResources,
    };
    let original = set(vec![
        primary(RegistrationType::PowerUp),
        report(),
        AccessMessage::SerialNumber { esn: 42 }.encode().unwrap(),
    ]);
    let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
        identity: original.identity,
        number: "1001".into(),
    }])
    .unwrap();
    let mut calls = CallPaging::new(
        TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
    );
    assert!(matches!(
        super::super::dispatch(&original, &mut registry, &mut calls, 1, 1, 500).unwrap(),
        super::super::Dispatch::Registration(_)
    ));
    let owner = registry.registration_handle(original.identity).unwrap();
    let before = registry.subscribers()[0].clone();
    assert_eq!(before.reported_serial.as_ref().unwrap().esn, 42);
    let invalid = set(vec![primary(RegistrationType::PowerDown), report()]);
    assert!(super::super::dispatch(&invalid, &mut registry, &mut calls, 2, 2, 500).is_err());
    assert!(registry.registration_current(&owner));
    assert_eq!(registry.subscribers()[0], before);
    assert_eq!(calls.requests().count(), 0);
    assert_eq!(calls.reservations().count(), 0);
}

#[test]
fn nokia_combines_audit_confirmation_forced_registration_and_capabilities() {
    let set = crate::link::rach::MessageSet {
        identity: crate::link::rach::MobileIdentity::Min34(3410151054),
        messages: vec![
            vec![0x01, 0x00],
            vec![0x0c, 0x35, 0xa0],
            vec![0x05, 0x45, 0x80, 0x02, 0xaf, 0x03, 0x1f, 0xe8, 0x10, 0x40],
        ],
    };
    let parsed = super::parse(&set).unwrap();
    assert_eq!(parsed.procedure, super::Procedure::Registration);
    assert!(parsed.audit.is_some());
    assert_eq!(parsed.primary.messages, set.messages[1..]);
    assert!(parsed.maca.is_none());
    let mut duplicate = set.clone();
    duplicate.messages.insert(1, vec![0x01, 0x00]);
    assert!(super::parse(&duplicate).is_err());
    let mut invalid = set;
    invalid.messages[1] = vec![0x07, 0x40, 0x18, 0x09, 0x11, 0x11, 0x60];
    assert!(super::parse(&invalid).is_err());
}
