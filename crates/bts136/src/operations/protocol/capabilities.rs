use crate::l3::legacy_traffic::{CapabilityResponse, CharacterSupport};

fn characters(value: &CharacterSupport) -> String {
    format!(
        "Latin1={} BMP={} Hebrew={}",
        value.latin1, value.bmp, value.latin_hebrew
    )
}

pub(super) fn details(response: &CapabilityResponse) -> String {
    let common = response.common();
    let mut text = format!(
        "Capability Update Response\nPV={:?} VoicePrivacy=0x{:X} VoiceCoders=0x{:02X} EncryptionKeys=0x{:X}\nARQ={} SOC=0x{:03X} BSMC=0x{:02X} SMS=0x{:X} IRA={}\nEncryptionAlgorithms={:?}",
        common.protocol_version,
        common.voice_privacy_map,
        common.voice_coder_map,
        common.encryption_key_map,
        common.facch_sacch_arq,
        common.soc,
        common.bsmc,
        common.sms_map,
        common.ira,
        common.encryption_algorithms,
    );
    let extended_characters = match response {
        CapabilityResponse::BaseStation(station) => {
            if let Some(value) = &station.pv1 {
                text.push_str(&format!(
                    "\nAsyncData={} G3Fax={} DADS={} UserGroup={} OATS={}\n{}",
                    value.async_data,
                    value.g3_fax,
                    value.dads,
                    value.user_group,
                    value.oats,
                    characters(&value.characters)
                ));
            }
            if let Some(bands) = station.extended_bands {
                text.push_str(&format!("\nExtendedBands=0x{bands:02X}"));
            }
            &station.extended_characters
        }
        CapabilityResponse::Mobile(mobile) => {
            text.push_str(&format!(
                "\nSupportedBands=0x{:02X} Analog800Speech={}",
                mobile.supported_bands, mobile.analog_800_speech
            ));
            if let Some(value) = &mobile.pv1 {
                text.push_str(&format!("\nSCM={} Software={} Firmware={} Model={} Manufacturer={} MaxPFC={}\nAsyncData={} G3Fax={} DADS={}\nSubaddress={:?}\nUserGroup={} HalfRate={} DoubleRate={} TripleRate={} STU-III={}\n{}",
                    value.scm, value.software_vintage, value.firmware_vintage, value.model, value.manufacturer, u16::from(value.max_supported_pfc) + 1,
                    value.async_data, value.g3_fax, value.dads, value.subaddress, value.user_group, value.half_rate_dtc, value.double_rate_dtc, value.triple_rate_dtc, value.stu_iii, characters(&value.characters)));
            }
            if let Some(advice) = mobile.advice {
                text.push_str(&format!("\nADVICE={advice}"));
            }
            if let Some(value) = &mobile.pv5 {
                text.push_str(&format!("\nPositioning=0x{:02X} ExtendedBands=0x{:02X} Modulation=0x{:02X} Interleaving=0x{:02X}", value.positioning_map, value.extended_bands, value.modulation_map, value.interleaving_map));
            }
            &mobile.extended_characters
        }
    };
    if let Some(flags) = extended_characters {
        let flags: String = flags
            .iter()
            .map(|value| if *value { '1' } else { '0' })
            .collect();
        text.push_str(&format!("\nISO-8859 parts 2-7,9-15={flags}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::{broadcast::ProtocolVersion, legacy_traffic::*};

    #[test]
    fn maximum_capability_report_keeps_the_last_extension_within_the_trace_limit() {
        let response = CapabilityResponse::Mobile(MobileCapabilities {
            common: CapabilityFields {
                protocol_version: ProtocolVersion::Pv7,
                voice_privacy_map: 15,
                voice_coder_map: 63,
                encryption_key_map: 15,
                facch_sacch_arq: true,
                soc: 4095,
                bsmc: 255,
                sms_map: 3,
                ira: true,
                encryption_algorithms: [Some(15); 8],
            },
            supported_bands: 255,
            analog_800_speech: true,
            pv1: Some(MobilePv1 {
                scm: 31,
                software_vintage: 63,
                firmware_vintage: 63,
                model: 255,
                manufacturer: 255,
                max_supported_pfc: 7,
                subaddress: Some(Subaddress {
                    odd: true,
                    kind: 2,
                    address: vec![255; 20],
                }),
                ..Default::default()
            }),
            advice: Some(true),
            pv5: Some(MobilePv5 {
                positioning_map: 255,
                extended_bands: 255,
                modulation_map: 255,
                interleaving_map: 255,
            }),
            extended_characters: Some([true; 13]),
        });
        let text = details(&response);
        assert!(
            text.len() <= super::super::MAX_RECORD_BYTES,
            "{} bytes",
            text.len()
        );
        assert!(text.ends_with("ISO-8859 parts 2-7,9-15=1111111111111"));
    }
}
