use super::*;
pub(super) fn append_metrics(
    readings: &mut Vec<crate::operations::proto::Metric>,
    measurements: &LegMeasurements,
    now_ms: u64,
) {
    use crate::operations::metric;
    let Some(at) = measurements.status_report_ms else {
        return;
    };
    readings.push(metric(
        "Last status report age",
        now_ms.saturating_sub(at),
        "ms",
    ));
    let status = &measurements.status;
    if let Some(values) = &status.serial_numbers {
        readings.push(metric(
            "Phone ESN",
            values
                .iter()
                .map(|value| format!("{value:08X}"))
                .collect::<Vec<_>>()
                .join(", "),
            "",
        ));
    }
    if let Some(values) = &status.call_modes {
        readings.push(metric(
            "Phone preferred call modes",
            values
                .iter()
                .map(|value| format!("0x{value:02X}"))
                .collect::<Vec<_>>()
                .join(", "),
            "",
        ));
    }
    if let Some(values) = &status.terminals {
        for (index, terminal) in values.iter().enumerate() {
            readings.extend([
                metric(
                    format!("Terminal {} protocol", index + 1),
                    format!("{:?}", terminal.protocol_version),
                    "",
                ),
                metric(
                    format!("Terminal {} manufacturer", index + 1),
                    terminal.manufacturer,
                    "",
                ),
                metric(format!("Terminal {} model", index + 1), terminal.model, ""),
                metric(
                    format!("Terminal {} firmware", index + 1),
                    terminal.firmware,
                    "",
                ),
                metric(
                    format!("Terminal {} access overload class", index + 1),
                    terminal.access_overload_class,
                    "",
                ),
                metric(
                    format!("Terminal {} local control", index + 1),
                    terminal.local_control,
                    "",
                ),
                metric(
                    format!("Terminal {} registration bit", index + 1),
                    terminal.registration,
                    "",
                ),
            ]);
        }
    }
    for (name, values) in [
        ("Phone reported privacy", &status.privacy_modes),
        (
            "Phone reported message encryption",
            &status.encryption_modes,
        ),
    ] {
        if let Some(values) = values {
            readings.push(metric(name, format!("{values:?}"), ""));
        }
    }
    if let Some(values) = &status.tasks {
        readings.push(metric("Phone reported tasks", format!("{values:?}"), ""));
    }
    if let Some(value) = status.serial_expansion {
        readings.push(metric("Phone ESN expansion", format!("{value:06X}"), ""));
    }
    if let Some(values) = &status.equipment_identities {
        readings.push(metric(
            "Phone MEID",
            values
                .iter()
                .map(|value| format!("{value:014X}"))
                .collect::<Vec<_>>()
                .join(", "),
            "",
        ));
    }
}
