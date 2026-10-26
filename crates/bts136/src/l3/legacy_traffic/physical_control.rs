use super::*;
use crate::l3::designation::TimeAlignment;

const PARAMETER_BITS: usize = 4;
const COUNT_BITS: usize = 6;
const POWER: u64 = 2;
const TIMING: u64 = 3;
const DTX_ALLOWED: u64 = 4;
const DIC: u64 = 5;
const DTX_MODE: u64 = 6;
const EXTENSION: u64 = 7;
const DPC: u64 = 1;
const FPC: u64 = 2;
const POWER_BITS: usize = 4;
const TIMING_BITS: usize = 5;
const DTX_MODE_BITS: usize = 6;
const DPC_BITS: usize = 3;
const FPC_BITS: usize = 2;
const FLAG_BITS: usize = 1;
pub const MAX_POWER_LEVEL: u8 = 10;
pub const MAX_ALIGNMENT_UNITS: u8 = 30;
const MAINTAIN_ALIGNMENT: u8 = 31;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysicalControlRequest {
    pub power_level: Option<u8>,
    /// TIA-136-133-E §5.1.2.4.70 uses absolute half-symbol offsets from SOR.
    pub time_alignment: Option<TimeAlignment>,
    pub dtx_allowed: Option<bool>,
    pub delay_compensation: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalControlFields {
    pub control: PhysicalControlRequest,
    pub dtx_mode: Option<u8>,
    pub downlink_power_control: Option<u8>,
    pub fast_power_control: Option<u8>,
    pub unparsed_bits: Vec<bool>,
}

impl PhysicalControlRequest {
    pub fn validate(self) -> Result<(), TrafficMessageError> {
        if self
            .power_level
            .is_some_and(|value| value > MAX_POWER_LEVEL)
        {
            return Err(TrafficMessageError::Field("power_level"));
        }
        if matches!(self.time_alignment, Some(TimeAlignment::OffsetUnits(value)) if value > MAX_ALIGNMENT_UNITS)
        {
            return Err(TrafficMessageError::Field("time_alignment"));
        }
        Ok(())
    }

    pub fn is_empty(self) -> bool {
        self == Self::default()
    }

    // TIA-136-133-E §5.1.2.2.7 requires all requested IEs and the resulting absolute alignment.
    pub(crate) fn answered_by(self, response: &PhysicalControlFields) -> bool {
        let actual = response.control;
        self.power_level.is_none_or(|_| {
            actual
                .power_level
                .is_some_and(|value| value <= MAX_POWER_LEVEL)
        }) && self
            .time_alignment
            .is_none_or(|requested| match (requested, actual.time_alignment) {
                (TimeAlignment::Maintain, Some(TimeAlignment::OffsetUnits(units))) => {
                    units <= MAX_ALIGNMENT_UNITS
                }
                (TimeAlignment::Maintain, Some(TimeAlignment::Maintain)) => true,
                (
                    TimeAlignment::OffsetUnits(requested),
                    Some(TimeAlignment::OffsetUnits(actual)),
                ) => requested == actual,
                _ => false,
            })
            && self
                .dtx_allowed
                .is_none_or(|requested| actual.dtx_allowed == Some(requested))
            && self
                .delay_compensation
                .is_none_or(|requested| actual.delay_compensation == Some(requested))
    }
}

fn write(
    bits: &mut Vec<bool>,
    parameter: u64,
    auxiliary: Option<u64>,
    width: usize,
    value: u64,
) -> Result<(), TrafficMessageError> {
    if value >= 1 << width {
        return Err(TrafficMessageError::Field("physical_control_value"));
    }
    put(bits, parameter, PARAMETER_BITS);
    if let Some(auxiliary) = auxiliary {
        put(bits, auxiliary, PARAMETER_BITS);
    }
    put(bits, 1, COUNT_BITS);
    put(bits, value, width);
    Ok(())
}

impl PhysicalControlFields {
    pub(crate) fn update(&mut self, report: &Self) {
        if report.control.power_level.is_some() {
            self.control.power_level = report.control.power_level;
        }
        if report
            .control
            .time_alignment
            .is_some_and(|value| value != TimeAlignment::Maintain)
        {
            self.control.time_alignment = report.control.time_alignment;
        }
        if report.control.delay_compensation.is_some() {
            self.control.delay_compensation = report.control.delay_compensation;
        }
        if report.control.dtx_allowed.is_some() {
            self.control.dtx_allowed = report.control.dtx_allowed;
            self.dtx_mode = None;
        }
        if report.dtx_mode.is_some() {
            self.dtx_mode = report.dtx_mode;
            self.control.dtx_allowed = None;
        }
        if report.downlink_power_control.is_some() {
            self.downlink_power_control = report.downlink_power_control;
        }
        if report.fast_power_control.is_some() {
            self.fast_power_control = report.fast_power_control;
        }
        self.unparsed_bits.clone_from(&report.unparsed_bits);
    }
    pub(super) fn encode_octets(&self) -> Result<Vec<u8>, TrafficMessageError> {
        self.control.validate()?;
        if self.control.dtx_allowed.is_some() && self.dtx_mode.is_some() {
            return Err(TrafficMessageError::Field("conflicting_dtx_control"));
        }
        let mut bits = Vec::new();
        if let Some(value) = self.control.power_level {
            write(&mut bits, POWER, None, POWER_BITS, u64::from(value))?;
        }
        if let Some(value) = self.control.time_alignment {
            let value = match value {
                TimeAlignment::Maintain => MAINTAIN_ALIGNMENT,
                TimeAlignment::OffsetUnits(value) => value,
            };
            write(&mut bits, TIMING, None, TIMING_BITS, u64::from(value))?;
        }
        if let Some(value) = self.control.dtx_allowed {
            write(&mut bits, DTX_ALLOWED, None, FLAG_BITS, u64::from(value))?;
        }
        if let Some(value) = self.control.delay_compensation {
            write(&mut bits, DIC, None, FLAG_BITS, u64::from(value))?;
        }
        if let Some(value) = self.dtx_mode {
            write(&mut bits, DTX_MODE, None, DTX_MODE_BITS, u64::from(value))?;
        }
        if let Some(value) = self.downlink_power_control {
            write(&mut bits, EXTENSION, Some(DPC), DPC_BITS, u64::from(value))?;
        }
        if let Some(value) = self.fast_power_control {
            write(&mut bits, EXTENSION, Some(FPC), FPC_BITS, u64::from(value))?;
        }
        bits.extend_from_slice(&self.unparsed_bits);
        bits.resize(bits.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
        if bits.len() > MAX_OPTIONAL_OCTETS * OCTET_BITS {
            return Err(TrafficMessageError::OptionalLength);
        }
        Ok(bits
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|octet| {
                octet
                    .iter()
                    .fold(0, |value, bit| value * 2 + u8::from(*bit))
            })
            .collect())
    }
}

// TIA-136-133-E §§5.1.2.2.7 and 9.1.2.2.6 use counted optional fields and octet padding.
pub(super) fn decode(octets: &[u8]) -> Result<PhysicalControlFields, TrafficMessageError> {
    let bits: Vec<_> = octets
        .iter()
        .flat_map(|octet| {
            (0..OCTET_BITS)
                .rev()
                .map(move |bit| octet & (1 << bit) != 0)
        })
        .collect();
    let mut reader = Reader {
        bits: &bits,
        offset: 0,
    };
    let mut fields = PhysicalControlFields::default();
    let mut last_rank = 0;
    while reader.offset < bits.len() {
        if bits.len() - reader.offset < OCTET_BITS && bits[reader.offset..].iter().all(|bit| !bit) {
            break;
        }
        let start = reader.offset;
        let parameter = reader.take(PARAMETER_BITS)?;
        let auxiliary = if parameter == EXTENSION {
            Some(reader.take(PARAMETER_BITS)?)
        } else {
            None
        };
        let (rank, width) = match (parameter, auxiliary) {
            (POWER, _) => (POWER, POWER_BITS),
            (TIMING, _) => (TIMING, TIMING_BITS),
            (DTX_ALLOWED | DIC, _) => (parameter, FLAG_BITS),
            (DTX_MODE, _) => (DTX_MODE, DTX_MODE_BITS),
            (EXTENSION, Some(DPC)) => (EXTENSION, DPC_BITS),
            (EXTENSION, Some(FPC)) => (EXTENSION + 1, FPC_BITS),
            (0, _) => return Err(TrafficMessageError::Field("physical_control_parameter")),
            _ => {
                fields.unparsed_bits = bits[start..].to_vec();
                break;
            }
        };
        if rank <= last_rank {
            return Err(TrafficMessageError::Field("physical_control_order"));
        }
        last_rank = rank;
        if reader.take(COUNT_BITS)? != 1 {
            return Err(TrafficMessageError::Field("physical_control_count"));
        }
        let value = reader.take(width)? as u8;
        match (parameter, auxiliary) {
            (POWER, _) => fields.control.power_level = Some(value),
            (TIMING, _) => {
                fields.control.time_alignment = Some(if value == MAINTAIN_ALIGNMENT {
                    TimeAlignment::Maintain
                } else {
                    TimeAlignment::OffsetUnits(value)
                })
            }
            (DTX_ALLOWED, _) => fields.control.dtx_allowed = Some(value != 0),
            (DIC, _) => fields.control.delay_compensation = Some(value != 0),
            (DTX_MODE, _) => fields.dtx_mode = Some(value),
            (EXTENSION, Some(DPC)) => fields.downlink_power_control = Some(value),
            (EXTENSION, Some(FPC)) => fields.fast_power_control = Some(value),
            _ => unreachable!("known physical control parameter"),
        }
    }
    fields.control.validate()?;
    if fields.control.dtx_allowed.is_some() && fields.dtx_mode.is_some() {
        return Err(TrafficMessageError::Field("conflicting_dtx_control"));
    }
    // TIA-136-133-E §4.4.2 forbids Maintain in an acknowledgment, but handsets send it. It
    // reports no change, so the acknowledgment is still accepted.
    Ok(fields)
}

#[cfg(test)]
mod tests;
