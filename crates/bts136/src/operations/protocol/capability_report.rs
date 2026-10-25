use crate::l3::capability_report::CapabilityReport;
pub(super) fn details(report: &CapabilityReport) -> String {
    let s = &report.services;
    let o = &report.options;
    let mut text = format!(
        "PV={:?} SCM={} SW={} FW={} Model4=0x{:X} Mfr=0x{:02X} MaxPFC={}\nBands=0x{:02X} SOC={} BSMC={} Data={} Fax={} SMS-BC={} Subaddr={} IRA={} Group={} Analog800={} Rates(H/D/T)={}/{}/{} STU-III={}",
        report.protocol_version,
        report.scm,
        report.software_vintage,
        report.firmware_vintage,
        report.model,
        report.manufacturer,
        report.max_supported_pfc + 1,
        report.supported_bands,
        u8::from(s.soc),
        u8::from(s.bsmc),
        u8::from(s.async_data),
        u8::from(s.g3_fax),
        u8::from(s.sms_broadcast),
        u8::from(s.subaddressing),
        u8::from(s.ira),
        u8::from(s.user_group),
        u8::from(s.analog_800_speech),
        u8::from(s.half_rate_dtc),
        u8::from(s.double_rate_dtc),
        u8::from(s.triple_rate_dtc),
        u8::from(s.stu_iii)
    );
    text.push_str(&format!("\nDTCBands={}", report.dtc_band_codes()));
    if let Some(address) = &o.subaddress {
        let hex: String = address
            .address
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect();
        text.push_str(&format!(
            "\nSubaddress kind={} odd={} data={hex}",
            address.kind,
            u8::from(address.odd)
        ));
    }
    for (name, value) in [
        ("VoiceCoders", o.voice_coder_map.map(u64::from)),
        ("AlternateSOC", o.alternate_soc.map(u64::from)),
        ("CompleteModel", o.complete_model.map(u64::from)),
        ("CharacterMap", o.character_map.map(u64::from)),
        ("FACCH/SACCH-ARQ", o.facch_sacch_arq.map(u64::from)),
    ] {
        if let Some(value) = value {
            text.push_str(&format!("\n{name}=0x{value:X}"));
        }
    }
    let e = &o.extensions;
    for (name, value) in [
        ("DADS", e.dads.map(u64::from)),
        ("ADVICE", e.advice.map(u64::from)),
        ("Positioning", e.positioning.map(u64::from)),
        ("ExtendedBands", e.frequency_bands.map(u64::from)),
        ("LanguagesExt", e.multilingual.map(u64::from)),
    ] {
        if let Some(value) = value {
            text.push_str(&format!("\n{name}=0x{value:X}"));
        }
    }
    if let Some(map) = &e.encryption {
        let domains = map
            .algorithms
            .iter()
            .enumerate()
            .filter_map(|(i, value)| value.map(|value| format!("{i}:{value:X}")))
            .collect::<Vec<_>>()
            .join(",");
        text.push_str(&format!("\nEncryptionDomains/Algorithms={domains}"));
    }
    if let Some(voice) = &e.voice {
        let modes = voice
            .modes
            .iter()
            .map(|mode| format!("{}/{}/{}", mode.coder, mode.modulation, mode.interleaving))
            .collect::<Vec<_>>()
            .join(",");
        text.push_str(&format!(
            "\nVoicePrivacy={} Modes(coder/modulation/interleaving)={modes}",
            voice.privacy
        ));
    }
    if !o.ignored_bits.is_empty() {
        text.push_str(&format!("\nIgnored optional bits={}", o.ignored_bits.len()));
    }
    if !o.unparsed_bits.is_empty() {
        let hex: String = o
            .unparsed_bits
            .chunks(4)
            .map(|bits| {
                let nibble = bits
                    .iter()
                    .fold(0u8, |value, bit| value << 1 | u8::from(*bit))
                    << (4 - bits.len());
                format!("{nibble:X}")
            })
            .collect();
        text.push_str(&format!("\nUnknownBits={}:{}", o.unparsed_bits.len(), hex));
    }
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoded_band_labels_distinguish_pcs_blocks_and_extended_bands() {
        let mut report =
            crate::l3::capability_report::decode(&[5, 0x57, 0x84, 0x1a, 0x10, 0x50, 0, 0xe0, 0])
                .unwrap();
        report.supported_bands = 0x90;
        report.services.analog_800_speech = true;
        report.protocol_version = ProtocolVersion::Pv5;
        report.options.extensions.frequency_bands = Some(1);
        let (name, decoded) = super::super::decode::control("Reverse", &report.encode().unwrap());
        assert_eq!(name, "Capability report");
        assert!(decoded.contains("DTCBands=PCS:D,700CD,IMT-2000"));
        assert!(decoded.contains("Bands=0x90"));
        assert!(!decoded.contains("DTCBands=800"));
    }
    use crate::l3::{
        broadcast::ProtocolVersion,
        capability_report::{ReportOptions, SupportedServices},
        legacy_traffic::Subaddress,
    };
    #[test]
    fn maximum_access_capability_report_retains_all_optional_data_in_trace_budget() {
        let mut report = CapabilityReport {
            protocol_version: ProtocolVersion::Pv7,
            scm: 31,
            software_vintage: 63,
            firmware_vintage: 63,
            model: 15,
            manufacturer: 255,
            max_supported_pfc: 7,
            supported_bands: 255,
            services: SupportedServices::default(),
            options: ReportOptions {
                subaddress: Some(Subaddress {
                    kind: 2,
                    odd: true,
                    address: vec![255; 20],
                }),
                voice_coder_map: Some(63),
                alternate_soc: Some(4095),
                complete_model: Some(255),
                character_map: Some(4095),
                facch_sacch_arq: Some(true),
                extensions: Default::default(),
                ignored_bits: Vec::new(),
                unparsed_bits: vec![],
            },
        };
        // TIA-136-123-E §5.4.5 gives 304 bits through these known optional fields.
        report.options.unparsed_bits = vec![true; 255 * 8 - 304];
        let bytes = report.encode().unwrap();
        assert_eq!(bytes.len(), 255);
        let decoded = crate::l3::capability_report::decode(&bytes).unwrap();
        let text = details(&decoded);
        assert!(
            text.len() <= super::super::MAX_RECORD_BYTES,
            "{} bytes",
            text.len()
        );
        assert!(text.contains("CompleteModel=0xFF"));
        assert!(text.ends_with(&"F".repeat((255 * 8 - 304) / 4)));
    }
}
#[cfg(test)]
mod extension_tests {
    use super::*;
    use crate::l3::{
        broadcast::ProtocolVersion,
        capability_report::{
            EncryptionMap, ExtendedVoice, ReportExtensions, ReportOptions, SupportedServices,
            VoiceMode,
        },
        legacy_traffic::Subaddress,
    };
    #[test]
    fn maximum_extended_report_keeps_all_fields_inside_the_trace_limit() {
        let mut value = CapabilityReport {
            protocol_version: ProtocolVersion::Pv7,
            scm: 31,
            software_vintage: 63,
            firmware_vintage: 63,
            model: 15,
            manufacturer: 255,
            max_supported_pfc: 7,
            supported_bands: 255,
            services: SupportedServices::default(),
            options: ReportOptions {
                subaddress: Some(Subaddress {
                    kind: 2,
                    odd: true,
                    address: vec![255; 20],
                }),
                alternate_soc: Some(4095),
                complete_model: Some(255),
                character_map: Some(4095),
                facch_sacch_arq: Some(true),
                extensions: ReportExtensions {
                    dads: Some(true),
                    encryption: Some(EncryptionMap {
                        algorithms: [Some(15); 8],
                    }),
                    advice: Some(true),
                    voice: Some(ExtendedVoice {
                        privacy: 4,
                        modes: vec![
                            VoiceMode {
                                coder: 6,
                                modulation: 3,
                                interleaving: 7
                            };
                            8
                        ],
                    }),
                    positioning: Some(255),
                    frequency_bands: Some(255),
                    multilingual: Some(0xf000),
                },
                // TIA-136-123-E §5.4.5 gives 494 bits through this complete known-field set.
                ignored_bits: Vec::new(),
                unparsed_bits: vec![true; 255 * 8 - 494],
                ..Default::default()
            },
        };
        let bytes = value.encode().unwrap();
        assert_eq!(bytes.len(), 255);
        let decoded = crate::l3::capability_report::decode(&bytes).unwrap();
        let text = details(&decoded);
        assert!(
            text.len() <= super::super::MAX_RECORD_BYTES,
            "{} bytes",
            text.len()
        );
        assert!(text.contains("LanguagesExt=0xF000"));
        assert!(text.ends_with('C'));
        value.options.ignored_bits = vec![
            true, false, false, false, false, false, false, false, false, false, false, false,
        ];
        value
            .options
            .unparsed_bits
            .truncate(255 * 8 - 494 - value.options.ignored_bits.len());
        let bytes = value.encode().unwrap();
        assert_eq!(bytes.len(), 255);
        let decoded = crate::l3::capability_report::decode(&bytes).unwrap();
        let text = details(&decoded);
        assert!(text.contains("Ignored optional bits=12"));
        assert!(
            text.len() <= super::super::MAX_RECORD_BYTES,
            "{} bytes",
            text.len()
        );
        assert!(text.ends_with('C'));
    }
}
