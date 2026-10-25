use super::{metric, proto::Metric, unavailable};
use crate::core::registry::ReportedCapabilities;
pub(super) fn measurements(reported: Option<&ReportedCapabilities>) -> Vec<Metric> {
    let mut values = vec![unavailable(
        "RSSI",
        "Calibrated per-mobile RSSI is not exposed",
    )];
    let Some(reported) = reported else {
        values.push(unavailable(
            "Handset model",
            "No access-channel capability report received",
        ));
        return values;
    };
    let report = &reported.report;
    let bands = report.dtc_band_names();
    let model = report
        .options
        .complete_model
        .map(|value| format!("0x{value:02X}"))
        .unwrap_or_else(|| format!("4-bit code 0x{:X}", report.model));
    values.extend([
        metric("Handset model", model, ""),
        metric(
            "Handset manufacturer code",
            format!("0x{:02X}", report.manufacturer),
            "",
        ),
        metric(
            "Capability report protocol",
            format!("{:?}", report.protocol_version),
            "",
        ),
        metric(
            "Capability report received",
            reported.received_superframe,
            "superframe",
        ),
        metric("Software vintage", report.software_vintage, ""),
        metric("Firmware vintage", report.firmware_vintage, ""),
        metric(
            "Reported maximum PFC",
            report.max_supported_pfc + 1,
            "frames",
        ),
        metric(
            "Reported frequency bands",
            format!("0x{:02X}", report.supported_bands),
            "",
        ),
        metric(
            "Reported DTC bands",
            if bands.is_empty() {
                "None reported".into()
            } else {
                bands.join(", ")
            },
            "",
        ),
        metric("IRA text support", report.services.ira, ""),
        metric("SMS broadcast support", report.services.sms_broadcast, ""),
        metric("Async data support", report.services.async_data, ""),
        metric("G3 fax support", report.services.g3_fax, ""),
        metric("Half-rate support", report.services.half_rate_dtc, ""),
        metric("Double-rate support", report.services.double_rate_dtc, ""),
        metric("Triple-rate support", report.services.triple_rate_dtc, ""),
    ]);
    if let Some(map) = report.voice_coder_map() {
        values.push(metric(
            "Reported voice coder map",
            format!("0x{map:02X}"),
            "",
        ));
    }
    let extensions = &report.options.extensions;
    for (name, value) in [
        (
            "DADS support",
            extensions.dads.map(|value| value.to_string()),
        ),
        (
            "ADVICE support",
            extensions.advice.map(|value| value.to_string()),
        ),
        (
            "Reported positioning map",
            extensions.positioning.map(|value| format!("0x{value:02X}")),
        ),
        (
            "Reported extended bands",
            extensions
                .frequency_bands
                .map(|value| format!("0x{value:02X}")),
        ),
        (
            "Reported extended character map",
            extensions
                .multilingual
                .map(|value| format!("0x{value:04X}")),
        ),
    ] {
        if let Some(value) = value {
            values.push(metric(name, value, ""));
        }
    }
    if let Some(map) = &extensions.encryption {
        values.push(metric(
            "Reported encryption algorithms",
            map.algorithms
                .iter()
                .enumerate()
                .filter_map(|(domain, value)| value.map(|value| format!("{domain}:0x{value:X}")))
                .collect::<Vec<_>>()
                .join(","),
            "",
        ));
    }
    if let Some(voice) = &extensions.voice {
        values.push(metric("Reported voice privacy", voice.privacy, ""));
        values.push(metric(
            "Reported voice modes (coder/modulation/interleaving)",
            voice
                .modes
                .iter()
                .map(|mode| format!("{}/{}/{}", mode.coder, mode.modulation, mode.interleaving))
                .collect::<Vec<_>>()
                .join(","),
            "",
        ));
    }
    if !report.options.unparsed_bits.is_empty() {
        values.push(metric(
            "Unparsed capability bits",
            report.options.unparsed_bits.len(),
            "bits",
        ));
    }
    values
}
pub(super) fn registration(
    options: Option<&crate::l3::registration::RegistrationOptions>,
) -> Vec<Metric> {
    let mut values = Vec::new();
    if let Some(options) = options {
        if let Some(pfc) = options.pfc {
            values.push(metric("Registration requested PFC", pfc + 1, "frames"));
            values.push(metric("Assigned PFC", 1, "frames"));
        }
        if let Some(sid) = options.sid_report {
            values.push(metric("Reported previous SID", sid, ""));
        }
        if let Some(request) = options.number_request {
            values.push(metric("Directory number requested", request == 1, ""));
        }
        if let Some(mode) = options.encryption {
            values.push(metric(
                "Registration encryption algorithm",
                mode.algorithm,
                "",
            ));
        }
        if !options.unparsed_bits.is_empty() {
            values.push(metric(
                "Unparsed registration bits",
                options.unparsed_bits.len(),
                "bits",
            ));
        }
    }
    values
}

pub(super) fn serial(value: Option<&crate::l3::registration::SerialNumber>) -> Vec<Metric> {
    let mut values = Vec::new();
    if let Some(serial) = value {
        if let Some(esnx) = serial.esnx {
            values.push(metric("Reported ESNX", format!("0x{esnx:06X}"), ""));
        }
        if let Some(esn) = serial.s_esn {
            values.push(metric("Reported S-ESN", format!("0x{esn:08X}"), ""));
        }
        if !serial.unparsed_bits.is_empty() {
            values.push(metric(
                "Unparsed Serial Number bits",
                serial.unparsed_bits.len(),
                "bits",
            ));
        }
    }
    values
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn digital_band_labels_preserve_pcs_blocks_and_separate_analog_support() {
        let mut report =
            crate::l3::capability_report::decode(&[5, 0x57, 0x84, 0x1a, 0x10, 0x50, 0, 0xe0, 0])
                .unwrap();
        report.supported_bands = 0x90;
        report.services.analog_800_speech = true;
        report.protocol_version = crate::l3::broadcast::ProtocolVersion::Pv5;
        report.options.extensions.frequency_bands = Some(1);
        let mut reported = ReportedCapabilities {
            report,
            received_superframe: 10,
        };
        let readings = measurements(Some(&reported));
        assert_eq!(
            readings
                .iter()
                .find(|metric| metric.name == "Reported DTC bands")
                .unwrap()
                .value
                .as_deref(),
            Some("1900 MHz D, 700 MHz C/D, IMT-2000 2 GHz")
        );
        reported.report.supported_bands = 0;
        reported.report.options.extensions.frequency_bands = None;
        let readings = measurements(Some(&reported));
        assert_eq!(
            readings
                .iter()
                .find(|metric| metric.name == "Reported DTC bands")
                .unwrap()
                .value
                .as_deref(),
            Some("None reported")
        );
        assert!(
            !measurements(None)
                .iter()
                .any(|metric| metric.name == "Reported DTC bands")
        );
    }
    #[test]
    fn subscriber_capabilities_report_partial_model_and_encoded_pfc_without_inventing_measurements()
    {
        let report =
            crate::l3::capability_report::decode(&[5, 0x57, 0x84, 0x1a, 0x10, 0x50, 0, 0xe0, 0])
                .unwrap();
        let reported = ReportedCapabilities {
            report,
            received_superframe: 10,
        };
        let readings = measurements(Some(&reported));
        for (name, expected) in [
            ("Handset model", "4-bit code 0x4"),
            ("Handset manufacturer code", "0x20"),
            ("Reported maximum PFC", "6"),
            ("Reported voice coder map", "0x01"),
        ] {
            assert!(
                readings
                    .iter()
                    .any(|metric| metric.name == name && metric.value.as_deref() == Some(expected)),
                "{readings:?}"
            );
        }
        assert!(
            readings
                .iter()
                .find(|metric| metric.name == "RSSI")
                .unwrap()
                .value
                .is_none()
        );
        assert!(
            measurements(None)
                .iter()
                .find(|metric| metric.name == "Handset model")
                .unwrap()
                .value
                .is_none()
        );
    }
}
