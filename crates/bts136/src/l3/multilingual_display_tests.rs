use super::{
    audit::{AuditConfirmation, MultilingualDisplay},
    call_control::{ExtendedCause, Response},
    control_fields::{self, Reader},
    message_waiting::{MessageWaiting, Waiting},
    page::Page,
    registration::RejectCause,
    user_alert::UserAlert,
};
fn display() -> MultilingualDisplay {
    MultilingualDisplay {
        encoding: 4,
        bytes: vec![0x03, 0xa9],
    }
}

#[test]
fn fixed_multilingual_vectors_count_the_encoding_octet_in_every_control_message() {
    let alert = UserAlert {
        multilingual_display: Some(display()),
        ..Default::default()
    };
    let alert_wire = [0x25, 0x90, 0x32, 0x00, 0x3a, 0x90];
    assert_eq!(alert.encode().unwrap(), alert_wire);
    assert_eq!(UserAlert::decode(&alert_wire).unwrap(), alert);
    let page = Page {
        multilingual_display: Some(display()),
        ..Default::default()
    };
    let page_wire = [0x18, 0x1c, 0x03, 0x20, 0x03, 0xa9];
    assert_eq!(page.encode().unwrap(), page_wire);
    assert_eq!(Page::decode(&page_wire).unwrap(), page);
    let audit = AuditConfirmation {
        multilingual_display: Some(display()),
        ..Default::default()
    };
    let audit_wire = [0x01, 0x12, 0x06, 0x40, 0x07, 0x52];
    assert_eq!(audit.encode().unwrap(), audit_wire);
    assert_eq!(AuditConfirmation::decode(&audit_wire).unwrap(), audit);
    let waiting = MessageWaiting {
        entries: vec![Waiting { kind: 0, count: 1 }],
        multilingual_display: Some(display()),
        ..Default::default()
    };
    let waiting_wire = [0x17, 0x00, 0x06, 0x40, 0xc8, 0x00, 0xea, 0x40];
    assert_eq!(waiting.encode().unwrap(), waiting_wire);
    assert_eq!(MessageWaiting::decode(&waiting_wire).unwrap(), waiting);
    for (mut response, wire) in [
        (
            Response::release(RejectCause::Extension),
            vec![0x1f, 0xf9, 0x03, 0x20, 0x03, 0xa9, 0x30, 0x10],
        ),
        (
            Response::registration_reject(RejectCause::Extension),
            vec![0x1e, 0xf9, 0x03, 0x20, 0x03, 0xa9, 0x30, 0x10],
        ),
        (
            Response::reorder(RejectCause::Extension, super::call_control::Tone::Reorder),
            vec![0x20, 0xf2, 0x40, 0xc8, 0x00, 0xea, 0x48, 0x04],
        ),
    ] {
        response.options.multilingual_display = Some(display());
        response.options.cause_extension = Some(ExtendedCause::DataPrivacyNotAvailable);
        assert_eq!(response.encode().unwrap(), wire);
        assert_eq!(Response::decode(&wire).unwrap(), response);
    }
}

#[test]
fn unknown_multilingual_field_can_be_skipped_without_losing_the_next_parameter() {
    let bytes = [0x1f, 0xf9, 0x03, 0x20, 0x03, 0xa9, 0x30, 0x10];
    let bits = control_fields::bits(&bytes).unwrap();
    let mut reader = Reader {
        bits: &bits,
        offset: 12,
    };
    let code = reader.take(4).unwrap() as u8;
    assert_eq!(code, 9);
    assert!(reader.skip_noncritical(code).unwrap());
    assert_eq!(reader.offset, 48);
    assert_eq!(reader.take(4).unwrap(), 3);
    assert_eq!(reader.take(8).unwrap(), 1);
}

#[test]
fn display_payload_bounds_and_empty_encoding_header_are_unambiguous() {
    for length in [0, 1, 2, 81, 82] {
        let alert = UserAlert {
            multilingual_display: Some(MultilingualDisplay {
                encoding: 3,
                bytes: vec![0xc4; length],
            }),
            ..Default::default()
        };
        let wire = alert.encode().unwrap();
        let bits = control_fields::bits(&wire).unwrap();
        let mut reader = Reader {
            bits: &bits,
            offset: 12,
        };
        assert_eq!(reader.take(8).unwrap(), (length + 1) as u64);
        assert_eq!(UserAlert::decode(&wire).unwrap(), alert);
    }
    assert!(
        UserAlert {
            multilingual_display: Some(MultilingualDisplay {
                encoding: 3,
                bytes: vec![0xc4; 83]
            }),
            ..Default::default()
        }
        .encode()
        .is_err()
    );
    for bytes in [
        &[0x25, 0x90, 0x00][..],
        &[0x25, 0x90, 0x22, 0x00, 0x3a, 0x90],
        &[0x25, 0x90, 0x32, 0x00, 0x3a],
    ] {
        assert!(UserAlert::decode(bytes).is_err(), "{bytes:02x?}");
    }
}
