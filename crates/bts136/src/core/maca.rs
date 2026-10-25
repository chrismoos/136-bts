use crate::l3::{
    broadcast::ProtocolVersion,
    legacy_traffic::{ControlChannel, Hyperband},
    maca::{
        ChannelReading, Error, HyperbandChannels, MacaReport, MeasurementFormat,
        MeasurementTargets,
        broadcast::{BroadcastKind, EnhancedControl, MacaBroadcast, MacaStatus, ReportTriggers},
    },
};
const UNRESTRICTED_THRESHOLD: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessTrigger {
    Registration,
    Origination,
    PageResponse,
    Audit,
}

pub struct MacaPlan {
    serving: ControlChannel,
    basic: MacaBroadcast,
    multi: Option<MacaBroadcast>,
    encoded: Vec<Vec<u8>>,
}
pub struct MacaOffer {
    status: MacaStatus,
    version: ProtocolVersion,
    format: MeasurementFormat,
    targets: MeasurementTargets,
}
impl MacaPlan {
    pub fn try_new(
        serving: ControlChannel,
        bmi_version: ProtocolVersion,
        status: MacaStatus,
        triggers: ReportTriggers,
        current: Vec<u16>,
        other: Vec<HyperbandChannels>,
    ) -> Result<Self, Error> {
        if bmi_version.code() < ProtocolVersion::Is136Rev0.code() {
            return Err(Error::Field("maca_bmi_version"));
        }
        MeasurementTargets::try_new(serving, current.clone(), other.clone())?;
        if status == MacaStatus::Disabled {
            return Err(Error::Field("maca_disabled_plan"));
        }
        let basic = MacaBroadcast {
            status,
            triggers,
            unrestricted: (current.len() > UNRESTRICTED_THRESHOLD).then_some(true),
            current_channels: (!current.is_empty()).then_some(current),
            enhanced: (bmi_version.code() >= ProtocolVersion::Pv2.code()).then_some(
                EnhancedControl {
                    dpc: false,
                    extended: true,
                },
            ),
            ..Default::default()
        };
        let multi = (!other.is_empty()).then(|| MacaBroadcast {
            kind: BroadcastKind::MultiHyperband,
            status,
            triggers,
            unrestricted: (basic.current_channels.as_ref().map_or(0, Vec::len)
                + other.iter().map(|list| list.channels.len()).sum::<usize>()
                > UNRESTRICTED_THRESHOLD)
                .then_some(true),
            other_channels: other,
            enhanced: basic.enhanced,
            ..Default::default()
        });
        let mut encoded = vec![basic.encode()?];
        if let Some(multi) = &multi {
            multi.resolve_targets(serving, Some(&basic))?;
            encoded.push(multi.encode()?);
        }
        Ok(Self {
            serving,
            basic,
            multi,
            encoded,
        })
    }
    pub(crate) fn from_broadcasts(
        serving: ControlChannel,
        version: ProtocolVersion,
        basic: MacaBroadcast,
        multi: Option<MacaBroadcast>,
    ) -> Result<Self, Error> {
        if basic.kind != BroadcastKind::Basic {
            return Err(Error::Field("maca_base_kind"));
        }
        basic.validate_version(version)?;
        basic.resolve_targets(serving, None)?;
        let mut encoded = vec![basic.encode()?];
        if let Some(value) = &multi {
            if value.kind != BroadcastKind::MultiHyperband {
                return Err(Error::Field("maca_multi_kind"));
            }
            value.validate_version(version)?;
            value.resolve_targets(serving, Some(&basic))?;
            encoded.push(value.encode()?);
        }
        Ok(Self {
            serving,
            basic,
            multi,
            encoded,
        })
    }
    pub fn messages(&self) -> &[Vec<u8>] {
        &self.encoded
    }
    /// TIA-136-123-E §5.1.1.2.9 selects the multi-hyperband message only when at least one listed channel is supported.
    pub fn offer(
        &self,
        trigger: AccessTrigger,
        version: ProtocolVersion,
        supports: impl Fn(Hyperband, u16) -> bool,
    ) -> Result<Option<MacaOffer>, Error> {
        if version.code() < ProtocolVersion::Is136Rev0.code() {
            return Err(Error::Field("maca_handset_version"));
        }
        let selected = self.multi.as_ref().filter(|multi| {
            multi.other_channels.iter().any(|list| {
                list.channels
                    .iter()
                    .any(|channel| supports(list.hyperband, *channel))
            })
        });
        let message = selected.unwrap_or(&self.basic);
        let enabled = match trigger {
            AccessTrigger::Registration => message.triggers.registration,
            AccessTrigger::Origination => message.triggers.origination,
            AccessTrigger::PageResponse => message.triggers.page_response,
            AccessTrigger::Audit => message.triggers.audit,
        };
        if !enabled || message.status == MacaStatus::Disabled {
            return Ok(None);
        }

        let targets = message.resolve_targets(self.serving, selected.map(|_| &self.basic))?;
        let format = if version.code() >= ProtocolVersion::Pv2.code() {
            message.advertised_format()
        } else {
            MeasurementFormat::Basic
        };
        Ok(Some(MacaOffer {
            status: message.status,
            version,
            format,
            targets,
        }))
    }
}
impl MacaOffer {
    pub fn targets(&self) -> &MeasurementTargets {
        &self.targets
    }
    pub fn format(&self) -> MeasurementFormat {
        self.format
    }
    /// TIA-136-123-E §5.4.6 omits band identifiers. Interpret each report using the offer retained for that access.
    pub fn short_term_readings(&self, report: &MacaReport) -> Result<Vec<ChannelReading>, Error> {
        report.validate_negotiation(self.version, self.format)?;
        if let Some(fields) = report.measurements.fields()
            && (self.status == MacaStatus::LongTerm
                && (fields.current.is_some() || !fields.other.is_empty())
                || self.status == MacaStatus::ShortTerm && fields.long_term.is_some())
        {
            return Err(Error::Field("maca_report_status"));
        }
        report.short_term_readings(&self.targets)
    }
}

#[cfg(test)]
mod tests;
