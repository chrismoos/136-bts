use crate::l3::{
    access_call::{CallOptions, OriginationRequest, PageResponseRequest},
    address::{AddressInfo, AddressValue},
};
pub(super) fn address(value: &AddressInfo) -> String {
    let number = match &value.address {
        AddressValue::Tbcd(number) => number.clone(),
        AddressValue::BinaryInternet(value) => value.to_string(),
        AddressValue::Ira(bytes) | AddressValue::DpcSsn(bytes) => {
            bytes.iter().map(|byte| format!("{byte:02X}")).collect()
        }
    };
    format!("{}/{}:{number}", value.number_type, value.numbering_plan)
}
fn options(value: &CallOptions) -> String {
    let mut text = String::new();
    if !value.voices.is_empty() {
        text.push_str(&format!(
            "\nVoiceModes(VC/PM)={}",
            value
                .voices
                .iter()
                .map(|voice| format!("{}/{}", voice.coder, voice.privacy))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if !value.extended_voices.is_empty() {
        let groups = value
            .extended_voices
            .iter()
            .map(|voice| {
                format!(
                    "{}:{}",
                    voice.privacy,
                    voice
                        .modes
                        .iter()
                        .map(|mode| format!(
                            "{}{}{}",
                            mode.coder, mode.modulation, mode.interleaving
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        text.push_str(&format!("\nExtendedVoice(PM:VC/MOD/INT digits)={groups}"));
    }
    if let Some(bandwidth) = value.bandwidth {
        text.push_str(&format!("\nBandwidth={bandwidth:?}"));
    }
    if let Some(encryption) = value.encryption {
        text.push_str(&format!(
            "\nEncryption={}/{}/{}",
            encryption.algorithm, encryption.domain, encryption.key
        ));
    }
    if let Some(data) = value.data_mode {
        text.push_str(&format!("\nDataMode=0x{data:03X}"));
    }
    if let Some(presentation) = value.presentation {
        text.push_str(&format!(
            "\nPresentation={} Screening={}",
            presentation.presentation, presentation.screening
        ));
    }
    if let Some(number) = &value.calling_party {
        text.push_str(&format!("\nCallingNumber={}", address(number)));
    }
    if let Some(group) = value.group {
        text.push_str(&format!("\nGroup={}:0x{:X}", group.kind, group.value));
    }
    for (name, subaddress) in [
        ("CalledSubaddress", value.called_subaddress.as_ref()),
        ("CallingSubaddress", value.calling_subaddress.as_ref()),
    ] {
        if let Some(subaddress) = subaddress {
            let hex: String = subaddress
                .address
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect();
            text.push_str(&format!(
                "\n{name}={}/odd{}:{hex}",
                subaddress.kind,
                u8::from(subaddress.odd)
            ));
        }
    }
    if let Some(system) = value.selected_system {
        text.push_str(&format!("\nSelectedSystem=0x{system:X}"));
    }
    if !value.ignored_bits.is_empty() {
        text.push_str(&format!(
            "\nIgnoredNoncriticalBits={}",
            value.ignored_bits.len()
        ));
    }
    if !value.unparsed_bits.is_empty() {
        text.push_str(&format!("\nUnparsedBits={}", value.unparsed_bits.len()));
    }
    text
}
pub(super) fn origination(value: &OriginationRequest) -> String {
    format!(
        "PV={:?} Emergency={} Called={} LastTry={} SCM={} Service={:?}{}",
        value.message.protocol_version,
        value.message.emergency_call,
        address(&value.message.called_party),
        value.message.last_try,
        value.message.scm,
        value.message.service_code,
        options(&value.options)
    )
}
pub(super) fn page(value: &PageResponseRequest) -> String {
    format!(
        "PV={:?} LastTry={} SCM={} Service={:?}{}",
        value.message.protocol_version,
        value.message.last_try,
        value.message.scm,
        value.message.service_code,
        options(&value.options)
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::{
        access_call::{LegacyVoice, PageResponseRequest},
        broadcast::ProtocolVersion,
        capability_report::{ExtendedVoice, VoiceMode},
        page_response::{PageResponse, ServiceCode},
    };
    #[test]
    fn maximum_call_preference_lists_fit_the_trace_limit_without_losing_the_last_entry() {
        for extended in [false, true] {
            let value = PageResponseRequest {
                message: PageResponse {
                    protocol_version: ProtocolVersion::Pv3,
                    last_try: false,
                    scm: 31,
                    service_code: ServiceCode::DigitalSpeech,
                },
                options: if extended {
                    CallOptions {
                        extended_voices: vec![
                            ExtendedVoice {
                                privacy: 4,
                                modes: vec![
                                    VoiceMode {
                                        coder: 5,
                                        modulation: 3,
                                        interleaving: 7
                                    };
                                    8
                                ]
                            };
                            25
                        ],
                        ..Default::default()
                    }
                } else {
                    CallOptions {
                        voices: vec![
                            LegacyVoice {
                                coder: 5,
                                privacy: 4
                            };
                            201
                        ],
                        ..Default::default()
                    }
                },
            };
            let bytes = value.encode().unwrap();
            let decoded = PageResponseRequest::decode(&bytes).unwrap();
            let text = page(&decoded);
            assert!(
                text.len() <= super::super::MAX_RECORD_BYTES,
                "{}",
                text.len()
            );
            assert!(text.ends_with(if extended { "537" } else { "5/4" }));
        }
    }
}
