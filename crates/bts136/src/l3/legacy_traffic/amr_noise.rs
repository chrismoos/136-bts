use super::{ProtocolVersion, Reader, TrafficMessage, TrafficMessageError, put};
use crate::l3::relay::Direction;
const AMR1: u8 = 0xaa;
const AMR2: u8 = 0xab;
const AMR3: u8 = 0xad;
const AMR4: u8 = 0xae;
const IMPLICIT: u8 = 0xd0;
const MODE_BITS: usize = 2;
const REFERENCE_BITS: usize = 3;
const SUBVECTOR_BITS: [usize; 3] = [8, 9, 9];
const ENERGY_BITS: usize = 6;
/// TIA-136-440 codec modes identify positions in the negotiated active set, not absolute rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ActiveAmrMode {
    Mode1 = 0,
    Mode2 = 1,
    Mode3 = 2,
    Mode4 = 3,
}
impl TryFrom<u8> for ActiveAmrMode {
    type Error = TrafficMessageError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Mode1),
            1 => Ok(Self::Mode2),
            2 => Ok(Self::Mode3),
            3 => Ok(Self::Mode4),
            _ => Err(TrafficMessageError::Field("amr_active_mode")),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmrNoiseParameters {
    pub lsf_reference_vector: u8,
    pub lsf_subvectors: [u16; 3],
    pub frame_energy: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmrNoiseMessage {
    Parameters {
        command: ActiveAmrMode,
        indication: ActiveAmrMode,
        parameters: AmrNoiseParameters,
    },
    /// TIA-136-133-E §§5.1.2.2.36/9.1.2.2.40 require direction and CMI phase to interpret this field.
    Implicit { data: ActiveAmrMode },
}
impl AmrNoiseMessage {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), TrafficMessageError> {
        if version.code() < ProtocolVersion::Pv5.code() {
            return Err(TrafficMessageError::Field("amr_noise_version"));
        }
        Ok(())
    }
    fn validate_direction(&self, direction: Direction) -> Result<(), TrafficMessageError> {
        if matches!(self, Self::Parameters { .. }) && direction != Direction::Forward {
            return Err(TrafficMessageError::Field("amr_noise_direction"));
        }
        Ok(())
    }
    pub fn encode_for_version(
        &self,
        version: ProtocolVersion,
        direction: Direction,
    ) -> Result<Vec<bool>, TrafficMessageError> {
        self.validate_version(version)?;
        self.validate_direction(direction)?;
        TrafficMessage::AmrNoise(*self).encode()
    }
    pub(super) fn message_type(&self) -> u8 {
        match self {
            Self::Parameters { command, .. } => match command {
                ActiveAmrMode::Mode1 => AMR1,
                ActiveAmrMode::Mode2 => AMR2,
                ActiveAmrMode::Mode3 => AMR3,
                ActiveAmrMode::Mode4 => AMR4,
            },
            Self::Implicit { .. } => IMPLICIT,
        }
    }
    // TIA-136-133-E §§9.1.2.2.33–36/40 define 47-bit parameter and 12-bit implicit messages without Remaining Length.
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        match self {
            Self::Parameters {
                indication,
                parameters,
                ..
            } => {
                if parameters.lsf_reference_vector >= 1 << REFERENCE_BITS
                    || parameters.frame_energy >= 1 << ENERGY_BITS
                    || parameters
                        .lsf_subvectors
                        .iter()
                        .zip(SUBVECTOR_BITS)
                        .any(|(value, width)| *value >= 1 << width)
                {
                    return Err(TrafficMessageError::Field("amr_noise_parameters"));
                }
                put(bits, *indication as u64, MODE_BITS);
                put(
                    bits,
                    u64::from(parameters.lsf_reference_vector),
                    REFERENCE_BITS,
                );
                for (value, width) in parameters.lsf_subvectors.iter().zip(SUBVECTOR_BITS) {
                    put(bits, u64::from(*value), width);
                }
                put(bits, u64::from(parameters.frame_energy), ENERGY_BITS);
            }
            Self::Implicit { data } => put(bits, *data as u64, MODE_BITS),
        }
        Ok(())
    }
}
pub(super) fn decode(
    kind: u8,
    reader: &mut Reader<'_>,
    direction: Direction,
    version: Option<ProtocolVersion>,
) -> Result<Option<AmrNoiseMessage>, TrafficMessageError> {
    let command = match kind {
        AMR1 => Some(ActiveAmrMode::Mode1),
        AMR2 => Some(ActiveAmrMode::Mode2),
        AMR3 => Some(ActiveAmrMode::Mode3),
        AMR4 => Some(ActiveAmrMode::Mode4),
        IMPLICIT => None,
        _ => return Ok(None),
    };
    if command.is_some() && direction != Direction::Forward {
        return Err(TrafficMessageError::Field("amr_noise_direction"));
    }
    let data = ActiveAmrMode::try_from(reader.take(MODE_BITS)? as u8)?;
    let message = if let Some(command) = command {
        let reference = reader.take(REFERENCE_BITS)? as u8;
        let mut subvectors = [0; 3];
        for (value, width) in subvectors.iter_mut().zip(SUBVECTOR_BITS) {
            *value = reader.take(width)? as u16;
        }
        AmrNoiseMessage::Parameters {
            command,
            indication: data,
            parameters: AmrNoiseParameters {
                lsf_reference_vector: reference,
                lsf_subvectors: subvectors,
                frame_energy: reader.take(ENERGY_BITS)? as u8,
            },
        }
    } else {
        AmrNoiseMessage::Implicit { data }
    };
    if let Some(version) = version {
        message.validate_version(version)?;
    }
    Ok(Some(message))
}
#[cfg(test)]
mod tests;
