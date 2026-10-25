use super::{
    audit::{AuditConfirmation, AuditOrder},
    capability_report,
    registration::{AcceptOptions, RegistrationRequest, SerialNumber},
};
fn wire(text: &str) -> Vec<u8> {
    let mut bits: Vec<_> = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            '0' => false,
            '1' => true,
            _ => panic!("invalid fixture bit"),
        })
        .collect();
    bits.resize(bits.len().div_ceil(8) * 8, false);
    bits.as_chunks::<8>()
        .0
        .iter()
        .map(|octet| octet.iter().fold(0, |v, bit| (v << 1) | u8::from(*bit)))
        .collect()
}
#[test]
fn unknown_noncritical_registration_ie_does_not_hide_pfc_or_sid() {
    // TIA-136-123-E §§5,5.4.12: 21-bit prefix, unknown8/length1/A5, PFC2/0, SID7/1/42.
    let bytes =
        wire("00001100 00011010 10101 1000 00000001 10100101 0010 000 0111 0001 000000000101010");
    let value = RegistrationRequest::decode(&bytes).unwrap();
    assert_eq!(value.options.pfc, Some(0));
    assert_eq!(value.options.sid_report, Some(42));
    assert_eq!(value.options.ignored_bits.len(), 20);
    assert!(value.options.unparsed_bits.is_empty());
    assert_eq!(
        RegistrationRequest::decode(&value.encode().unwrap()).unwrap(),
        value
    );
}
#[test]
fn unknown_noncritical_serial_ie_does_not_hide_equipment_fields() {
    let bytes = wire(
        "00001101 00010010 00110100 01010110 01111000 1000 00000001 10100101 0001 101010111100110111101111 0010 00010000001000000011000001000000",
    );
    let value = SerialNumber::decode(&bytes).unwrap();
    assert_eq!(value.esn, 0x12345678);
    assert_eq!(value.esnx, Some(0xabcdef));
    assert_eq!(value.s_esn, Some(0x10203040));
    assert_eq!(value.ignored_bits.len(), 20);
    assert!(value.unparsed_bits.is_empty());
    assert_eq!(
        SerialNumber::decode(&value.encode().unwrap()).unwrap(),
        value
    );
}
#[test]
fn unknown_noncritical_capability_ie_does_not_hide_voice_and_arq() {
    // TIA-136-123-E §5.4.5 has 65 mandatory bits for this protocol version.
    let bytes = wire(
        "00 000101 0101 01111 000010 000011 0100 00100000 101 000000 00000011 1000000 1000 00000001 10100101 0010 000001 0110 1",
    );
    let value = capability_report::decode(&bytes).unwrap();
    assert_eq!(value.voice_coder_map(), Some(1));
    assert_eq!(value.options.facch_sacch_arq, Some(true));
    assert_eq!(value.options.ignored_bits.len(), 20);
    assert!(value.options.unparsed_bits.is_empty());
    assert_eq!(
        capability_report::decode(&value.encode().unwrap()).unwrap(),
        value
    );
}
#[test]
fn unknown_noncritical_accept_ie_does_not_hide_pfc_assignment() {
    let value = AcceptOptions::decode(&wire("00011101 1000 00000001 10100101 0001 000")).unwrap();
    assert_eq!(value.pfc, Some(0));
    assert_eq!(value.ignored_bits.len(), 20);
    assert!(value.unparsed_bits.is_empty());
    assert_eq!(
        AcceptOptions::decode(&value.encode().unwrap()).unwrap(),
        value
    );
}
#[test]
fn unknown_noncritical_audit_ies_do_not_hide_supported_fields() {
    let value = AuditOrder::decode(&wire(
        "00010010 10 1010 00000001 10100101 0001 00000010 00000100 00100001",
    ))
    .unwrap();
    assert!(value.force_registration);
    assert!(!value.debug_display_allowed);
    assert_eq!(value.subaddress.as_ref().unwrap().address, [0x21]);
    assert_eq!(value.ignored_bits.len(), 20);
    assert!(value.unparsed_bits.is_empty());
    assert_eq!(AuditOrder::decode(&value.encode().unwrap()).unwrap(), value);
    let value = AuditConfirmation::decode(&wire(
        "00000001 000 1010 00000001 10100101 0001 0000 1000 00000001 01000001",
    ))
    .unwrap();
    assert_eq!(value.selected_system, Some(0));
    assert_eq!(value.display.as_deref(), Some("A"));
    assert_eq!(value.ignored_bits.len(), 20);
    assert!(value.unparsed_bits.is_empty());
    assert_eq!(
        AuditConfirmation::decode(&value.encode().unwrap()).unwrap(),
        value
    );
}
#[test]
fn every_unknown_noncritical_code_is_bounded_and_skipped() {
    for parameter in 8..=14 {
        for octets in [0, 1, 245] {
            let mut text =
                format!("00001101 00000000000000000000000000000000 {parameter:04b} {octets:08b}");
            text.push_str(&"10100101".repeat(octets));
            text.push_str("0001 000000000000000000000001");
            let bytes = wire(&text);
            let value = SerialNumber::decode(&bytes).unwrap();
            assert_eq!(value.esnx, Some(1));
            assert_eq!(value.ignored_bits.len(), 12 + octets * 8);
            assert!(value.unparsed_bits.is_empty());
        }
    }
    for suffix in ["1000", "1000 00000001", "1000 11111111 10100101"] {
        let bytes = wire(&format!(
            "00001101 00000000000000000000000000000000 {suffix}"
        ));
        assert!(SerialNumber::decode(&bytes).is_err());
    }
}
#[test]
fn unknown_critical_ie_accepts_prefix_and_stops_before_later_known_fields() {
    let value = SerialNumber::decode(&wire("00001101 00000000000000000000000000000000 0001 000000000000000000000001 0011 0010 00010000001000000011000001000000")).unwrap();
    assert_eq!(value.esnx, Some(1));
    assert_eq!(value.s_esn, None);
    assert!(value.ignored_bits.is_empty());
    assert!(!value.unparsed_bits.is_empty());
}

#[test]
fn skipping_unknown_fields_still_checks_known_order_and_length_bounds() {
    let repeated = wire(
        "00001101 00000000000000000000000000000000 0001 000000000000000000000001 1000 00000000 0001 000000000000000000000010",
    );
    assert!(SerialNumber::decode(&repeated).is_err());
    let unknown = "1110 11111111";
    assert!(
        RegistrationRequest::decode(&wire(&format!("00001100 00011010 10101 {unknown}"))).is_err()
    );
    assert!(AcceptOptions::decode(&wire(&format!("00011101 {unknown}"))).is_err());
    assert!(AuditOrder::decode(&wire(&format!("00010010 10 {unknown}"))).is_err());
    assert!(AuditConfirmation::decode(&wire(&format!("00000001 000 {unknown}"))).is_err());
    assert!(
        capability_report::decode(&wire(&format!(
            "00 000101 0101 01111 000010 000011 0100 00100000 101 000000 00000011 1000000 {unknown}"
        )))
        .is_err()
    );
}
