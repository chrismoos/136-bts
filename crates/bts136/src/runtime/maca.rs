use super::access::maca::{MacaCompanion, MacaSource};
use crate::{
    core::{
        maca::MacaPlan,
        registry::LocalRegistry,
        store::{StateStore, broadcast::CellKey},
    },
    l3::{
        broadcast::{MandatoryBroadcast, ProtocolVersion},
        capability_report::CapabilityReport,
        legacy_traffic::{ControlChannel, Hyperband},
        maca::{
            ChannelReading, LongTermMeasurement, MeasurementFormat,
            broadcast::{BroadcastKind, MacaBroadcast},
        },
    },
    link::rach::MessageSet,
    scheduler::broadcast::BroadcastContent,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Measurements {
    pub readings: Vec<ChannelReading>,
    pub long_term: Option<LongTermMeasurement>,
    pub format: Option<MeasurementFormat>,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Association {
    Observed(Measurements),
    Unmapped(&'static str),
}
pub(crate) struct MacaRuntime {
    history: Vec<MacaPlan>,
    serving: ControlChannel,
}
impl MacaRuntime {
    pub fn try_new(
        store: &StateStore,
        cell: CellKey,
        serving: ControlChannel,
        desired: &BroadcastContent,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut runtime = Self {
            history: vec![],
            serving,
        };
        runtime.retain(desired)?;
        let recovery = store.broadcast_recovery(cell)?;
        let mut revision = recovery.pending.or(recovery.confirmed);
        while let Some(current) = revision {
            runtime.retain(&BroadcastContent::from_storage_bytes(current.contents())?)?;
            revision = store.broadcast_predecessor(&current)?;
        }
        Ok(runtime)
    }
    fn retain(&mut self, content: &BroadcastContent) -> Result<(), crate::l3::maca::Error> {
        if let Some(plan) = plan(self.serving, content)?
            && !self
                .history
                .iter()
                .any(|old| old.messages() == plan.messages())
        {
            self.history.push(plan);
        }
        Ok(())
    }
    pub fn associate(
        &self,
        content: &BroadcastContent,
        registry: &LocalRegistry,
        primary: &MessageSet,
        companion: &MacaCompanion,
    ) -> Association {
        let subscriber = registry
            .subscribers()
            .iter()
            .find(|entry| entry.provisioning.identity == primary.identity);
        let version = companion.version.or_else(|| {
            subscriber.and_then(|entry| {
                entry
                    .registration
                    .map(|registration| registration.protocol_version)
            })
        });
        let fresh_capabilities = primary
            .messages
            .iter()
            .find(|message| message.first() == Some(&crate::l3::capability_report::MESSAGE_TYPE))
            .and_then(|message| crate::l3::capability_report::decode(message).ok());
        let capabilities = fresh_capabilities.as_ref().or_else(|| {
            subscriber
                .and_then(|entry| entry.capabilities.as_ref().map(|reported| &reported.report))
        });
        self.associate_with_capabilities(content, companion, version, capabilities)
    }
    fn associate_with_capabilities(
        &self,
        content: &BroadcastContent,
        companion: &MacaCompanion,
        version: Option<ProtocolVersion>,
        capabilities: Option<&CapabilityReport>,
    ) -> Association {
        let MacaSource::Broadcast(trigger) = companion.source else {
            return Association::Unmapped("No matching queued request");
        };
        let Some(version) = version else {
            return Association::Unmapped("Handset protocol version unavailable");
        };
        let Ok(Some(active)) = plan(self.serving, content) else {
            return Association::Unmapped("MACA is not configured");
        };
        let supports = |band: Hyperband, channel| {
            capabilities.is_some_and(|report| report.supports_dtc_channel(band, channel))
        };
        if ![false, true].into_iter().any(|unknown_supports| {
            active
                .offer(trigger, version, |band, channel| {
                    if capabilities.is_some() {
                        supports(band, channel)
                    } else {
                        unknown_supports
                    }
                })
                .ok()
                .flatten()
                .is_some()
        }) {
            return Association::Unmapped("MACA was not requested for this access");
        }
        let mut accepted = None;
        // TIA-136-123-E §5.4.6 has no revision. Older retry-compatible lists must not assign different channel identities.
        for plan in &self.history {
            for unknown_supports in [false, true] {
                let Ok(Some(offer)) = plan.offer(trigger, version, |band, channel| {
                    if capabilities.is_some() {
                        supports(band, channel)
                    } else {
                        unknown_supports
                    }
                }) else {
                    continue;
                };
                let Ok(readings) = offer.short_term_readings(&companion.report) else {
                    continue;
                };
                let measurements = Measurements {
                    readings,
                    long_term: companion
                        .report
                        .measurements
                        .fields()
                        .and_then(|fields| fields.long_term),
                    format: companion.report.measurements.format(),
                };
                if accepted.as_ref().is_some_and(|old| old != &measurements) {
                    return Association::Unmapped("Ambiguous historical target lists");
                }
                accepted = Some(measurements);
            }
        }
        accepted
            .map(Association::Observed)
            .unwrap_or(Association::Unmapped(
                "Report does not match an advertised offer",
            ))
    }
}
fn plan(
    serving: ControlChannel,
    content: &BroadcastContent,
) -> Result<Option<MacaPlan>, crate::l3::maca::Error> {
    let mut basic = None;
    let mut multi = None;
    for bytes in content.optional_messages() {
        let slot = if bytes.first() == Some(&BroadcastKind::Basic.header()) {
            &mut basic
        } else if bytes.first() == Some(&BroadcastKind::MultiHyperband.header()) {
            &mut multi
        } else {
            continue;
        };
        if slot.replace(MacaBroadcast::decode(bytes)?).is_some() {
            return Err(crate::l3::maca::Error::Field("duplicate_maca_broadcast"));
        }
    }
    let Some(basic) = basic else {
        if multi.is_some() {
            return Err(crate::l3::maca::Error::Field("missing_maca_base"));
        }
        return Ok(None);
    };
    let MandatoryBroadcast::SystemIdentity(identity) = &content.messages()[4] else {
        return Err(crate::l3::maca::Error::Field("maca_bmi_version"));
    };
    MacaPlan::from_broadcasts(serving, identity.protocol_version, basic, multi).map(Some)
}

#[cfg(test)]
mod tests;
