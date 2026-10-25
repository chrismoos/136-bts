use super::*;
use crate::l3::{
    audit::MultilingualDisplay,
    broadcast::ProtocolVersion,
    legacy_traffic::Subaddress,
    user_alert::{Pitch, Signal},
};
#[test]
fn mandatory_and_extended_causes_and_tones_match_independent_vectors() {
    assert_eq!(digital_page(), [0x18, 0x10]);
    for code in 0..16 {
        let cause = RejectCause::decode(code).unwrap();
        for response in [
            Response::release(cause),
            Response::reorder(cause, Tone::Reorder),
        ] {
            let wire = if response.kind == ResponseKind::Release {
                release(cause)
            } else {
                reorder(cause)
            };
            assert_eq!(
                wire,
                [
                    if response.kind == ResponseKind::Release {
                        0x1f
                    } else {
                        0x20
                    },
                    code << 4
                ]
            );
            assert_eq!(response.encode().unwrap(), wire);
            assert_eq!(decode_response(&wire).unwrap(), response);
        }
    }
    let mut release = Response::release(RejectCause::Congestion);
    release.options.signal = Some(Signal::default());
    let mut extended = Response::release(RejectCause::Extension);
    extended.options.cause_extension = Some(ExtendedCause::DataPrivacyNotAvailable);
    let mut reorder = Response::reorder(RejectCause::Extension, Tone::Intercept);
    reorder.options.cause_extension = Some(ExtendedCause::DataPrivacyNotAvailable);
    let mut display_reorder = Response::reorder(RejectCause::Extension, Tone::Intercept);
    display_reorder.options.display = Some("A".into());
    display_reorder.options.cause_extension = Some(ExtendedCause::DataPrivacyNotSupported);
    for (response, wire) in [
        (
            Response::reorder(RejectCause::ServiceCodeNotSupported, Tone::Intercept),
            vec![0x20, 0x74],
        ),
        (release, vec![0x1f, 0x11, 0x01, 0xf0]),
        (extended, vec![0x1f, 0xf3, 0x01]),
        (reorder, vec![0x20, 0xf4, 0x80, 0x40]),
        (display_reorder, vec![0x20, 0xf6, 0x00, 0x50, 0x48, 0x00]),
    ] {
        assert_eq!(response.encode().unwrap(), wire);
        assert_eq!(Response::decode(&wire).unwrap(), response);
    }
}
#[test]
fn optional_fields_follow_table_order_instead_of_parameter_code_order() {
    let mut response = Response::release(RejectCause::Extension);
    response.options.display = Some("A".into());
    response.options.cause_extension = Some(ExtendedCause::DataPrivacyNotAvailable);
    let wire = [0x1f, 0xf8, 0x01, 0x41, 0x30, 0x10];
    assert_eq!(response.encode().unwrap(), wire);
    assert_eq!(Response::decode(&wire).unwrap(), response);
    assert!(Response::decode(&[0x1f, 0xf3, 0x01, 0x80, 0x14, 0x10]).is_err());
    assert!(
        response
            .validate_version(ProtocolVersion::Is136RevA)
            .is_err()
    );
    response.validate_version(ProtocolVersion::Pv1).unwrap();
    for kind in [ResponseKind::Release, ResponseKind::Reorder] {
        let mut response = if kind == ResponseKind::Release {
            Response::release(RejectCause::Extension)
        } else {
            Response::reorder(RejectCause::Extension, Tone::Intercept)
        };
        response.options = ResponseOptions {
            signal: (kind == ResponseKind::Release).then_some(Signal {
                pitch: Pitch::Low,
                cadence: 12,
                duration_code: 0,
            }),
            subaddress: Some(Subaddress {
                kind: 2,
                odd: true,
                address: vec![0xff; 20],
            }),
            display: Some("Call ended".into()),
            multilingual_display: Some(MultilingualDisplay {
                encoding: 4,
                bytes: vec![0x03, 0xa9],
            }),
            cause_extension: Some(ExtendedCause::DataPrivacyNotSupported),
            ..Default::default()
        };
        let wire = response.encode().unwrap();
        assert_eq!(Response::decode(&wire).unwrap(), response);
        assert!(
            response
                .validate_version(ProtocolVersion::Is136RevA)
                .is_err()
        );
        response.validate_version(ProtocolVersion::Pv1).unwrap();
    }
}
#[test]
fn reserved_cause_extensions_tones_invalid_markers_duplicates_and_truncation_are_rejected() {
    for value in 0..=u8::MAX {
        assert_eq!(Response::decode(&[0x1f, 0xf3, value]).is_ok(), value < 2);
    }
    for tone in 0..4 {
        assert_eq!(
            Response::decode(&[0x20, 0x10 | (tone << 2)]).is_ok(),
            tone < 2
        );
    }
    for wire in [
        vec![0x1f],
        vec![0x5f, 0],
        vec![0x18, 0x10],
        vec![0x1f, 0x13, 0],
        vec![0x1f, 0xf3],
        vec![0x1f, 0xf3, 0, 0x30, 0],
        vec![0x1f, 0x18, 1],
        vec![0x1f, 0x1f, 0],
        vec![0x1f, 0x17, 0],
        vec![0x1f, 0x17, 0xf0],
        vec![0x1f, 0x10, 0],
    ] {
        assert!(Response::decode(&wire).is_err(), "{wire:02X?}");
    }
    let mut response = Response::reorder(RejectCause::Congestion, Tone::Reorder);
    response.options.signal = Some(Signal::default());
    assert!(response.encode().is_err());
    response.options.signal = None;
    response.tone = None;
    assert!(response.encode().is_err());
    response = Response::release(RejectCause::Congestion);
    response.tone = Some(Tone::Reorder);
    assert!(response.encode().is_err());
    response.tone = None;
    response.options.cause_extension = Some(ExtendedCause::DataPrivacyNotAvailable);
    assert!(response.encode().is_err());
}
#[test]
fn display_subaddress_bmp_and_aggregate_bounds_are_checked() {
    let mut response = Response::release(RejectCause::Unknown);
    response.options.subaddress = Some(Subaddress {
        kind: 0,
        odd: false,
        address: vec![1; 21],
    });
    assert!(response.encode().is_err());
    response.options.subaddress = None;
    for display in ["A".repeat(83), "é".into()] {
        response.options.display = Some(display);
        assert!(response.encode().is_err());
    }
    response.options.display = None;
    for bytes in [vec![0xd8, 0], vec![0, 1, 2], vec![0; 84]] {
        response.options.multilingual_display = Some(MultilingualDisplay { encoding: 4, bytes });
        assert!(response.encode().is_err());
    }
    response.options.multilingual_display = None;
    response.options.unparsed_bits = vec![true; 256 * 8];
    assert!(response.encode().is_err());
    assert!(Response::decode(&[0; 256]).is_err());
}
#[test]
fn unknown_critical_stops_and_unknown_noncritical_continues_the_valid_prefix() {
    let response = Response::decode(&[0x1f, 0x14, 0xab, 0xcd]).unwrap();
    assert_eq!(response.cause, RejectCause::Congestion);
    assert_eq!(response.options.unparsed_bits.len(), 20);
    assert_eq!(response.encode().unwrap(), [0x1f, 0x14, 0xab, 0xcd]);
    let response = Response::decode(&[0x1f, 0x1a, 1, 0xff, 0x80, 0x14, 0x10]).unwrap();
    assert_eq!(response.options.display.as_deref(), Some("A"));
    assert_eq!(response.options.ignored_bits.len(), 20);
    assert!(Response::decode(&[0x1f, 0x1a, 2, 0xff]).is_err());
    let extension = Response::decode(&[0x1f, 0xf7, 0x10]).unwrap();
    assert_eq!(extension.options.unparsed_bits.len(), 12);
    assert_eq!(extension.encode().unwrap(), [0x1f, 0xf7, 0x10]);
    let marker = Response::decode(&[0x1f, 0xf0]).unwrap();
    assert_eq!(marker.cause, RejectCause::Extension);
    assert!(marker.options.cause_extension.is_none());
}

#[test]
fn release_signal_has_control_layout_and_exhaustive_pitch_cadence_duration_validation() {
    for pitch in 0..4 {
        for cadence in 0..64 {
            for duration in 0..16 {
                let wire = [0x1f, 0x11, (pitch << 6) | cadence, duration << 4];
                assert_eq!(Response::decode(&wire).is_ok(), pitch < 3 && cadence <= 12);
            }
        }
    }
    let marker = Response::release(RejectCause::Extension);
    assert!(marker.validate_version(ProtocolVersion::Is136RevA).is_err());
    marker.validate_version(ProtocolVersion::Pv1).unwrap();
}
