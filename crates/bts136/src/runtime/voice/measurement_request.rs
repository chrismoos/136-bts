use crate::l3::{
    broadcast::ProtocolVersion,
    legacy_traffic::{ExtendedMeasurementOrder, ExtendedMeasurementTargets, PhysicalControlFields},
};
use std::{error::Error as StdError, fmt};
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Error {
    Protocol,
    Dtx,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Protocol => "extended measurements require an assigned PV2 or later bearer",
            Self::Dtx => "extended measurement requests require confirmed DTX disabled until DTX reporting is implemented",
        })
    }
}
impl StdError for Error {}
pub(super) fn order(
    targets: ExtendedMeasurementTargets,
    version: ProtocolVersion,
    controls: &PhysicalControlFields,
) -> Result<ExtendedMeasurementOrder, Error> {
    if version.code() < ProtocolVersion::Pv2.code() {
        return Err(Error::Protocol);
    }
    if controls.control.dtx_allowed != Some(false) {
        return Err(Error::Dtx);
    }
    Ok(ExtendedMeasurementOrder {
        targets,
        mobile_ack_support: (version.code() >= ProtocolVersion::Pv6.code()).then_some(true),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::legacy_traffic::ExtendedMeasurementType;
    #[test]
    fn protocol_and_confirmed_dtx_state_control_admission_and_mandatory_fields() {
        let targets =
            ExtendedMeasurementTargets::try_new(ExtendedMeasurementType::WordErrorRate, vec![])
                .unwrap();
        let mut controls = PhysicalControlFields::default();
        for allowed in [None, Some(true)] {
            controls.control.dtx_allowed = allowed;
            assert_eq!(
                order(targets.clone(), ProtocolVersion::Pv2, &controls),
                Err(Error::Dtx)
            );
        }
        controls.control.dtx_allowed = Some(false);
        assert_eq!(
            order(targets.clone(), ProtocolVersion::Is136RevA, &controls),
            Err(Error::Protocol)
        );
        let pv2 = order(targets.clone(), ProtocolVersion::Pv2, &controls).unwrap();
        assert_eq!(pv2.mobile_ack_support, None);
        assert!(pv2.encode_for_version(ProtocolVersion::Pv2).is_ok());
        let pv6 = order(targets, ProtocolVersion::Pv6, &controls).unwrap();
        assert_eq!(pv6.mobile_ack_support, Some(true));
        assert!(pv6.encode_for_version(ProtocolVersion::Pv6).is_ok());
    }
}
