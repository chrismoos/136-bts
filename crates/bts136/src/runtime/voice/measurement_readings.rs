use crate::{
    core::call_session::ExtendedMeasurementSnapshot,
    l3::legacy_traffic::ExtendedQualityMetric,
    operations::{metric, proto::Metric, unavailable},
};
// TIA-136-133-E §2.4.1 Tables 2, 4, 6 and 7 define quantized handset estimates.
const BER_RANGES: [&str; 16] = [
    "<0.01",
    "0.01–<0.1",
    "0.1–<0.5",
    "0.5–<1.0",
    "1.0–<1.5",
    "1.5–<2.0",
    "2.0–<2.5",
    "2.5–<3.0",
    "3.0–<3.5",
    "3.5–<4.0",
    "4.0–<4.5",
    "4.5–<5.0",
    "5.0–<6.0",
    "6.0–<7.0",
    "7.0–<8.0",
    ">8.0",
];
const CIR_RANGES: [&str; 16] = [
    "<8.0",
    "8.0–<9.0",
    "9.0–<10.0",
    "10.0–<11.0",
    "11.0–<12.0",
    "12.0–<13.0",
    "13.0–<14.0",
    "14.0–<15.0",
    "15.0–<16.0",
    "16.0–<17.0",
    "17.0–<18.0",
    "18.0–<19.0",
    "19.0–<20.0",
    "20.0–<21.0",
    "21.0–<22.0",
    "≥22.0",
];
const WER_RANGES: [&str; 16] = [
    "0",
    ">0–≤2",
    ">2–≤4",
    ">4–≤6",
    ">6–≤8",
    ">8–≤10",
    ">10–≤12",
    ">12–≤14",
    ">14–≤18",
    ">18–≤20",
    ">20–≤25",
    ">25–≤30",
    ">30–≤40",
    ">40–≤50",
    ">50–≤60",
    ">60",
];
const RSSI_BASE_DBM: i16 = -113;
const RSSI_STEP_DB: i16 = 2;
const RSSI_MAX_CODE: u8 = 41;
pub(super) fn append_metrics(
    readings: &mut Vec<Metric>,
    snapshot: Option<&ExtendedMeasurementSnapshot>,
    now_ms: u64,
) {
    let Some(snapshot) = snapshot else { return };
    readings.push(metric(
        "Extended measurement type",
        format!("{:?}", snapshot.targets.measurement_type()),
        "",
    ));
    if let Some(at) = snapshot.received_monotonic_ms {
        readings.push(metric(
            "Latest extended report age",
            now_ms.saturating_sub(at),
            "ms",
        ));
    }
    if let Some(serving) = snapshot.serving {
        readings.push(range(
            "Extended serving BER",
            serving.ber_code,
            &BER_RANGES,
            "%",
        ));
        readings.push(match serving.metric {
            ExtendedQualityMetric::CarrierInterference(code) => {
                range("Extended serving C/I", code, &CIR_RANGES, "dB")
            }
            ExtendedQualityMetric::WordErrorRate(code) => {
                range("Extended serving WER", code, &WER_RANGES, "%")
            }
        });
        readings.push(rssi("Extended serving RSSI", Some(serving.rssi_code)));
    } else {
        readings.push(unavailable(
            "Extended serving measurements",
            "No current-channel report received",
        ));
    }
    for (index, channel) in snapshot.targets.channels().iter().enumerate() {
        let name = format!(
            "Extended neighbor {} {:?} channel {} RSSI",
            index + 1,
            channel.hyperband,
            channel.channel
        );
        readings.push(rssi(
            &name,
            snapshot.neighbor_rssi_codes.get(index).copied().flatten(),
        ));
    }
}
fn range(name: &str, code: u8, ranges: &[&str], unit: &str) -> Metric {
    match ranges.get(usize::from(code)) {
        Some(value) => metric(name, value, unit),
        None => unavailable(name, &format!("Invalid measurement code {code}")),
    }
}
fn rssi(name: &str, code: Option<u8>) -> Metric {
    let Some(code) = code else {
        return unavailable(name, "No report received for this entry");
    };
    let value = RSSI_BASE_DBM + RSSI_STEP_DB * i16::from(code);
    match code {
        0 => metric(name, format!("≤{value} (code 0)"), "dBm"),
        RSSI_MAX_CODE => metric(name, format!("≥{value}"), "dBm"),
        1..RSSI_MAX_CODE => metric(name, value, "dBm"),
        _ => unavailable(name, &format!("Reserved RSSI code {code}")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::legacy_traffic::{
        ExtendedMeasurementTargets, ExtendedMeasurementType, ExtendedServingMeasurement, Hyperband,
        MeasurementChannel,
    };
    #[test]
    fn extended_table_boundaries_and_reserved_rssi_do_not_use_baseline_scaling() {
        assert_eq!(
            rssi("RSSI", Some(0)).value.as_deref(),
            Some("≤-113 (code 0)")
        );
        assert_eq!(rssi("RSSI", Some(1)).value.as_deref(), Some("-111"));
        assert_eq!(rssi("RSSI", Some(31)).value.as_deref(), Some("-51"));
        assert_eq!(rssi("RSSI", Some(40)).value.as_deref(), Some("-33"));
        assert_eq!(rssi("RSSI", Some(41)).value.as_deref(), Some("≥-31"));
        for code in 42..=63 {
            let reading = rssi("RSSI", Some(code));
            assert!(reading.value.is_none());
            assert_eq!(
                reading.unavailable_reason,
                format!("Reserved RSSI code {code}")
            );
        }
        assert_ne!(rssi("RSSI", None), rssi("RSSI", Some(0)));
        assert_eq!(
            range("BER", 4, &BER_RANGES, "%").value.as_deref(),
            Some("1.0–<1.5")
        );
        assert_eq!(
            range("BER", 14, &BER_RANGES, "%").value.as_deref(),
            Some("7.0–<8.0")
        );
        assert_eq!(
            range("C/I", 0, &CIR_RANGES, "dB").value.as_deref(),
            Some("<8.0")
        );
        assert_eq!(
            range("C/I", 15, &CIR_RANGES, "dB").value.as_deref(),
            Some("≥22.0")
        );
        assert_eq!(
            range("WER", 0, &WER_RANGES, "%").value.as_deref(),
            Some("0")
        );
        assert_eq!(
            range("WER", 8, &WER_RANGES, "%").value.as_deref(),
            Some(">14–≤18")
        );
        assert_eq!(
            range("WER", 15, &WER_RANGES, "%").value.as_deref(),
            Some(">60")
        );
    }
    #[test]
    fn readings_preserve_order_duplicates_missing_values_and_the_requested_metric() {
        let mut snapshot = ExtendedMeasurementSnapshot {
            targets: ExtendedMeasurementTargets::try_new(
                ExtendedMeasurementType::WordErrorRate,
                vec![
                    MeasurementChannel {
                        hyperband: Hyperband::Pcs1900,
                        channel: 1,
                    },
                    MeasurementChannel {
                        hyperband: Hyperband::Cellular800,
                        channel: 1,
                    },
                    MeasurementChannel {
                        hyperband: Hyperband::Pcs1900,
                        channel: 1,
                    },
                ],
            )
            .unwrap(),
            serving: Some(ExtendedServingMeasurement {
                ber_code: 5,
                metric: ExtendedQualityMetric::WordErrorRate(8),
                rssi_code: 41,
            }),
            neighbor_rssi_codes: vec![Some(0), None, Some(42)],
            received_monotonic_ms: Some(100),
        };
        let mut readings = vec![];
        append_metrics(&mut readings, Some(&snapshot), 140);
        assert_eq!(
            readings
                .iter()
                .find(|m| m.name == "Latest extended report age")
                .unwrap()
                .value
                .as_deref(),
            Some("40")
        );
        assert!(
            readings
                .iter()
                .any(|m| m.name == "Extended serving WER" && m.unit == "%")
        );
        assert!(!readings.iter().any(|m| m.name == "Extended serving C/I"));
        let neighbors: Vec<_> = readings
            .iter()
            .filter(|m| m.name.starts_with("Extended neighbor"))
            .collect();
        assert_eq!(neighbors.len(), 3);
        assert_eq!(
            neighbors[0].name,
            "Extended neighbor 1 Pcs1900 channel 1 RSSI"
        );
        assert_eq!(
            neighbors[1].name,
            "Extended neighbor 2 Cellular800 channel 1 RSSI"
        );
        assert_eq!(
            neighbors[2].name,
            "Extended neighbor 3 Pcs1900 channel 1 RSSI"
        );
        assert!(neighbors[0].value.is_some());
        assert_eq!(
            neighbors[1].unavailable_reason,
            "No report received for this entry"
        );
        assert_eq!(neighbors[2].unavailable_reason, "Reserved RSSI code 42");
        snapshot.targets = ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::CarrierInterference,
            snapshot.targets.channels().to_vec(),
        )
        .unwrap();
        snapshot.serving.as_mut().unwrap().metric = ExtendedQualityMetric::CarrierInterference(15);
        readings.clear();
        append_metrics(&mut readings, Some(&snapshot), 90);
        assert!(
            readings
                .iter()
                .any(|m| m.name == "Extended serving C/I" && m.unit == "dB")
        );
        assert!(!readings.iter().any(|m| m.name == "Extended serving WER"));
        readings.clear();
        append_metrics(&mut readings, None, 150);
        assert!(readings.is_empty());
    }
}
