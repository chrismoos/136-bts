use crate::l3::{
    broadcast::ProtocolVersion,
    legacy_traffic::{BaseStationCapabilities, CapabilityFields, CharacterSupport, StationPv1},
    service_menu::{ServiceMenu, ServiceMenuPv1},
};

pub(crate) const SERVING_PROTOCOL: ProtocolVersion = ProtocolVersion::Pv2;

const SUPPORTED_VOICE_CODERS: u8 = crate::codec::speech::Codec::Vselp.capability_mask()
    | crate::codec::speech::Codec::AcelpCc1.capability_mask();
const SMS_SUBMIT_AND_DELIVER: u8 = 3;
// IS-136.1-A §6.5 Menu Map identifies full-rate digital speech in bit two.
const FULL_RATE_DIGITAL_SPEECH_MENU: u16 = 1 << 2;
const UNKNOWN_SOC: u16 = 0;
const UNKNOWN_BSMC: u8 = 0;

pub(super) fn service_menu() -> ServiceMenu {
    ServiceMenu {
        voice_privacy_map: 0,
        data_privacy_map: 0,
        voice_coder_map: SUPPORTED_VOICE_CODERS,
        encryption_algorithms: [None; 8],
        encryption_key_map: 0,
        menu_map: FULL_RATE_DIGITAL_SPEECH_MENU,
        facch_sacch_arq: false,
        user_group: false,
        sms_map: SMS_SUBMIT_AND_DELIVER,
        ira: true,
        oats: Some(false),
        pv1: Some(ServiceMenuPv1 {
            latin1: true,
            bmp: true,
            ..Default::default()
        }),
        sms_broadcast: Some(false),
        extended_characters: None,
    }
}

pub(super) fn station_response() -> BaseStationCapabilities {
    let menu = service_menu();
    BaseStationCapabilities {
        common: CapabilityFields {
            protocol_version: SERVING_PROTOCOL,
            voice_privacy_map: menu.voice_privacy_map,
            voice_coder_map: menu.voice_coder_map,
            encryption_key_map: menu.encryption_key_map,
            facch_sacch_arq: menu.facch_sacch_arq,
            soc: UNKNOWN_SOC,
            bsmc: UNKNOWN_BSMC,
            sms_map: menu.sms_map,
            ira: menu.ira,
            encryption_algorithms: menu.encryption_algorithms,
        },
        pv1: Some(StationPv1 {
            characters: CharacterSupport {
                latin1: true,
                bmp: true,
                latin_hebrew: false,
            },
            ..Default::default()
        }),
        extended_bands: None,
        extended_characters: None,
    }
}

pub(super) fn append_metrics(
    readings: &mut Vec<crate::operations::proto::Metric>,
    capabilities: Option<&crate::l3::legacy_traffic::MobileCapabilities>,
    report_ms: Option<u64>,
    now_ms: u64,
) {
    use crate::operations::metric;
    let Some(capabilities) = capabilities else {
        return;
    };
    let common = &capabilities.common;
    readings.extend([
        metric(
            "Common capability protocol",
            format!("{:?}", SERVING_PROTOCOL.common(common.protocol_version)),
            "",
        ),
        metric(
            "Phone capability protocol",
            format!("{:?}", common.protocol_version),
            "",
        ),
        metric(
            "Phone voice coder map",
            format!("0x{:02X}", common.voice_coder_map),
            "",
        ),
        metric(
            "Phone voice privacy map",
            format!("0x{:X}", common.voice_privacy_map),
            "",
        ),
        metric(
            "Phone encryption key map",
            format!("0x{:X}", common.encryption_key_map),
            "",
        ),
        metric(
            "Phone encryption algorithms",
            format!("{:?}", common.encryption_algorithms),
            "",
        ),
        metric("Phone FACCH/SACCH ARQ support", common.facch_sacch_arq, ""),
        metric("Phone SMS capability map", common.sms_map, ""),
        metric("Phone IRA support", common.ira, ""),
        metric(
            "Phone supported bands",
            format!("0x{:02X}", capabilities.supported_bands),
            "",
        ),
        metric(
            "Phone 800 MHz analog speech support",
            capabilities.analog_800_speech,
            "",
        ),
    ]);
    if let Some(at) = report_ms {
        readings.push(metric(
            "Last capability report age",
            now_ms.saturating_sub(at),
            "ms",
        ));
    }
    if let Some(pv1) = &capabilities.pv1 {
        readings.extend([
            metric("Capability manufacturer", pv1.manufacturer, ""),
            metric("Capability model", pv1.model, ""),
            metric("Capability software vintage", pv1.software_vintage, ""),
            metric("Capability firmware vintage", pv1.firmware_vintage, ""),
            metric(
                "Phone maximum supported PFC",
                u16::from(pv1.max_supported_pfc) + 1,
                "",
            ),
        ]);
    }
    if let Some(pv5) = &capabilities.pv5 {
        readings.extend([
            metric(
                "Phone extended bands",
                format!("0x{:02X}", pv5.extended_bands),
                "",
            ),
            metric(
                "Phone modulation support",
                format!("0x{:02X}", pv5.modulation_map),
                "",
            ),
            metric(
                "Phone interleaving support",
                format!("0x{:02X}", pv5.interleaving_map),
                "",
            ),
            metric(
                "Phone positioning support",
                format!("0x{:02X}", pv5.positioning_map),
                "",
            ),
        ]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_response_advertises_exactly_the_broadcast_services() {
        let menu = service_menu();
        let response = station_response();
        assert_eq!(menu.voice_coder_map, 0x03);
        assert_eq!(response.common.protocol_version, ProtocolVersion::Pv2);
        assert_eq!(response.common.voice_privacy_map, menu.voice_privacy_map);
        assert_eq!(response.common.voice_coder_map, menu.voice_coder_map);
        assert_eq!(response.common.encryption_key_map, menu.encryption_key_map);
        assert_eq!(
            response.common.encryption_algorithms,
            menu.encryption_algorithms
        );
        assert_eq!(response.common.facch_sacch_arq, menu.facch_sacch_arq);
        assert_eq!(response.common.sms_map, menu.sms_map);
        assert_eq!(response.common.ira, menu.ira);
        let station = response.pv1.as_ref().unwrap();
        let broadcast = menu.pv1.as_ref().unwrap();
        assert_eq!(station.characters.latin1, broadcast.latin1);
        assert_eq!(station.characters.bmp, broadcast.bmp);
        assert_eq!(station.characters.latin_hebrew, broadcast.latin_hebrew);
        assert!(
            !station.async_data
                && !station.g3_fax
                && !station.dads
                && !station.oats
                && !station.user_group
        );
        assert_eq!(menu.sms_broadcast, Some(false));
        assert!(menu.encode(SERVING_PROTOCOL).is_ok());
        assert!(
            crate::l3::legacy_traffic::TrafficMessage::CapabilityUpdateResponse(
                crate::l3::legacy_traffic::CapabilityResponse::BaseStation(response.clone())
            )
            .encode()
            .is_ok()
        );
        assert!(response.extended_bands.is_none());
        assert!(response.extended_characters.is_none());
    }
}
