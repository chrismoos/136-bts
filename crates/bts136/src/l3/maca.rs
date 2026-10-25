pub mod broadcast;
pub(super) mod targets;
pub use super::control_fields::Error;
use super::{
    broadcast::ProtocolVersion,
    control_fields::{self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put},
    legacy_traffic::{ControlChannel, Hyperband},
};
pub use targets::{HyperbandChannels, MeasurementTargets};
pub(super) use targets::{MAX_OTHER_CHANNELS, validate_target_lists};
pub const MESSAGE_TYPE: u8 = 6;
const LTM: u8 = 1;
const STM: u8 = 2;
const OTHER: u8 = 3;
const EXTENDED_LTM: u8 = 4;
const EXTENDED_STM: u8 = 5;
const EXTENDED_OTHER: u8 = 6;
const COUNT_BITS: usize = 4;
const BASIC_QUALITY_BITS: usize = 3;
const EXTENDED_QUALITY_BITS: usize = 4;
const BASIC_RSS_BITS: usize = 5;
const EXTENDED_RSS_BITS: usize = 6;
const BASIC_RSS_MAX: u8 = 31;
const EXTENDED_RSS_MAX: u8 = 41;
const MAX_CURRENT_VALUES: usize = 16;
const MAX_OTHER_REPORTS: usize = 3;
const RSS_MIN_CODE: u8 = 0;
const RSS_BASE_DBM: i16 = -113;
const RSS_STEP_DB: i16 = 2;
const RESERVED: u8 = 0;
const EXTENSION: u8 = 7;
const FUTURE: u8 = 15;
const TYPE_BITS: usize = 6;
#[derive(Clone, Copy, PartialEq, Eq)]
enum MeasurementKind {
    LongTerm,
    Current,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasurementFormat {
    Basic,
    Extended,
}
impl MeasurementFormat {
    fn quality_bits(self) -> usize {
        match self {
            Self::Basic => BASIC_QUALITY_BITS,
            Self::Extended => EXTENDED_QUALITY_BITS,
        }
    }
    fn rss_bits(self) -> usize {
        match self {
            Self::Basic => BASIC_RSS_BITS,
            Self::Extended => EXTENDED_RSS_BITS,
        }
    }
    fn rss_max(self) -> u8 {
        match self {
            Self::Basic => BASIC_RSS_MAX,
            Self::Extended => EXTENDED_RSS_MAX,
        }
    }
    fn codes(self) -> [u8; 3] {
        match self {
            Self::Basic => [LTM, STM, OTHER],
            Self::Extended => [EXTENDED_LTM, EXTENDED_STM, EXTENDED_OTHER],
        }
    }
    fn validate_rss(self, code: u8) -> Result<(), Error> {
        if code > self.rss_max() {
            return Err(Error::Field("maca_rss"));
        }
        Ok(())
    }
    pub fn rss_description(self, code: u8) -> Result<String, Error> {
        self.validate_rss(code)?;
        let value = RSS_BASE_DBM + RSS_STEP_DB * i16::from(code);
        Ok(if code == RSS_MIN_CODE {
            format!("≤{value} dBm")
        } else if code == self.rss_max() {
            format!("≥{value} dBm")
        } else {
            format!("{value} dBm")
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongTermMeasurement {
    pub wer: u8,
    pub ber: u8,
    pub rss: u8,
    pub full: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherMeasurements {
    pub rss: Vec<Option<u8>>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Measurements {
    pub long_term: Option<LongTermMeasurement>,
    pub current: Option<Vec<u8>>,
    pub other: Vec<OtherMeasurements>,
}
impl Measurements {
    fn validate(&self, format: MeasurementFormat) -> Result<(), Error> {
        if self.long_term.is_none() && self.current.is_none() && self.other.is_empty() {
            return Err(Error::Field("empty_maca_format"));
        }
        if let Some(value) = self.long_term {
            if u64::from(value.wer) >= 1 << format.quality_bits()
                || u64::from(value.ber) >= 1 << format.quality_bits()
            {
                return Err(Error::Field("maca_quality"));
            }
            format.validate_rss(value.rss)?;
        }
        if let Some(values) = &self.current {
            if values.is_empty() || values.len() > MAX_CURRENT_VALUES {
                return Err(Error::Field("maca_current_count"));
            }
            for code in values {
                format.validate_rss(*code)?;
            }
        }
        if self.other.len() > MAX_OTHER_REPORTS {
            return Err(Error::Field("maca_other_count"));
        }
        for report in &self.other {
            if report.rss.is_empty() || report.rss.len() > MAX_OTHER_CHANNELS {
                return Err(Error::Field("maca_report_map_length"));
            }
            for code in report.rss.iter().flatten() {
                format.validate_rss(*code)?;
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ReportMeasurements {
    #[default]
    None,
    Basic(Measurements),
    Extended(Measurements),
}
impl ReportMeasurements {
    pub fn format(&self) -> Option<MeasurementFormat> {
        match self {
            Self::None => None,
            Self::Basic(_) => Some(MeasurementFormat::Basic),
            Self::Extended(_) => Some(MeasurementFormat::Extended),
        }
    }
    pub fn fields(&self) -> Option<&Measurements> {
        match self {
            Self::None => None,
            Self::Basic(value) | Self::Extended(value) => Some(value),
        }
    }
    fn fields_mut(&mut self, format: MeasurementFormat) -> Result<&mut Measurements, Error> {
        if self.format().is_some_and(|current| current != format) {
            return Err(Error::Field("mixed_maca_formats"));
        }
        if matches!(self, Self::None) {
            *self = match format {
                MeasurementFormat::Basic => Self::Basic(Default::default()),
                MeasurementFormat::Extended => Self::Extended(Default::default()),
            };
        }
        match self {
            Self::Basic(value) | Self::Extended(value) => Ok(value),
            Self::None => Err(Error::Field("maca_format")),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacaReport {
    pub measurements: ReportMeasurements,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelReading {
    pub hyperband: Hyperband,
    pub channel: u16,
    pub rss_code: Option<u8>,
    pub format: MeasurementFormat,
}
impl MacaReport {
    pub fn validate_negotiation(
        &self,
        version: ProtocolVersion,
        format: MeasurementFormat,
    ) -> Result<(), Error> {
        if self
            .measurements
            .format()
            .is_some_and(|actual| actual != format)
            || format == MeasurementFormat::Extended && version.code() < ProtocolVersion::Pv2.code()
        {
            return Err(Error::Field("maca_negotiation"));
        }
        self.encode().map(|_| ())
    }
    /// TIA-136-123-E §5.4.6 omits band IDs. The caller must retain the original target list for the measurement exchange.
    pub fn short_term_readings(
        &self,
        targets: &MeasurementTargets,
    ) -> Result<Vec<ChannelReading>, Error> {
        let Some(fields) = self.measurements.fields() else {
            return Ok(vec![]);
        };
        let format = self
            .measurements
            .format()
            .ok_or(Error::Field("maca_format"))?;
        fields.validate(format)?;
        if fields.current.is_none() && fields.other.is_empty() {
            return Ok(vec![]);
        }
        if fields.other.len() != targets.other.len() {
            return Err(Error::Field("maca_target_count"));
        }
        let mut readings = Vec::new();
        if let Some(values) = &fields.current {
            if values.len() != targets.current.len() + 1 {
                return Err(Error::Field("maca_target_count"));
            }
            for (channel, rss) in std::iter::once(targets.serving.channel)
                .chain(targets.current.iter().copied())
                .zip(values)
            {
                readings.push(ChannelReading {
                    hyperband: targets.serving.hyperband,
                    channel,
                    rss_code: Some(*rss),
                    format,
                });
            }
        }
        for (report, list) in fields.other.iter().zip(&targets.other) {
            if report.rss.len() != list.channels.len() {
                return Err(Error::Field("maca_target_map_length"));
            }
            for (channel, rss) in list.channels.iter().zip(&report.rss) {
                readings.push(ChannelReading {
                    hyperband: list.hyperband,
                    channel: *channel,
                    rss_code: *rss,
                    format,
                });
            }
        }
        Ok(readings)
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bits = Vec::new();
        put(&mut bits, u64::from(MESSAGE_TYPE), OCTET_BITS)?;
        if let (Some(format), Some(fields)) =
            (self.measurements.format(), self.measurements.fields())
        {
            fields.validate(format)?;
            let codes = format.codes();
            if let Some(value) = fields.long_term {
                put(&mut bits, u64::from(codes[0]), PARAMETER_BITS)?;
                put(&mut bits, u64::from(value.wer), format.quality_bits())?;
                put(&mut bits, u64::from(value.ber), format.quality_bits())?;
                put(&mut bits, u64::from(value.rss), format.rss_bits())?;
                put(&mut bits, u64::from(value.full), 1)?;
            }
            if let Some(values) = &fields.current {
                put(&mut bits, u64::from(codes[1]), PARAMETER_BITS)?;
                put(&mut bits, (values.len() - 1) as u64, COUNT_BITS)?;
                for code in values {
                    put(&mut bits, u64::from(*code), format.rss_bits())?;
                }
            }
            for report in &fields.other {
                put(&mut bits, u64::from(codes[2]), PARAMETER_BITS)?;
                put(&mut bits, report.rss.len() as u64, COUNT_BITS)?;
                for value in &report.rss {
                    put(&mut bits, u64::from(value.is_some()), 1)?;
                }
                for code in report.rss.iter().flatten() {
                    put(&mut bits, u64::from(*code), format.rss_bits())?;
                }
            }
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
        if header != MESSAGE_TYPE {
            return Err(Error::MessageType);
        }
        let mut result = Self::default();
        let mut previous = 0;
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
            let (format, kind) = match code {
                LTM => (MeasurementFormat::Basic, MeasurementKind::LongTerm),
                STM => (MeasurementFormat::Basic, MeasurementKind::Current),
                OTHER => (MeasurementFormat::Basic, MeasurementKind::Other),
                EXTENDED_LTM => (MeasurementFormat::Extended, MeasurementKind::LongTerm),
                EXTENDED_STM => (MeasurementFormat::Extended, MeasurementKind::Current),
                EXTENDED_OTHER => (MeasurementFormat::Extended, MeasurementKind::Other),
                _ => {
                    if reader.skip_noncritical(code)? {
                        result
                            .ignored_bits
                            .extend_from_slice(&bits[start..reader.offset]);
                        continue;
                    }
                    result.unparsed_bits = bits[start..].to_vec();
                    break;
                }
            };
            if code < previous || code == previous && kind != MeasurementKind::Other {
                return Err(Error::Field("parameter_order"));
            }
            previous = code;
            let fields = result.measurements.fields_mut(format)?;
            match kind {
                MeasurementKind::LongTerm => {
                    fields.long_term = Some(LongTermMeasurement {
                        wer: reader.take(format.quality_bits())? as u8,
                        ber: reader.take(format.quality_bits())? as u8,
                        rss: reader.take(format.rss_bits())? as u8,
                        full: reader.flag()?,
                    })
                }
                MeasurementKind::Current => {
                    let count = reader.take(COUNT_BITS)? as usize + 1;
                    let values = (0..count)
                        .map(|_| reader.take(format.rss_bits()).map(|value| value as u8))
                        .collect::<Result<Vec<_>, _>>()?;
                    fields.current = Some(values);
                }
                MeasurementKind::Other => {
                    if fields.other.len() == MAX_OTHER_REPORTS {
                        return Err(Error::Field("maca_other_count"));
                    }
                    let count = reader.take(COUNT_BITS)? as usize;
                    if count == 0 {
                        return Err(Error::Field("maca_report_map_length"));
                    }
                    let map = (0..count)
                        .map(|_| reader.flag())
                        .collect::<Result<Vec<_>, _>>()?;
                    let mut values = Vec::with_capacity(count);
                    for measured in map {
                        values.push(if measured {
                            Some(reader.take(format.rss_bits())? as u8)
                        } else {
                            None
                        });
                    }
                    fields.other.push(OtherMeasurements { rss: values });
                }
            }
        }
        if let (Some(format), Some(fields)) =
            (result.measurements.format(), result.measurements.fields())
        {
            fields.validate(format)?;
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests;
