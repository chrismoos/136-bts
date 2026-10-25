use super::*;
pub(crate) fn report() -> CapabilityReport {
    CapabilityReport {
        protocol_version: ProtocolVersion::Pv1,
        scm: 15,
        software_vintage: 2,
        firmware_vintage: 3,
        model: 4,
        manufacturer: 0x20,
        max_supported_pfc: 5,
        supported_bands: 3,
        services: SupportedServices {
            ira: true,
            ..Default::default()
        },
        options: ReportOptions::default(),
    }
}
#[test]
fn capability_report_fixed_mandatory_vector_and_every_octet_truncation() {
    let message = report();
    let text =
        "00 000101 0101 01111 000010 000011 0100 00100000 101 000000 00000011 1000000 0000000";
    let bits: Vec<_> = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c == '1')
        .collect();
    let bytes: Vec<_> = bits
        .as_chunks::<OCTET_BITS>()
        .0
        .iter()
        .map(|c| c.iter().fold(0u8, |value, bit| value << 1 | u8::from(*bit)))
        .collect();
    assert_eq!(bits.len(), 72);
    assert_eq!(message.encode().unwrap(), bytes);
    assert_eq!(decode(&bytes).unwrap(), message);
    for end in 0..bytes.len() {
        assert!(decode(&bytes[..end]).is_err());
    }
    assert_eq!(message.voice_coder_map(), Some(VSELP_MAP));
}
#[test]
fn capability_report_preserves_optional_zero_flags_subaddress_and_future_tail() {
    let mut message = report();
    message.options = ReportOptions {
        subaddress: Some(Subaddress {
            kind: 2,
            odd: true,
            address: vec![0x21; 20],
        }),
        voice_coder_map: Some(0),
        alternate_soc: Some(0),
        complete_model: Some(0),
        character_map: Some(0),
        facch_sacch_arq: Some(false),
        extensions: Default::default(),
        ignored_bits: Vec::new(),
        unparsed_bits: vec![],
    };
    let bytes = message.encode().unwrap();
    assert_eq!(decode(&bytes).unwrap(), message);
    message.options.unparsed_bits = vec![false, true, true, true, true, false, true];
    let bytes = message.encode().unwrap();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.encode().unwrap(), bytes);
    assert!(!decoded.options.unparsed_bits.is_empty());
    message.options.voice_coder_map = None;
    assert_eq!(message.voice_coder_map(), None);
    message.services.soc = true;
    assert!(message.encode().is_err());
    message.services.soc = false;
    message.protocol_version = ProtocolVersion::Is136RevA;
    assert!(message.encode().is_err());
}
#[test]
fn capability_report_rejects_known_truncation_duplicate_parameters_and_bounds() {
    let mut message = report();
    message.options.voice_coder_map = Some(1);
    message.options.unparsed_bits = vec![
        false, false, true, false, false, false, false, false, false, true,
    ];
    assert!(decode(&message.encode().unwrap()).is_err());
    message.options.unparsed_bits.clear();
    message.options.subaddress = Some(Subaddress {
        address: vec![0; MAX_SUBADDRESS_OCTETS + 1],
        ..Default::default()
    });
    assert!(message.encode().is_err());
    message.options.subaddress = None;
    message.max_supported_pfc = 8;
    assert!(message.encode().is_err());
    message.max_supported_pfc = 5;
    message.options.unparsed_bits = vec![true; MAX_REPORT_OCTETS * OCTET_BITS];
    assert_eq!(message.encode(), Err(ReportError::Length));
    assert_eq!(
        decode(&vec![0; MAX_REPORT_OCTETS + 1]),
        Err(ReportError::Length)
    );
    message.options.unparsed_bits.clear();
    message.options.subaddress = Some(Subaddress {
        address: vec![0x21; MAX_SUBADDRESS_OCTETS],
        ..Default::default()
    });
    let bytes = message.encode().unwrap();
    for end in 9..bytes.len() - 1 {
        assert!(decode(&bytes[..end]).is_err(), "truncated at {end}");
    }
}
#[test]
fn capability_report_stu_iii_and_option_boundaries_follow_release_columns() {
    let mut report = report();
    report.protocol_version = ProtocolVersion::Is136Rev0;
    let bytes = [5, 0x27, 0x84, 0x1a, 0x10, 0x50, 0, 0xe0];
    assert_eq!(report.encode().unwrap(), bytes);
    assert_eq!(decode(&bytes).unwrap(), report);
    report.services.stu_iii = true;
    assert!(report.encode().is_err());
    report.services.stu_iii = false;
    report.options.voice_coder_map = Some(1);
    assert!(report.encode().is_err());
    report.protocol_version = ProtocolVersion::Is136RevA;
    assert_eq!(decode(&report.encode().unwrap()).unwrap(), report);
    report.options.facch_sacch_arq = Some(false);
    assert!(report.encode().is_err());
    report.protocol_version = ProtocolVersion::Pv1;
    assert_eq!(decode(&report.encode().unwrap()).unwrap(), report);
}
