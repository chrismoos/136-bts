use super::targets::{read_maca_channels, write_maca_channels};
use super::{
    Error, HyperbandChannels, MeasurementFormat, MeasurementTargets, validate_target_lists,
};
use crate::l3::{
    broadcast::ProtocolVersion,
    control_fields::{self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put},
    legacy_traffic::{ControlChannel, Hyperband},
};
const BASIC_TYPE: u8 = 5;
const MULTI_TYPE: u8 = 41;
const TYPE_BITS: usize = 6;
const STATUS_BITS: usize = 2;
const TRIGGER_BITS: usize = 4;
const HYPERBAND_BITS: usize = 2;
const ENHANCED_BITS: usize = 3;
const DPC: u8 = 1;
const EXTENDED: u8 = 2;
const RESERVED_ENHANCED: u8 = 4;
const AT_REGISTRATION: u8 = 1;
const AT_ORIGINATION: u8 = 2;
const AT_PAGE_RESPONSE: u8 = 4;
const AT_AUDIT: u8 = 8;
const EIGHT_CONTROL: u8 = 1;
const CURRENT_LIST: u8 = 2;
const BASIC_ENHANCED: u8 = 3;
const MULTI_OTHER: u8 = 3;
const MULTI_ENHANCED: u8 = 4;
const RESERVED: u8 = 0;
const EXTENSION: u8 = 7;
const FUTURE: u8 = 15;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BroadcastKind {
    #[default]
    Basic,
    MultiHyperband,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum MacaStatus {
    #[default]
    Disabled = 0,
    LongTerm = 1,
    ShortTerm = 2,
    Both = 3,
}
impl TryFrom<u8> for MacaStatus {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Disabled),
            1 => Ok(Self::LongTerm),
            2 => Ok(Self::ShortTerm),
            3 => Ok(Self::Both),
            _ => Err(Error::Field("maca_status")),
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReportTriggers {
    pub registration: bool,
    pub origination: bool,
    pub page_response: bool,
    pub audit: bool,
}
impl ReportTriggers {
    fn bits(self) -> u8 {
        (u8::from(self.registration) * AT_REGISTRATION)
            | (u8::from(self.origination) * AT_ORIGINATION)
            | (u8::from(self.page_response) * AT_PAGE_RESPONSE)
            | (u8::from(self.audit) * AT_AUDIT)
    }
    fn decode(value: u8) -> Self {
        Self {
            registration: value & AT_REGISTRATION != 0,
            origination: value & AT_ORIGINATION != 0,
            page_response: value & AT_PAGE_RESPONSE != 0,
            audit: value & AT_AUDIT != 0,
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnhancedControl {
    pub dpc: bool,
    pub extended: bool,
}
impl EnhancedControl {
    fn bits(self) -> u8 {
        (u8::from(self.dpc) * DPC) | (u8::from(self.extended) * EXTENDED)
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacaBroadcast {
    pub kind: BroadcastKind,
    pub status: MacaStatus,
    pub triggers: ReportTriggers,
    pub unrestricted: Option<bool>,
    pub current_channels: Option<Vec<u16>>,
    pub other_channels: Vec<HyperbandChannels>,
    pub enhanced: Option<EnhancedControl>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Parameter {
    EightControl,
    Current,
    Other,
    Enhanced,
}
impl BroadcastKind {
    pub(crate) fn header(self) -> u8 {
        match self {
            Self::Basic => BASIC_TYPE,
            Self::MultiHyperband => MULTI_TYPE,
        }
    }
    fn enhanced_code(self) -> u8 {
        match self {
            Self::Basic => BASIC_ENHANCED,
            Self::MultiHyperband => MULTI_ENHANCED,
        }
    }
    fn parameter(self, code: u8) -> Option<Parameter> {
        match code {
            EIGHT_CONTROL => Some(Parameter::EightControl),
            CURRENT_LIST => Some(Parameter::Current),
            MULTI_OTHER if self == Self::MultiHyperband => Some(Parameter::Other),
            code if code == self.enhanced_code() => Some(Parameter::Enhanced),
            _ => None,
        }
    }
}
impl MacaBroadcast {
    fn validate(&self) -> Result<(), Error> {
        if (self.kind == BroadcastKind::Basic && !self.other_channels.is_empty())
            || (self.kind == BroadcastKind::MultiHyperband && self.other_channels.is_empty())
        {
            return Err(Error::Field("maca_broadcast_kind"));
        }
        validate_target_lists(self.current_channels.as_deref(), &self.other_channels)
    }
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), Error> {
        if self.enhanced.is_some() && version.code() < ProtocolVersion::Pv2.code() {
            return Err(Error::Field("maca_broadcast_version"));
        }
        self.encode().map(|_| ())
    }
    pub fn advertised_format(&self) -> MeasurementFormat {
        if self.enhanced.is_some_and(|value| value.extended) {
            MeasurementFormat::Extended
        } else {
            MeasurementFormat::Basic
        }
    }
    /// TIA-136-123-E §5.1.1.2.9 retains an omitted current-band list from the base. The caller selects the applicable broadcast using handset capabilities.
    pub fn resolve_targets(
        &self,
        serving: ControlChannel,
        base: Option<&Self>,
    ) -> Result<MeasurementTargets, Error> {
        self.validate()?;
        if let Some(base) = base {
            if base.kind != BroadcastKind::Basic {
                return Err(Error::Field("maca_base_kind"));
            }
            base.validate()?;
        }
        let current = match (&self.current_channels, self.kind, base) {
            (Some(channels), _, _) => channels.clone(),
            (None, BroadcastKind::MultiHyperband, Some(base)) => {
                base.current_channels.clone().unwrap_or_default()
            }
            _ => vec![],
        };
        MeasurementTargets::try_new(serving, current, self.other_channels.clone())
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let mut bits = vec![];
        put(&mut bits, u64::from(self.kind.header()), OCTET_BITS)?;
        put(&mut bits, self.status as u64, STATUS_BITS)?;
        put(&mut bits, u64::from(self.triggers.bits()), TRIGGER_BITS)?;
        if let Some(value) = self.unrestricted {
            put(&mut bits, u64::from(EIGHT_CONTROL), PARAMETER_BITS)?;
            put(&mut bits, u64::from(value), 1)?;
        }
        if let Some(channels) = &self.current_channels {
            put(&mut bits, u64::from(CURRENT_LIST), PARAMETER_BITS)?;
            write_maca_channels(&mut bits, channels)?;
        }
        for list in &self.other_channels {
            put(&mut bits, u64::from(MULTI_OTHER), PARAMETER_BITS)?;
            put(&mut bits, list.hyperband as u64, HYPERBAND_BITS)?;
            write_maca_channels(&mut bits, &list.channels)?;
        }
        if let Some(enhanced) = self.enhanced {
            put(
                &mut bits,
                u64::from(self.kind.enhanced_code()),
                PARAMETER_BITS,
            )?;
            put(&mut bits, u64::from(enhanced.bits()), ENHANCED_BITS)?;
        }
        bits.extend(&self.ignored_bits);
        bits.extend(&self.unparsed_bits);
        pack(bits)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let bits = control_fields::bits(bytes)?;
        let mut reader = Reader {
            bits: &bits,
            offset: 0,
        };
        let header = reader.take(OCTET_BITS)? as u8;
        if header >> TYPE_BITS != 0 {
            return Err(Error::Protocol);
        }
        let kind = match header {
            BASIC_TYPE => BroadcastKind::Basic,
            MULTI_TYPE => BroadcastKind::MultiHyperband,
            _ => return Err(Error::MessageType),
        };
        let mut result = Self {
            kind,
            status: MacaStatus::try_from(reader.take(STATUS_BITS)? as u8)?,
            triggers: ReportTriggers::decode(reader.take(TRIGGER_BITS)? as u8),
            ..Default::default()
        };
        let mut previous = None;
        while reader.offset < bits.len() {
            let remaining = &bits[reader.offset..];
            if remaining.len() < OCTET_BITS && remaining.iter().all(|bit| !*bit) {
                break;
            }
            let start = reader.offset;
            let code = reader.take(PARAMETER_BITS)? as u8;
            if matches!(code, RESERVED | FUTURE) {
                return Err(Error::Field("optional_reserved"));
            }
            if code == EXTENSION {
                let auxiliary = reader.take(PARAMETER_BITS)? as u8;
                if matches!(auxiliary, RESERVED | FUTURE) {
                    return Err(Error::Field("auxiliary_reserved"));
                }
            }
            let Some(parameter) = kind.parameter(code) else {
                if reader.skip_noncritical(code)? {
                    result
                        .ignored_bits
                        .extend_from_slice(&bits[start..reader.offset]);
                    continue;
                }
                result.unparsed_bits = bits[start..].to_vec();
                break;
            };
            if previous.is_some_and(|previous| {
                parameter < previous || parameter == previous && parameter != Parameter::Other
            }) {
                return Err(Error::Field("parameter_order"));
            }
            previous = Some(parameter);
            match parameter {
                Parameter::EightControl => result.unrestricted = Some(reader.flag()?),
                Parameter::Current => {
                    result.current_channels = Some(read_maca_channels(&mut reader)?)
                }
                Parameter::Other => {
                    let hyperband = Hyperband::try_from(reader.take(HYPERBAND_BITS)? as u8)
                        .map_err(|_| Error::Field("hyperband"))?;
                    result.other_channels.push(HyperbandChannels {
                        hyperband,
                        channels: read_maca_channels(&mut reader)?,
                    });
                }
                Parameter::Enhanced => {
                    let value = reader.take(ENHANCED_BITS)? as u8;
                    if value & RESERVED_ENHANCED != 0 {
                        return Err(Error::Field("enhanced_maca_reserved"));
                    }
                    result.enhanced = Some(EnhancedControl {
                        dpc: value & DPC != 0,
                        extended: value & EXTENDED != 0,
                    });
                }
            }
        }
        result.validate()?;
        Ok(result)
    }
}
#[cfg(test)]
mod tests;
