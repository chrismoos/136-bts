use super::*;
use crate::core::calls::tests::{call_reservations, registered_registry, request, set};

#[test]
fn unknown_digital_destinations_reserve_only_one_mobile_bearer() {
    let registry = registered_registry(&[1, 2, 3]);
    let mut calls = call_reservations();
    let mut request = request();
    request.called_party.address = AddressValue::Tbcd("5551234".into());
    let OriginationRoute::SampleAudio(first) =
        calls.route_origination(&registry, &set(&request)).unwrap()
    else {
        panic!("unknown number must select sample audio");
    };
    assert_eq!(first.mobile.identity, MobileIdentity::Min34(1));
    assert_eq!(first.direction, AudioDirection::MobileOriginated);
    assert_eq!(first.remote_number, "5551234");
    assert_eq!(first.mobile.codec, Codec::Vselp);
    let second = calls
        .reserve_audio(
            &registry,
            MobileIdentity::Min34(2),
            "1000",
            AudioDirection::MobileTerminated,
            Some(Codec::AcelpCc1),
        )
        .unwrap();
    assert_ne!(first.mobile.assigned_slots, second.mobile.assigned_slots);
    assert_ne!(first.handle.id(), second.handle.id());
    assert!(matches!(
        calls.reserve_audio(
            &registry,
            MobileIdentity::Min34(3),
            "1000",
            AudioDirection::MobileTerminated,
            None
        ),
        Err(CallError::Allocation(AllocationError::Exhausted))
    ));
    calls.release_audio(&first.handle).unwrap();
    assert!(matches!(
        calls.release_audio(&first.handle),
        Err(CallError::StaleHandle)
    ));
    let third = calls
        .reserve_audio(
            &registry,
            MobileIdentity::Min34(3),
            "1000",
            AudioDirection::MobileTerminated,
            None,
        )
        .unwrap();
    assert_eq!(first.mobile.assigned_slots, third.mobile.assigned_slots);
    calls.release_audio(&second.handle).unwrap();
    calls.release_audio(&third.handle).unwrap();
    assert_eq!(calls.audio_reservations().count(), 0);
    let pair = calls
        .reserve(&registry, MobileIdentity::Min34(1), "1002")
        .unwrap();
    calls.release(&pair.handle).unwrap();
}

#[test]
fn audio_and_mobile_calls_share_busy_checks_and_resource_ownership() {
    let registry = registered_registry(&[1, 2, 3]);
    let mut calls = call_reservations();
    let audio = calls
        .reserve_audio(
            &registry,
            MobileIdentity::Min34(1),
            "1234",
            AudioDirection::MobileTerminated,
            None,
        )
        .unwrap();
    assert!(matches!(
        calls.reserve(&registry, MobileIdentity::Min34(1), "1002"),
        Err(CallError::Busy(MobileIdentity::Min34(1)))
    ));
    assert!(matches!(
        calls.reserve(&registry, MobileIdentity::Min34(2), "1001"),
        Err(CallError::Busy(MobileIdentity::Min34(1)))
    ));
    calls.release_audio(&audio.handle).unwrap();
    let pair = calls
        .reserve(&registry, MobileIdentity::Min34(1), "1002")
        .unwrap();
    assert!(matches!(
        calls.reserve_audio(
            &registry,
            MobileIdentity::Min34(2),
            "1234",
            AudioDirection::MobileTerminated,
            None
        ),
        Err(CallError::Busy(MobileIdentity::Min34(2)))
    ));
    calls.release(&pair.handle).unwrap();
}

#[test]
fn known_offline_numbers_and_analog_requests_do_not_fall_back_to_audio() {
    let registry = registered_registry(&[1]);
    let mut calls = call_reservations();
    let mut request = request();
    assert!(matches!(
        calls.route_origination(&registry, &set(&request)),
        Err(CallError::DestinationOffline)
    ));
    request.called_party.address = AddressValue::Tbcd("5551234".into());
    request.service_code = ServiceCode::AnalogSpeech;
    assert!(matches!(
        calls.route_origination(&registry, &set(&request)),
        Err(CallError::Service(ServiceCode::AnalogSpeech))
    ));
    assert_eq!(calls.audio_reservations().count(), 0);
    assert_eq!(calls.reservations().count(), 0);
}

#[test]
fn audio_calls_use_explicit_acelp_requests_and_reject_incompatible_reports() {
    let mut registry = registered_registry(&[1]);
    let mut calls = call_reservations();
    let mut origin =
        crate::l3::access_call::OriginationRequest::decode(&set(&request()).messages[0]).unwrap();
    origin.message.called_party.address = AddressValue::Tbcd("5551234".into());
    origin
        .options
        .voices
        .push(crate::l3::access_call::LegacyVoice {
            coder: 2,
            privacy: 0,
        });
    let input = MessageSet {
        identity: MobileIdentity::Min34(1),
        messages: vec![origin.encode().unwrap()],
    };
    let OriginationRoute::SampleAudio(call) = calls.route_origination(&registry, &input).unwrap()
    else {
        panic!("sample route required")
    };
    assert_eq!(call.mobile.codec, Codec::AcelpCc1);
    calls.release_audio(&call.handle).unwrap();
    let report = crate::l3::capability_report::decode(&[
        0x05, 0x45, 0x80, 0x02, 0xaf, 0x03, 0x1f, 0xe8, 0x10, 0x40,
    ])
    .unwrap();
    registry
        .record_capabilities(input.identity, report, 0)
        .unwrap();
    assert!(matches!(
        calls.reserve_audio(
            &registry,
            input.identity,
            "1234",
            AudioDirection::MobileTerminated,
            Some(Codec::Vselp)
        ),
        Err(CallError::Admission(AdmissionError::Codec))
    ));
    let incoming = calls
        .reserve_audio(
            &registry,
            input.identity,
            "1234",
            AudioDirection::MobileTerminated,
            None,
        )
        .unwrap();
    assert_eq!(incoming.mobile.codec, Codec::AcelpCc1);
}

#[test]
fn unprovisioned_originators_get_audio_without_creating_subscribers() {
    let registry = LocalRegistry::try_new(vec![]).unwrap();
    let identity = MobileIdentity::Min34(3410151054);
    let mut request = request();
    request.called_party.address = AddressValue::Tbcd("2222".into());
    let mut input = set(&request);
    input.identity = identity;
    let mut calls = call_reservations();
    let OriginationRoute::SampleAudio(call) = calls.route_origination(&registry, &input).unwrap()
    else {
        panic!("audio route required");
    };
    assert_eq!(call.mobile.identity, identity);
    assert_eq!(call.mobile.number, "3143781765");
    assert_eq!(call.mobile.protocol_version, Some(request.protocol_version));
    assert_eq!(call.mobile.codec, Codec::Vselp);
    assert!(registry.subscribers().is_empty());
    assert!(
        matches!(calls.route_origination(&registry, &input), Err(CallError::Busy(value)) if value == identity)
    );
    calls.release_audio(&call.handle).unwrap();
}

#[test]
fn unprovisioned_originators_can_call_registered_destinations() {
    let registry = registered_registry(&[2]);
    let mut input = set(&request());
    input.identity = MobileIdentity::Min34(3410151054);
    let mut calls = call_reservations();
    let OriginationRoute::Mobile(call) = calls.route_origination(&registry, &input).unwrap() else {
        panic!("mobile route required");
    };
    assert_eq!(call.caller.number, "3143781765");
    assert_eq!(call.callee.number, "1002");
    assert_eq!(
        call.caller.protocol_version,
        Some(request().protocol_version)
    );
    assert!(registry.by_identity(input.identity).is_none());
    calls.release(&call.handle).unwrap();
}

#[test]
fn provisioned_originators_need_no_prior_registration_for_access_calls() {
    let registry = registered_registry(&[2]);
    let mut calls = call_reservations();
    let OriginationRoute::Mobile(call) = calls
        .route_origination(&registry, &set(&request()))
        .unwrap()
    else {
        panic!("mobile route required");
    };
    assert_eq!(call.caller.number, "1001");
    calls.release(&call.handle).unwrap();
}
