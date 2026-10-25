use super::{NeighborGroup, ProtocolVersion, Reader, TrafficMessageError, put};
use crate::l3::relay::Direction;
pub(super) mod order;
pub use order::{
    ExtendedMeasurementOrder, ExtendedMeasurementResults, ExtendedMeasurementTargets,
    ExtendedNeighborMeasurement, ExtendedServingMeasurement, MAX_EXTENDED_MEASUREMENT_CHANNELS,
    MeasurementChannel,
};
const FIRST: u8 = 0x53;
const SECOND: u8 = 0xa8;
const THIRD: u8 = 0xdf;
const FOURTH: u8 = 0xa5;
const QUALITY_BITS: usize = 4;
const RSSI_BITS: usize = 6;
const FIRST_NEIGHBORS: usize = 4;
const LATER_NEIGHBORS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtendedMeasurementType {
    CarrierInterference,
    WordErrorRate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtendedQualityMetric {
    CarrierInterference(u8),
    WordErrorRate(u8),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedCurrentQuality {
    pub ber_code: u8,
    // TIA-136-133-E §5.1.2.2.30 requires the Extended Measurement Order to distinguish CIR from WER.
    pub measurement_code: u8,
    pub rssi_code: u8,
}
impl ExtendedCurrentQuality {
    pub fn metric(self, kind: ExtendedMeasurementType) -> ExtendedQualityMetric {
        match kind {
            ExtendedMeasurementType::CarrierInterference => {
                ExtendedQualityMetric::CarrierInterference(self.measurement_code)
            }
            ExtendedMeasurementType::WordErrorRate => {
                ExtendedQualityMetric::WordErrorRate(self.measurement_code)
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtendedChannelQualityReport {
    First {
        current: ExtendedCurrentQuality,
        neighbor_rssi_codes: [u8; FIRST_NEIGHBORS],
    },
    Later {
        group: NeighborGroup,
        neighbor_rssi_codes: [u8; LATER_NEIGHBORS],
    },
}
impl ExtendedChannelQualityReport {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), TrafficMessageError> {
        if version.code() < ProtocolVersion::Pv2.code() {
            return Err(TrafficMessageError::Field("extended_measurement_version"));
        }
        Ok(())
    }
    // TIA-136-133-E §§5.1.2.2.30–33 identify positions in the original order, not RF channel numbers.
    pub fn neighbors(&self) -> (usize, &[u8]) {
        match self {
            Self::First {
                neighbor_rssi_codes,
                ..
            } => (0, neighbor_rssi_codes),
            Self::Later {
                group,
                neighbor_rssi_codes,
            } => (
                match group {
                    NeighborGroup::Second => FIRST_NEIGHBORS,
                    NeighborGroup::Third => FIRST_NEIGHBORS + LATER_NEIGHBORS,
                    NeighborGroup::Fourth => FIRST_NEIGHBORS + 2 * LATER_NEIGHBORS,
                },
                neighbor_rssi_codes,
            ),
        }
    }
    pub(super) fn message_type(&self) -> u8 {
        match self {
            Self::First { .. } => FIRST,
            Self::Later { group, .. } => match group {
                NeighborGroup::Second => SECOND,
                NeighborGroup::Third => THIRD,
                NeighborGroup::Fourth => FOURTH,
            },
        }
    }
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        self.validate_fields()?;
        if let Self::First { current, .. } = self {
            put(bits, u64::from(current.ber_code), QUALITY_BITS);
            put(bits, u64::from(current.measurement_code), QUALITY_BITS);
            put(bits, u64::from(current.rssi_code), RSSI_BITS);
        }
        for code in self.neighbors().1 {
            put(bits, u64::from(*code), RSSI_BITS);
        }
        Ok(())
    }
    fn validate_fields(&self) -> Result<(), TrafficMessageError> {
        if let Self::First { current, .. } = self
            && (current.ber_code >= 1 << QUALITY_BITS
                || current.measurement_code >= 1 << QUALITY_BITS
                || current.rssi_code >= 1 << RSSI_BITS)
        {
            return Err(TrafficMessageError::Field("extended_current_quality"));
        }
        for code in self.neighbors().1 {
            if *code >= 1 << RSSI_BITS {
                return Err(TrafficMessageError::Field("extended_neighbor_rssi"));
            }
        }
        Ok(())
    }
}
// TIA-136-133-E §§5.1.2.2.30–33 define 48/46 bits without Remaining Length.
pub(super) fn decode(
    kind: u8,
    reader: &mut Reader<'_>,
    direction: Direction,
) -> Result<Option<ExtendedChannelQualityReport>, TrafficMessageError> {
    if !matches!(kind, FIRST | SECOND | THIRD | FOURTH) {
        return Ok(None);
    }
    if direction != Direction::Reverse {
        return Err(TrafficMessageError::Field("extended_quality_direction"));
    }
    if kind == FIRST {
        let current = ExtendedCurrentQuality {
            ber_code: reader.take(QUALITY_BITS)? as u8,
            measurement_code: reader.take(QUALITY_BITS)? as u8,
            rssi_code: reader.take(RSSI_BITS)? as u8,
        };
        let mut neighbor_rssi_codes = [0; FIRST_NEIGHBORS];
        for code in &mut neighbor_rssi_codes {
            *code = reader.take(RSSI_BITS)? as u8;
        }
        return Ok(Some(ExtendedChannelQualityReport::First {
            current,
            neighbor_rssi_codes,
        }));
    }
    let mut neighbor_rssi_codes = [0; LATER_NEIGHBORS];
    for code in &mut neighbor_rssi_codes {
        *code = reader.take(RSSI_BITS)? as u8;
    }
    Ok(Some(ExtendedChannelQualityReport::Later {
        group: match kind {
            SECOND => NeighborGroup::Second,
            THIRD => NeighborGroup::Third,
            _ => NeighborGroup::Fourth,
        },
        neighbor_rssi_codes,
    }))
}
#[cfg(test)]
mod tests;
