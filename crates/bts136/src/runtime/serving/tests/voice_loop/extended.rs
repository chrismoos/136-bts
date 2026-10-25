use super::*;
use crate::l3::legacy_traffic::{
    ExtendedChannelQualityReport, ExtendedCurrentQuality, ExtendedMeasurementOrder, NeighborGroup,
};
pub(super) fn respond(leg: &mut Leg, order: ExtendedMeasurementOrder) -> Result<(), HalError> {
    order.validate_version(ProtocolVersion::Pv2).map_err(hal)?;
    if order.targets.channels().len() != 22 {
        return Err("unexpected extended list length".into());
    }
    leg.tx
        .enqueue(&TrafficMessage::ExtendedMeasurementAck(order.targets))
        .map_err(hal)?;
    leg.tx
        .enqueue(&TrafficMessage::ExtendedChannelQuality(
            ExtendedChannelQualityReport::First {
                current: ExtendedCurrentQuality {
                    ber_code: 4,
                    measurement_code: 8,
                    rssi_code: 41,
                },
                neighbor_rssi_codes: [1, 2, 3, 4],
            },
        ))
        .map_err(hal)?;
    for (group, start) in [
        (NeighborGroup::Second, 5),
        (NeighborGroup::Third, 11),
        (NeighborGroup::Fourth, 17),
    ] {
        let report = TrafficMessage::ExtendedChannelQuality(ExtendedChannelQualityReport::Later {
            group,
            neighbor_rssi_codes: std::array::from_fn(|index| start + index as u8),
        });
        leg.sacch_tx
            .enqueue(&encode_signaling(&report).map_err(hal)?)
            .map_err(hal)?;
    }
    Ok(())
}
pub(super) fn operator(
    hub: crate::operations::Hub,
    shutdown: Arc<AtomicBool>,
    gate: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut requested = false;
        while !shutdown.load(Ordering::Relaxed) {
            let snapshot = hub.snapshot();
            let caller = snapshot
                .calls
                .iter()
                .filter(|call| call.phase() == crate::operations::proto::CallPhase::Connected)
                .flat_map(|call| &call.legs)
                .find(|leg| leg.party == "Caller");
            if let Some(caller) = caller {
                if !requested
                    && caller
                        .measurements
                        .iter()
                        .any(|metric| metric.name == "Phone reported downlink RSSI")
                    && caller.measurements.iter().any(|metric| {
                        metric.name == "Physical control"
                            && metric.value.as_deref() == Some("Acknowledged")
                    })
                {
                    let (response, result) = tokio::sync::oneshot::channel();
                    hub.commands.send(crate::operations::PendingCommand {
                        command: crate::operations::Command::Measurements(crate::operations::proto::SetMeasurementsRequest {
                            call_id: FIRST_CALL_ID, party: crate::operations::proto::CallParty::Caller as i32, enabled: true,
                            extended: Some(crate::operations::proto::ExtendedMeasurements {
                                kind: crate::operations::proto::ExtendedMeasurementKind::ExtendedWordErrorRate as i32,
                                channels: (1..=22).map(|channel| crate::operations::proto::MeasurementChannel {
                                    band: crate::operations::proto::MeasurementBand::MeasurementCellular800 as i32, channel,
                                }).collect(),
                            }),
                        }), response,
                    }).unwrap();
                    result.blocking_recv().unwrap().unwrap();
                    requested = true;
                }
                let has = |name: &str, value: &str| {
                    caller
                        .measurements
                        .iter()
                        .any(|metric| metric.name == name && metric.value.as_deref() == Some(value))
                };
                if requested
                    && has("Extended serving BER", "1.0–<1.5")
                    && has("Extended serving WER", ">14–≤18")
                    && has("Extended serving RSSI", "≥-31")
                    && has("Extended neighbor 22 Cellular800 channel 22 RSSI", "-69")
                    && has("Measurement request", "Acknowledged")
                {
                    gate.store(true, Ordering::Relaxed);
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("cell stopped before owned extended readings were verified");
    })
}
#[test]
#[ignore = "real-time I/Q call, run alone with --ignored"]
fn pv2_call_receives_owned_extended_facch_and_sacch_readings() {
    run_call_with_extended(
        Scenario::Connected {
            hangup: CALLEE,
            sms: None,
            tones: false,
            information: false,
            services: false,
        },
        ChannelModel::SinglePath,
        false,
        AlertFeedback::Normal,
        paging::ReplyOn::FirstComplete,
        ReleaseReason::Normal,
        TestFeatures {
            extended: true,
            neighbors: false,
        },
    );
}
