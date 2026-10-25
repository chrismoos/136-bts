use super::{ActiveAmrMode, ProtocolVersion, Reader, TrafficMessage, TrafficMessageError, put};
use crate::l3::relay::Direction;
const CONFIGURATION: u8 = 0xd1;
const THRESHOLDS: u8 = 0xd2;
const PHASE_FREEZE: u8 = 0xa9;
const FLAG_BITS: usize = 1;
const MODE_BITS: usize = 2;
const SET_BITS: usize = 8;
const THRESHOLD_BITS: usize = 6;
const HYSTERESIS_BITS: usize = 4;
const MAX_ACTIVE_MODES: usize = 4;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AmrLink {
    Neither = 0,
    Downlink = 1,
    Uplink = 2,
    Both = 3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AmrPhase {
    DefaultOdd = 0,
    OppositeEven = 1,
    Maintain = 2,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AmrFreeze {
    Unfreeze = 0,
    Freeze = 1,
    Maintain = 2,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmrCodecMode {
    Mr475,
    Mr515,
    Mr59,
    Mr67,
    Mr74,
    Mr795,
    Mr102,
    Mr122,
}
const CODEC_MODES: [AmrCodecMode; 8] = [
    AmrCodecMode::Mr475,
    AmrCodecMode::Mr515,
    AmrCodecMode::Mr59,
    AmrCodecMode::Mr67,
    AmrCodecMode::Mr74,
    AmrCodecMode::Mr795,
    AmrCodecMode::Mr102,
    AmrCodecMode::Mr122,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmrActiveSet(u8);
impl AmrActiveSet {
    // TIA-136-440-A §6.1.1 Table 1 assigns MR122 to bit 7 and MR475 to bit 0, with at most four modes.
    pub fn try_new(bitmap: u8) -> Result<Self, TrafficMessageError> {
        if !(1..=MAX_ACTIVE_MODES).contains(&(bitmap.count_ones() as usize)) {
            return Err(TrafficMessageError::Field("amr_active_set"));
        }
        Ok(Self(bitmap))
    }
    pub fn bitmap(self) -> u8 {
        self.0
    }
    pub fn mode_count(self) -> usize {
        self.0.count_ones() as usize
    }
    // TIA-136-440-A §6.1.1 Table 2 orders in-band mode positions by increasing speech bit rate.
    pub fn codec_mode(self, position: ActiveAmrMode) -> Option<AmrCodecMode> {
        CODEC_MODES
            .into_iter()
            .enumerate()
            .filter(|(bit, _)| self.0 & (1 << bit) != 0)
            .nth(position as usize)
            .map(|(_, mode)| mode)
    }
    pub fn contains_position(self, mode: ActiveAmrMode) -> bool {
        (mode as usize) < self.mode_count()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmrThresholds {
    pub lower: [u8; 3],
    pub hysteresis: [u8; 3],
}
impl AmrThresholds {
    fn validate_fields(&self) -> Result<(), TrafficMessageError> {
        if self.lower.iter().any(|value| *value >= 1 << THRESHOLD_BITS)
            || self
                .hysteresis
                .iter()
                .any(|value| *value >= 1 << HYSTERESIS_BITS)
        {
            return Err(TrafficMessageError::Field("amr_thresholds"));
        }
        Ok(())
    }
    // TIA-136-440-A §6.1.5 defines a threshold for each adjacent pair and strictly ascending lower/upper thresholds.
    pub fn validate_for_active_set(&self, active: AmrActiveSet) -> Result<(), TrafficMessageError> {
        self.validate_fields()?;
        let count = active.mode_count() - 1;
        let upper: [u16; 3] = std::array::from_fn(|index| {
            u16::from(self.lower[index]) + u16::from(self.hysteresis[index])
        });
        if self.lower[..count]
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
            || upper[..count].windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(TrafficMessageError::Field("amr_threshold_order"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmrConfiguration {
    pub countdown: bool,
    pub link: AmrLink,
    pub initial_mode: ActiveAmrMode,
    pub active_set: AmrActiveSet,
    pub thresholds: [u8; 3],
    pub common_hysteresis: u8,
}
impl AmrConfiguration {
    fn validate(&self) -> Result<(), TrafficMessageError> {
        if !self.active_set.contains_position(self.initial_mode) {
            return Err(TrafficMessageError::Field("amr_initial_mode"));
        }
        AmrThresholds {
            lower: self.thresholds,
            hysteresis: [self.common_hysteresis; 3],
        }
        .validate_for_active_set(self.active_set)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmrControlMessage {
    Configuration(AmrConfiguration),
    Thresholds(AmrThresholds),
    PhaseFreeze {
        phase: AmrPhase,
        freeze: AmrFreeze,
        /// TIA-136-133-E §9.1.2.2.39 transmits this field even when Freeze Indicator makes it inactive.
        mode: ActiveAmrMode,
    },
}
impl AmrControlMessage {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), TrafficMessageError> {
        if version.code() < ProtocolVersion::Pv5.code() {
            return Err(TrafficMessageError::Field("amr_control_version"));
        }
        Ok(())
    }
    pub fn encode_for_version(
        &self,
        version: ProtocolVersion,
    ) -> Result<Vec<bool>, TrafficMessageError> {
        self.validate_version(version)?;
        TrafficMessage::AmrControl(*self).encode()
    }
    pub(super) fn message_type(&self) -> u8 {
        match self {
            Self::Configuration(_) => CONFIGURATION,
            Self::Thresholds(_) => THRESHOLDS,
            Self::PhaseFreeze { .. } => PHASE_FREEZE,
        }
    }
    // TIA-136-133-E §§9.1.2.2.37–39 specify 45, 40 and 16 bits without Remaining Length.
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        match self {
            Self::Configuration(configuration) => {
                configuration.validate()?;
                put(bits, u64::from(configuration.countdown), FLAG_BITS);
                put(bits, configuration.link as u64, MODE_BITS);
                put(bits, configuration.initial_mode as u64, MODE_BITS);
                put(bits, u64::from(configuration.active_set.bitmap()), SET_BITS);
                for threshold in configuration.thresholds {
                    put(bits, u64::from(threshold), THRESHOLD_BITS);
                }
                put(
                    bits,
                    u64::from(configuration.common_hysteresis),
                    HYSTERESIS_BITS,
                );
            }
            Self::Thresholds(thresholds) => {
                thresholds.validate_fields()?;
                for threshold in thresholds.lower {
                    put(bits, u64::from(threshold), THRESHOLD_BITS);
                }
                for hysteresis in thresholds.hysteresis {
                    put(bits, u64::from(hysteresis), HYSTERESIS_BITS);
                }
            }
            Self::PhaseFreeze {
                phase,
                freeze,
                mode,
            } => {
                put(bits, *phase as u64, MODE_BITS);
                put(bits, *freeze as u64, MODE_BITS);
                put(bits, *mode as u64, MODE_BITS);
            }
        }
        Ok(())
    }
}
pub(super) fn decode(
    kind: u8,
    reader: &mut Reader<'_>,
    direction: Direction,
    version: Option<ProtocolVersion>,
) -> Result<Option<AmrControlMessage>, TrafficMessageError> {
    if !matches!(kind, CONFIGURATION | THRESHOLDS | PHASE_FREEZE) {
        return Ok(None);
    }
    if direction != Direction::Forward {
        return Err(TrafficMessageError::Field("amr_control_direction"));
    }
    let message = match kind {
        CONFIGURATION => {
            let countdown = reader.take(FLAG_BITS)? != 0;
            let link = match reader.take(MODE_BITS)? {
                0 => AmrLink::Neither,
                1 => AmrLink::Downlink,
                2 => AmrLink::Uplink,
                _ => AmrLink::Both,
            };
            let initial_mode = ActiveAmrMode::try_from(reader.take(MODE_BITS)? as u8)?;
            let active_set = AmrActiveSet::try_new(reader.take(SET_BITS)? as u8)?;
            let mut thresholds = [0; 3];
            for threshold in &mut thresholds {
                *threshold = reader.take(THRESHOLD_BITS)? as u8;
            }
            let configuration = AmrConfiguration {
                countdown,
                link,
                initial_mode,
                active_set,
                thresholds,
                common_hysteresis: reader.take(HYSTERESIS_BITS)? as u8,
            };
            configuration.validate()?;
            AmrControlMessage::Configuration(configuration)
        }
        THRESHOLDS => {
            let mut lower = [0; 3];
            let mut hysteresis = [0; 3];
            for value in &mut lower {
                *value = reader.take(THRESHOLD_BITS)? as u8;
            }
            for value in &mut hysteresis {
                *value = reader.take(HYSTERESIS_BITS)? as u8;
            }
            AmrControlMessage::Thresholds(AmrThresholds { lower, hysteresis })
        }
        _ => {
            let phase = match reader.take(MODE_BITS)? {
                0 => AmrPhase::DefaultOdd,
                1 => AmrPhase::OppositeEven,
                2 => AmrPhase::Maintain,
                _ => return Err(TrafficMessageError::Field("amr_phase")),
            };
            let freeze = match reader.take(MODE_BITS)? {
                0 => AmrFreeze::Unfreeze,
                1 => AmrFreeze::Freeze,
                2 => AmrFreeze::Maintain,
                _ => return Err(TrafficMessageError::Field("amr_freeze")),
            };
            AmrControlMessage::PhaseFreeze {
                phase,
                freeze,
                mode: ActiveAmrMode::try_from(reader.take(MODE_BITS)? as u8)?,
            }
        }
    };
    if let Some(version) = version {
        message.validate_version(version)?;
    }
    Ok(Some(message))
}
#[cfg(test)]
mod tests;
