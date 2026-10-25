use super::*;
use crate::l3::{
    audit::MultilingualDisplay,
    broadcast::ProtocolVersion,
    call_control::{ExtendedCause, Response, ResponseKind},
    legacy_traffic::Subaddress,
    registration::RejectCause,
    user_alert::Signal,
};

#[test]
fn independent_registration_reject_vectors_keep_field_order_and_timer_units() {
    let wire = [0x1e, 0xf1, 0x24, 0x80, 0x14, 0x13, 0x01];
    let response = Response::decode(&wire).unwrap();
    assert_eq!(response.kind, ResponseKind::RegistrationReject);
    assert_eq!(response.cause, RejectCause::Extension);
    assert_eq!(
        response.options.reject_time,
        Some(RejectTime::try_new(2, 4).unwrap())
    );
    assert_eq!(response.options.display.as_deref(), Some("A"));
    assert_eq!(
        response.options.cause_extension,
        Some(ExtendedCause::DataPrivacyNotAvailable)
    );
    assert_eq!(response.encode().unwrap(), wire);
    assert!(
        response
            .validate_version(ProtocolVersion::Is136RevA)
            .is_err()
    );
    response.validate_version(ProtocolVersion::Pv1).unwrap();
    assert_eq!(
        Response::decode(&[0x1e, 0x10]).unwrap(),
        Response::registration_reject(RejectCause::Congestion)
    );
    assert!(
        Response::decode(&[0x1e, 0x11, 0xf2])
            .unwrap()
            .options
            .reject_time
            .unwrap()
            .until_power_down()
    );
}

#[test]
fn all_reject_time_boundaries_and_random_choices_follow_the_specification() {
    for lower in 0..=15 {
        for upper in 0..=15 {
            let result = RejectTime::try_new(lower, upper);
            if lower != 15 && upper != 15 && lower > upper {
                assert!(result.is_err());
                continue;
            }
            let time = result.unwrap();
            let mut response = Response::registration_reject(RejectCause::Congestion);
            response.options.reject_time = Some(time);
            assert_eq!(
                Response::decode(&response.encode().unwrap()).unwrap(),
                response
            );
            for random in 1..=10 {
                let expected = if lower == 15 || upper == 15 {
                    RegistrationRetry::UntilPowerDown
                } else {
                    RegistrationRetry::Superframes(
                        u16::from(lower) * 100 + u16::from(upper - lower) * 10 * u16::from(random),
                    )
                };
                assert_eq!(time.retry_after(random).unwrap(), expected);
            }
            assert!(time.retry_after(0).is_err());
            assert!(time.retry_after(11).is_err());
        }
    }
    assert!(RejectTime::try_new(16, 0).is_err());
    assert!(RejectTime::try_new(0, 16).is_err());
    assert_eq!(DEFAULT_REJECT_SUPERFRAMES, 2048);
    assert_eq!(
        Response::registration_reject(RejectCause::Congestion)
            .registration_retry(1)
            .unwrap(),
        RegistrationRetry::Superframes(2048)
    );
    assert!(
        Response::release(RejectCause::Congestion)
            .registration_retry(1)
            .is_err()
    );
}

#[test]
fn registration_reject_rejects_duplicate_order_truncated_and_invalid_fields() {
    for wire in [
        &[0x1e, 0x11][..],
        &[0x1e, 0x11, 0x42],
        &[0x1e, 0x11, 0x24, 0x12, 0x40],
        &[0x1e, 0x18, 0x01, 0x41, 0x12, 0x40],
        &[0x1e, 0x13, 0x00],
        &[0x1e, 0xf3, 0x02],
    ] {
        assert!(Response::decode(wire).is_err(), "{wire:02x?}");
    }
    let mut response = Response::registration_reject(RejectCause::Congestion);
    response.options.signal = Some(Signal::default());
    assert!(response.encode().is_err());
    for mut response in [
        Response::release(RejectCause::Congestion),
        Response::reorder(
            RejectCause::Congestion,
            crate::l3::call_control::Tone::Reorder,
        ),
    ] {
        response.options.reject_time = Some(RejectTime::try_new(0, 1).unwrap());
        assert!(response.encode().is_err());
    }
}

#[test]
fn registration_reject_handles_all_shared_options_and_unknown_suffixes() {
    let mut response = Response::registration_reject(RejectCause::Extension);
    response.options.reject_time = Some(RejectTime::try_new(0, 14).unwrap());
    response.options.subaddress = Some(Subaddress {
        kind: 2,
        odd: true,
        address: vec![255; 20],
    });
    response.options.display = Some("A".repeat(82));
    response.options.multilingual_display = Some(MultilingualDisplay {
        encoding: 4,
        bytes: vec![0x03, 0xa9],
    });
    response.options.cause_extension = Some(ExtendedCause::DataPrivacyNotSupported);
    assert_eq!(
        Response::decode(&response.encode().unwrap()).unwrap(),
        response
    );
    response.options.unparsed_bits = vec![false, true, false, false, true, false, true, false];
    let wire = response.encode().unwrap();
    let decoded = Response::decode(&wire).unwrap();
    assert!(
        decoded
            .options
            .unparsed_bits
            .starts_with(&response.options.unparsed_bits)
    );
    assert_eq!(decoded.encode().unwrap(), wire);
    response.options.display = Some("A".repeat(83));
    assert!(response.encode().is_err());
}
