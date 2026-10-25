use super::*;
use crate::l3::{
    capability_report::{self, CapabilityReport},
    control_fields,
};
fn report(extensions: ReportExtensions) -> CapabilityReport {
    let mut report = capability_report::tests::report();
    report.protocol_version = ProtocolVersion::Pv7;
    report.options.extensions = extensions;
    report
}
fn voice() -> ExtendedVoice {
    ExtendedVoice {
        privacy: 2,
        modes: vec![
            VoiceMode {
                coder: 6,
                modulation: 1,
                interleaving: 7,
            },
            VoiceMode {
                coder: 1,
                modulation: 0,
                interleaving: 1,
            },
        ],
    }
}
fn raw(tail: &str) -> Vec<u8> {
    let bytes = report(Default::default()).encode().unwrap();
    let mut bits = control_fields::bits(&bytes).unwrap();
    bits.truncate(65);
    bits.extend(tail.bytes().map(|byte| byte == b'1'));
    control_fields::pack(bits).unwrap()
}
#[test]
fn all_extensions_match_fixed_unaligned_vector_and_keep_voice_preference() {
    let value = report(ReportExtensions {
        dads: Some(true),
        encryption: Some(EncryptionMap {
            algorithms: [Some(1), Some(9), None, None, None, None, None, None],
        }),
        advice: Some(false),
        voice: Some(voice()),
        positioning: Some(15),
        frequency_bands: Some(1),
        multilingual: Some(0xf000),
    });
    // TIA-136-123-E §5.4.5: 65 mandatory bits, then auxiliary fields 1..7 without octet padding.
    let wire = [
        0x05, 0xb7, 0x84, 0x1a, 0x10, 0x50, 0x00, 0xe0, 0x38, 0xdc, 0x80, 0xe4, 0x5c, 0xce, 0x88,
        0xe7, 0x90, 0xba, 0x87, 0xbb, 0x00, 0xbb, 0xf8, 0x00, 0x00,
    ];
    assert_eq!(value.encode().unwrap(), wire);
    assert_eq!(capability_report::decode(&wire).unwrap(), value);
    assert_eq!(value.voice_coder_map(), Some(17));
}
#[test]
fn each_extension_has_its_own_version_and_zero_presence() {
    for (minimum, extensions) in [
        (
            ProtocolVersion::Pv1,
            ReportExtensions {
                dads: Some(false),
                ..Default::default()
            },
        ),
        (
            ProtocolVersion::Pv4,
            ReportExtensions {
                encryption: Some(EncryptionMap::default()),
                ..Default::default()
            },
        ),
        (
            ProtocolVersion::Pv4,
            ReportExtensions {
                advice: Some(false),
                ..Default::default()
            },
        ),
        (
            ProtocolVersion::Pv5,
            ReportExtensions {
                voice: Some(voice()),
                ..Default::default()
            },
        ),
        (
            ProtocolVersion::Pv5,
            ReportExtensions {
                positioning: Some(0),
                ..Default::default()
            },
        ),
        (
            ProtocolVersion::Pv5,
            ReportExtensions {
                frequency_bands: Some(0),
                ..Default::default()
            },
        ),
        (
            ProtocolVersion::Pv6,
            ReportExtensions {
                multilingual: Some(0),
                ..Default::default()
            },
        ),
    ] {
        for version in [
            ProtocolVersion::Is136RevA,
            ProtocolVersion::Pv1,
            ProtocolVersion::Pv4,
            ProtocolVersion::Pv5,
            ProtocolVersion::Pv6,
            ProtocolVersion::Pv7,
        ] {
            let mut value = report(extensions.clone());
            value.protocol_version = version;
            if version.code() < minimum.code() {
                assert!(value.encode().is_err());
            } else {
                assert_eq!(
                    capability_report::decode(&value.encode().unwrap()).unwrap(),
                    value
                );
            }
        }
        let mut wire = report(extensions).encode().unwrap();
        wire[1] = (ProtocolVersion::Is136RevA.code() << 4) | (wire[1] & 15);
        assert!(capability_report::decode(&wire).is_err());
    }
}
#[test]
fn malformed_extensions_duplicates_reserved_modes_and_conflicts_are_rejected() {
    for tail in [
        "011100010011100010",
        "01110100000111",
        "011101110000000000000001",
    ] {
        assert!(capability_report::decode(&raw(tail)).is_err());
    }
    for invalid in [
        ExtendedVoice {
            privacy: 0,
            modes: vec![],
        },
        ExtendedVoice {
            privacy: 0,
            modes: vec![voice().modes[0]; 9],
        },
        ExtendedVoice {
            privacy: 3,
            ..voice()
        },
        ExtendedVoice {
            privacy: 0,
            modes: vec![VoiceMode {
                coder: 1,
                modulation: 0,
                interleaving: 0,
            }],
        },
        ExtendedVoice {
            privacy: 0,
            modes: vec![VoiceMode {
                coder: 7,
                modulation: 0,
                interleaving: 0,
            }],
        },
    ] {
        assert!(
            report(ReportExtensions {
                voice: Some(invalid),
                ..Default::default()
            })
            .encode()
            .is_err()
        );
    }
    let mut value = report(ReportExtensions {
        voice: Some(voice()),
        ..Default::default()
    });
    value.options.voice_coder_map = Some(1);
    assert!(value.encode().is_err());
    let mut bits = control_fields::bits(&report(Default::default()).encode().unwrap()).unwrap();
    bits.truncate(65);
    bits.extend("0010000001".bytes().map(|byte| byte == b'1'));
    ReportExtensions {
        voice: Some(voice()),
        ..Default::default()
    }
    .write(&mut bits, ProtocolVersion::Pv7)
    .unwrap();
    assert!(capability_report::decode(&control_fields::pack(bits).unwrap()).is_err());
    assert!(
        report(ReportExtensions {
            multilingual: Some(1),
            ..Default::default()
        })
        .encode()
        .is_err()
    );
    let mut map = EncryptionMap::default();
    map.algorithms[0] = Some(16);
    assert!(
        report(ReportExtensions {
            encryption: Some(map),
            ..Default::default()
        })
        .encode()
        .is_err()
    );
    let wire = raw("0111100011111111");
    assert_eq!(
        capability_report::decode(&wire).unwrap().encode().unwrap(),
        wire
    );
}

#[test]
fn coder_summary_uses_standard_map_positions_and_never_defaults_explicit_no_coder() {
    for (coder, expected) in [(0, 0), (1, 1), (2, 2), (3, 4), (4, 32), (5, 8), (6, 16)] {
        let report = report(ReportExtensions {
            voice: Some(ExtendedVoice {
                privacy: 0,
                modes: vec![VoiceMode {
                    coder,
                    modulation: 0,
                    interleaving: if coder == 1 { 1 } else { 0 },
                }],
            }),
            ..Default::default()
        });
        assert_eq!(
            capability_report::decode(&report.encode().unwrap())
                .unwrap()
                .voice_coder_map(),
            Some(expected)
        );
    }
}
