use super::{Config, ConfigBand};
use crate::{
    core::maca::MacaPlan,
    l3::{
        legacy_traffic::{ControlChannel, Hyperband},
        maca::{
            Error, HyperbandChannels,
            broadcast::{MacaStatus, ReportTriggers},
        },
    },
};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacaMode {
    LongTerm,
    #[default]
    ShortTerm,
    Both,
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementBand {
    Cellular800,
    Pcs1900,
    Cellular700,
    Imt2000,
}
impl MeasurementBand {
    fn hyperband(self) -> Hyperband {
        match self {
            Self::Cellular800 => Hyperband::Cellular800,
            Self::Pcs1900 => Hyperband::Pcs1900,
            Self::Cellular700 => Hyperband::Cellular700,
            Self::Imt2000 => Hyperband::Imt2000,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtherChannels {
    pub band: MeasurementBand,
    pub channels: Vec<u16>,
}
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MacaTriggers {
    pub registration: bool,
    pub origination: bool,
    pub page_response: bool,
    pub audit: bool,
}
impl Default for MacaTriggers {
    fn default() -> Self {
        Self {
            registration: true,
            origination: true,
            page_response: true,
            audit: true,
        }
    }
}
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MacaConfig {
    pub mode: MacaMode,
    pub triggers: MacaTriggers,
    pub current_channels: Vec<u16>,
    pub other_hyperbands: Vec<OtherChannels>,
}
impl Config {
    pub(crate) fn maca_serving(&self) -> ControlChannel {
        ControlChannel {
            hyperband: match self.cell.band {
                ConfigBand::Cellular800 => Hyperband::Cellular800,
                ConfigBand::Pcs1900 => Hyperband::Pcs1900,
            },
            channel: self.cell.channel,
            dvcc: self.cell.dvcc,
        }
    }
    pub(crate) fn maca_plan(&self) -> Result<Option<MacaPlan>, Error> {
        let Some(config) = &self.maca else {
            return Ok(None);
        };
        let triggers = ReportTriggers {
            registration: config.triggers.registration,
            origination: config.triggers.origination,
            page_response: config.triggers.page_response,
            audit: config.triggers.audit,
        };
        if triggers == ReportTriggers::default() {
            return Err(Error::Field("maca.triggers"));
        }
        MacaPlan::try_new(
            self.maca_serving(),
            crate::runtime::capabilities::SERVING_PROTOCOL,
            match config.mode {
                MacaMode::LongTerm => MacaStatus::LongTerm,
                MacaMode::ShortTerm => MacaStatus::ShortTerm,
                MacaMode::Both => MacaStatus::Both,
            },
            triggers,
            config.current_channels.clone(),
            config
                .other_hyperbands
                .iter()
                .map(|list| HyperbandChannels {
                    hyperband: list.band.hyperband(),
                    channels: list.channels.clone(),
                })
                .collect(),
        )
        .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BASE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    ));
    #[test]
    fn omitted_maca_is_disabled_and_enabled_configuration_preserves_order() {
        assert!(
            Config::from_yaml(BASE)
                .unwrap()
                .maca_plan()
                .unwrap()
                .is_none()
        );
        let config = Config::from_yaml(&format!("{BASE}\nmaca:\n  mode: both\n  current_channels: [993, 992]\n  other_hyperbands:\n    - band: pcs1900\n      channels: [101, 100]\n")).unwrap();
        let plan = config.maca_plan().unwrap().unwrap();
        let offer = plan
            .offer(
                crate::core::maca::AccessTrigger::Registration,
                crate::l3::broadcast::ProtocolVersion::Pv2,
                |_, _| true,
            )
            .unwrap()
            .unwrap();
        assert_eq!(offer.targets().current_channels(), [993, 992]);
        assert_eq!(offer.targets().other_channels()[0].channels, [101, 100]);
    }
    #[test]
    fn invalid_maca_lists_triggers_and_unknown_settings_fail_before_radio_open() {
        for yaml in [
            "current_channels: [1]",
            "current_channels: [2048]",
            "current_channels: [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16]",
            "other_hyperbands: [{band: cellular800, channels: [992]}]",
            "other_hyperbands: [{band: pcs1900, channels: [1]}, {band: pcs1900, channels: [2]}]",
            "triggers: {registration: false, origination: false, page_response: false, audit: false}",
            "mode: disabled",
            "extended: false",
        ] {
            assert!(
                Config::from_yaml(&format!("{BASE}\nmaca:\n  {yaml}\n")).is_err(),
                "{yaml}"
            );
        }
    }
}
