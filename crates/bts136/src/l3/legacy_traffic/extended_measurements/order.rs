use super::{ExtendedChannelQualityReport, ExtendedMeasurementType, ExtendedQualityMetric};
use crate::l3::{
    broadcast::ProtocolVersion,
    legacy_traffic::{Hyperband, Reader, TrafficMessage, TrafficMessageError, put},
    relay::Direction,
};
const ORDER: u8 = 0xd9;
const ACK: u8 = 0xd4;
const MEASUREMENT_BITS: usize = 3;
const COUNT_BITS: usize = 5;
const HYPERBAND_BITS: usize = 2;
const CHANNEL_BITS: usize = 11;
const ACK_SUPPORT_BITS: usize = 1;
pub const MAX_EXTENDED_MEASUREMENT_CHANNELS: usize = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasurementChannel {
    pub hyperband: Hyperband,
    pub channel: u16,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedMeasurementTargets {
    measurement_type: ExtendedMeasurementType,
    channels: Vec<MeasurementChannel>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedMeasurementOrder {
    pub targets: ExtendedMeasurementTargets,
    pub mobile_ack_support: Option<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedServingMeasurement {
    pub ber_code: u8,
    pub metric: ExtendedQualityMetric,
    pub rssi_code: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedNeighborMeasurement {
    pub channel: MeasurementChannel,
    pub rssi_code: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedMeasurementResults {
    pub serving: Option<ExtendedServingMeasurement>,
    pub neighbors: Vec<ExtendedNeighborMeasurement>,
}
impl ExtendedMeasurementType {
    fn code(self) -> u64 {
        match self {
            Self::CarrierInterference => 1,
            Self::WordErrorRate => 2,
        }
    }
    fn decode(value: u64) -> Result<Self, TrafficMessageError> {
        match value {
            1 => Ok(Self::CarrierInterference),
            2 => Ok(Self::WordErrorRate),
            _ => Err(TrafficMessageError::Field("extended_measurement_type")),
        }
    }
}
impl ExtendedMeasurementTargets {
    // TIA-136-133-E §§5.1.2.3.111/5.1.2.4.37 define a count and ordered band2/channel11 entries.
    pub fn try_new(
        measurement_type: ExtendedMeasurementType,
        channels: Vec<MeasurementChannel>,
    ) -> Result<Self, TrafficMessageError> {
        if channels.len() > MAX_EXTENDED_MEASUREMENT_CHANNELS {
            return Err(TrafficMessageError::Field("extended_measurement_count"));
        }
        if channels
            .iter()
            .any(|value| u64::from(value.channel) >= 1 << CHANNEL_BITS)
        {
            return Err(TrafficMessageError::Field("extended_measurement_channel"));
        }
        Ok(Self {
            measurement_type,
            channels,
        })
    }
    pub fn measurement_type(&self) -> ExtendedMeasurementType {
        self.measurement_type
    }
    pub fn channels(&self) -> &[MeasurementChannel] {
        &self.channels
    }
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), TrafficMessageError> {
        if version.code() < ProtocolVersion::Pv2.code() {
            return Err(TrafficMessageError::Field("extended_measurement_version"));
        }
        Ok(())
    }
    // Association uses this owned order snapshot. Exchange ownership and response timing are separate checks.
    pub fn resolve(
        &self,
        report: &ExtendedChannelQualityReport,
    ) -> Result<ExtendedMeasurementResults, TrafficMessageError> {
        report.validate_fields()?;
        let serving = match report {
            ExtendedChannelQualityReport::First { current, .. } => {
                Some(ExtendedServingMeasurement {
                    ber_code: current.ber_code,
                    metric: current.metric(self.measurement_type),
                    rssi_code: current.rssi_code,
                })
            }
            _ => None,
        };
        let (start, codes) = report.neighbors();
        let mut neighbors = Vec::new();
        for (offset, code) in codes.iter().enumerate() {
            if let Some(channel) = self.channels.get(start + offset) {
                neighbors.push(ExtendedNeighborMeasurement {
                    channel: *channel,
                    rssi_code: *code,
                });
            } else if *code != 0 {
                return Err(TrafficMessageError::Field("unused_extended_measurement"));
            }
        }
        Ok(ExtendedMeasurementResults { serving, neighbors })
    }
    pub(in crate::l3::legacy_traffic) fn encode_fields(&self, bits: &mut Vec<bool>) {
        put(bits, self.measurement_type.code(), MEASUREMENT_BITS);
        put(bits, self.channels.len() as u64, COUNT_BITS);
        for value in &self.channels {
            put(bits, value.hyperband as u64, HYPERBAND_BITS);
            put(bits, u64::from(value.channel), CHANNEL_BITS);
        }
    }
}
impl ExtendedMeasurementOrder {
    // TIA-136-133-E §9.1.2.2.32 makes Mobile Ack Support mandatory starting at PV6.
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), TrafficMessageError> {
        self.targets.validate_version(version)?;
        if self.mobile_ack_support.is_some() != (version.code() >= ProtocolVersion::Pv6.code()) {
            return Err(TrafficMessageError::Field(
                "extended_measurement_ack_support_version",
            ));
        }
        Ok(())
    }
    pub fn encode_for_version(
        &self,
        version: ProtocolVersion,
    ) -> Result<Vec<bool>, TrafficMessageError> {
        self.validate_version(version)?;
        TrafficMessage::ExtendedMeasurementOrder(self.clone()).encode()
    }
    pub(in crate::l3::legacy_traffic) fn encode_fields(&self, bits: &mut Vec<bool>) {
        self.targets.encode_fields(bits);
        if let Some(support) = self.mobile_ack_support {
            put(bits, u64::from(support), ACK_SUPPORT_BITS);
        }
    }
}
pub(in crate::l3::legacy_traffic) fn message_type(ack: bool) -> u8 {
    if ack { ACK } else { ORDER }
}
pub(in crate::l3::legacy_traffic) fn decode(
    kind: u8,
    reader: &mut Reader<'_>,
    direction: Direction,
    version: Option<ProtocolVersion>,
) -> Result<Option<TrafficMessage>, TrafficMessageError> {
    if !matches!(kind, ORDER | ACK) {
        return Ok(None);
    }
    if (kind == ORDER) != (direction == Direction::Forward) {
        return Err(TrafficMessageError::Field("extended_measurement_direction"));
    }
    if kind == ORDER && version.is_none() {
        return Err(TrafficMessageError::Field(
            "extended_order_version_required",
        ));
    }
    let measurement_type = ExtendedMeasurementType::decode(reader.take(MEASUREMENT_BITS)?)?;
    let count = reader.take(COUNT_BITS)? as usize;
    if count > MAX_EXTENDED_MEASUREMENT_CHANNELS {
        return Err(TrafficMessageError::Field("extended_measurement_count"));
    }
    let mut channels = Vec::with_capacity(count);
    for _ in 0..count {
        channels.push(MeasurementChannel {
            hyperband: Hyperband::try_from(reader.take(HYPERBAND_BITS)? as u8)?,
            channel: reader.take(CHANNEL_BITS)? as u16,
        });
    }
    let targets = ExtendedMeasurementTargets::try_new(measurement_type, channels)?;
    if let Some(version) = version {
        targets.validate_version(version)?;
    }
    if kind == ACK {
        return Ok(Some(TrafficMessage::ExtendedMeasurementAck(targets)));
    }
    let mobile_ack_support =
        if version.is_some_and(|value| value.code() >= ProtocolVersion::Pv6.code()) {
            Some(reader.take(ACK_SUPPORT_BITS)? != 0)
        } else {
            None
        };
    Ok(Some(TrafficMessage::ExtendedMeasurementOrder(
        ExtendedMeasurementOrder {
            targets,
            mobile_ack_support,
        },
    )))
}
#[cfg(test)]
mod tests;
