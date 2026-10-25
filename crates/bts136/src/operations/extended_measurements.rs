use super::proto;
use crate::l3::legacy_traffic::{
    ExtendedMeasurementTargets, ExtendedMeasurementType, Hyperband, MeasurementChannel,
};
pub(super) fn validate(request: &proto::SetMeasurementsRequest) -> Result<(), tonic::Status> {
    if let Some(extended) = &request.extended {
        if !request.enabled {
            return Err(tonic::Status::invalid_argument(
                "Extended measurements require enabled=true. Omit extended to stop reports",
            ));
        }
        targets(extended)?;
    }
    Ok(())
}
pub(super) fn targets(
    request: &proto::ExtendedMeasurements,
) -> Result<ExtendedMeasurementTargets, tonic::Status> {
    if request.channels.len() > crate::l3::legacy_traffic::MAX_EXTENDED_MEASUREMENT_CHANNELS {
        return Err(tonic::Status::invalid_argument(
            "Too many extended measurement channels",
        ));
    }
    let kind = match proto::ExtendedMeasurementKind::try_from(request.kind) {
        Ok(proto::ExtendedMeasurementKind::ExtendedCarrierInterference) => {
            ExtendedMeasurementType::CarrierInterference
        }
        Ok(proto::ExtendedMeasurementKind::ExtendedWordErrorRate) => {
            ExtendedMeasurementType::WordErrorRate
        }
        _ => {
            return Err(tonic::Status::invalid_argument(
                "Select carrier/interference or word error rate",
            ));
        }
    };
    let channels = request
        .channels
        .iter()
        .map(|entry| {
            let hyperband = match proto::MeasurementBand::try_from(entry.band) {
                Ok(proto::MeasurementBand::MeasurementCellular800) => Hyperband::Cellular800,
                Ok(proto::MeasurementBand::MeasurementPcs1900) => Hyperband::Pcs1900,
                Ok(proto::MeasurementBand::MeasurementCellular700) => Hyperband::Cellular700,
                Ok(proto::MeasurementBand::MeasurementImt2000) => Hyperband::Imt2000,
                Err(_) => return Err(tonic::Status::invalid_argument("Unknown measurement band")),
            };
            let channel = u16::try_from(entry.channel).map_err(|_| {
                tonic::Status::invalid_argument("Measurement channel is out of range")
            })?;
            Ok(MeasurementChannel { hyperband, channel })
        })
        .collect::<Result<Vec<_>, tonic::Status>>()?;
    ExtendedMeasurementTargets::try_new(kind, channels)
        .map_err(|error| tonic::Status::invalid_argument(error.to_string()))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> proto::ExtendedMeasurements {
        proto::ExtendedMeasurements {
            kind: proto::ExtendedMeasurementKind::ExtendedWordErrorRate as i32,
            channels: vec![],
        }
    }
    #[test]
    fn empty_and_repeated_targets_preserve_measurement_semantics() {
        assert!(targets(&request()).unwrap().channels().is_empty());
        let mut input = request();
        input.channels = vec![
            proto::MeasurementChannel {
                band: proto::MeasurementBand::MeasurementPcs1900 as i32,
                channel: 2047
            };
            22
        ];
        let parsed = targets(&input).unwrap();
        assert_eq!(parsed.channels().len(), 22);
        assert!(
            parsed
                .channels()
                .iter()
                .all(|entry| entry.hyperband == Hyperband::Pcs1900 && entry.channel == 2047)
        );
        assert_eq!(
            parsed.measurement_type(),
            ExtendedMeasurementType::WordErrorRate
        );
    }
    #[test]
    fn invalid_enum_bounds_and_disabled_extended_requests_are_rejected() {
        for kind in [0, 99] {
            assert_eq!(
                targets(&proto::ExtendedMeasurements { kind, ..request() })
                    .unwrap_err()
                    .code(),
                tonic::Code::InvalidArgument
            );
        }
        for (band, channel, count) in [(99, 1, 1), (0, 2048, 1), (0, u32::MAX, 1), (0, 1, 23)] {
            let input = proto::ExtendedMeasurements {
                channels: vec![proto::MeasurementChannel { band, channel }; count],
                ..request()
            };
            assert_eq!(
                targets(&input).unwrap_err().code(),
                tonic::Code::InvalidArgument
            );
        }
        assert!(
            validate(&proto::SetMeasurementsRequest {
                call_id: 1,
                party: 0,
                enabled: false,
                extended: Some(request())
            })
            .is_err()
        );
        assert!(
            validate(&proto::SetMeasurementsRequest {
                call_id: 1,
                party: 0,
                enabled: false,
                extended: None
            })
            .is_ok()
        );
    }
}
