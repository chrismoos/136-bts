mod maca;
pub use crate::realtime::Config as RealtimeConfig;
mod overhead;
use crate::{
    core::{
        registry::{LocalRegistry, ProvisionedSubscriber, RegistryError},
        store::{StateStore, StoreError},
    },
    l3::designation::AssignedSlots,
    link::rach::MobileIdentity,
    phy::{
        channel::{self, Band, ChannelError, DuplexFrequencies},
        pulse::SYMBOL_RATE_HZ as PROTOCOL_SYMBOL_RATE_HZ,
    },
    resources::{AllocationError, TrafficResources},
};
pub use maca::{MacaConfig, MacaMode, MacaTriggers, MeasurementBand, OtherChannels};
pub use overhead::{OperatorIdentity, RegistrationConfig};
use sdr_hal::config::RadioConfig;
use serde::Deserialize;
use std::{error::Error, fmt, path::PathBuf};

const SYMBOL_RATE_HZ: usize = PROTOCOL_SYMBOL_RATE_HZ as usize;
pub const SAMPLES_PER_SYMBOL: usize = 16;
pub const SAMPLE_RATE_HZ: usize = SYMBOL_RATE_HZ * SAMPLES_PER_SYMBOL;
// The forward modulator caps oversampling at 32.
const SUPPORTED_SAMPLES_PER_SYMBOL: std::ops::RangeInclusive<usize> = 8..=32;
const SID_BITS: usize = 15;
const PRIMARY_CONTROL_SLOT: u8 = 1;
const DEFAULT_MOBILE_POWER_LEVEL: u8 = 7;
pub(crate) const MAX_MOBILE_POWER_LEVEL: u8 = crate::l3::legacy_traffic::MAX_POWER_LEVEL;
fn default_mobile_power_level() -> u8 {
    DEFAULT_MOBILE_POWER_LEVEL
}
fn default_control_slot() -> u8 {
    PRIMARY_CONTROL_SLOT
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigBand {
    Cellular800,
    Pcs1900,
}
impl ConfigBand {
    pub(crate) fn band(self) -> Band {
        match self {
            Self::Cellular800 => Band::Cellular800,
            Self::Pcs1900 => Band::Pcs1900,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellConfig {
    pub band: ConfigBand,
    pub channel: u16,
    pub sid: u16,
    #[serde(default)]
    pub network_name: Option<String>,
    #[serde(default)]
    pub operator: Option<OperatorIdentity>,
    #[serde(default)]
    pub neighbors: Vec<crate::l3::neighbors::Neighbor>,
    pub dvcc: u8,
    #[serde(default = "default_mobile_power_level")]
    pub access_power_level: u8,
    #[serde(default = "default_mobile_power_level")]
    pub traffic_power_level: u8,
    #[serde(default = "default_control_slot")]
    pub control_slot: u8,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdentityConfig {
    Min34 { value: u64 },
    Imsi50 { value: u64 },
    Tmsi20 { value: u32 },
    Tmsi24 { value: u32 },
}
impl IdentityConfig {
    fn identity(&self) -> MobileIdentity {
        match *self {
            Self::Min34 { value } => MobileIdentity::Min34(value),
            Self::Imsi50 { value } => MobileIdentity::Imsi50(value),
            Self::Tmsi20 { value } => MobileIdentity::Tmsi20(value),
            Self::Tmsi24 { value } => MobileIdentity::Tmsi24(value),
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriberConfig {
    pub identity: IdentityConfig,
    pub number: String,
}
const DEFAULT_STATE_PATH: &str = "136-bts.sqlite3";
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreConfig {
    pub state_path: PathBuf,
}
impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            state_path: PathBuf::from(DEFAULT_STATE_PATH),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub filter: String,
    pub format: LogFormat,
}
impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            filter: "info".into(),
            format: LogFormat::Text,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingbackStyle {
    #[default]
    Us,
    Eu,
}

const DEFAULT_ANSWER_SECONDS: u64 = 60;
const DEFAULT_RADIO_LOSS_SECONDS: u64 = 5;
const MAX_MOBILE_ALERT_SECONDS: u64 = crate::core::call_session::MAX_ANSWER_TIMEOUT_MS / 1000;
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CallConfig {
    pub ringback: RingbackStyle,
    pub answer_timeout_seconds: u64,
    pub radio_loss_timeout_seconds: u64,
}
impl Default for CallConfig {
    fn default() -> Self {
        Self {
            ringback: RingbackStyle::default(),
            answer_timeout_seconds: DEFAULT_ANSWER_SECONDS,
            radio_loss_timeout_seconds: DEFAULT_RADIO_LOSS_SECONDS,
        }
    }
}
const DEFAULT_WEB_PORT: u16 = 8080;
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebConfig {
    pub enabled: bool,
    pub listen: std::net::SocketAddr,
    pub allowed_origins: Vec<String>,
}
impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: std::net::SocketAddr::from(([127, 0, 0, 1], DEFAULT_WEB_PORT)),
            allowed_origins: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeSource {
    #[default]
    LocalSystem,
}
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeConfig {
    pub source: TimeSource,
}
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmsDisplayTime {
    Temporary,
    Default,
    #[default]
    Invoke,
}
impl SmsDisplayTime {
    pub(crate) fn wire(self) -> crate::l3::sms::DisplayTime {
        match self {
            Self::Temporary => crate::l3::sms::DisplayTime::Temporary,
            Self::Default => crate::l3::sms::DisplayTime::Default,
            Self::Invoke => crate::l3::sms::DisplayTime::Invoke,
        }
    }
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmsConfig {
    #[serde(default, alias = "operator_display_time")]
    pub display_time: SmsDisplayTime,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub realtime: RealtimeConfig,
    #[serde(default)]
    pub sms_hub: crate::sms_hub::SmsHubConfig,
    #[serde(default)]
    pub sip: crate::sip::SipConfig,
    #[serde(default)]
    pub sms: SmsConfig,
    #[serde(default)]
    pub registration: RegistrationConfig,
    #[serde(default)]
    pub time: TimeConfig,
    #[serde(default)]
    pub maca: Option<MacaConfig>,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub calls: CallConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    pub radio: RadioConfig,
    pub cell: CellConfig,
    #[serde(default)]
    pub core: CoreConfig,
    #[serde(default)]
    pub subscribers: Vec<SubscriberConfig>,
}
#[derive(Debug)]
pub enum ConfigError {
    Yaml(serde_yaml_ng::Error),
    Field(&'static str),
    Channel(ChannelError),
    Registry(RegistryError),
    Allocation(AllocationError),
    Store(StoreError),
    Maca(crate::l3::maca::Error),
    Broadcast(crate::scheduler::broadcast::Error),
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yaml(error) => write!(f, "YAML configuration: {error}"),
            Self::Field(field) => write!(f, "unsupported configuration value for {field}"),
            Self::Channel(error) => error.fmt(f),
            Self::Registry(error) => error.fmt(f),
            Self::Allocation(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
            Self::Maca(error) => error.fmt(f),
            Self::Broadcast(error) => error.fmt(f),
        }
    }
}
impl Error for ConfigError {}
#[derive(Debug)]
pub struct ConfiguredState {
    pub frequencies: DuplexFrequencies,
    pub registry: LocalRegistry,
    pub traffic: TrafficResources,
}
impl Config {
    pub fn from_yaml(text: &str) -> Result<Self, ConfigError> {
        let config: Self = serde_yaml_ng::from_str(text).map_err(ConfigError::Yaml)?;
        config.prepare()?;
        Ok(config)
    }
    /// TX and RX sample rate requested from the radio. `radio.tx_sample_rate_hz` sets it, and
    /// zero selects 388800.
    pub fn sample_rate_hz(&self) -> usize {
        match self.radio.tx_sample_rate_hz {
            0 => SAMPLE_RATE_HZ,
            rate => rate,
        }
    }
    pub fn open_store(&self) -> Result<StateStore, ConfigError> {
        let prepared = self.prepare()?;
        let provisioning: Vec<_> = prepared
            .registry
            .subscribers()
            .iter()
            .map(|subscriber| subscriber.provisioning.clone())
            .collect();
        let mut store = StateStore::open(&self.core.state_path).map_err(ConfigError::Store)?;
        store.seed_once(&provisioning).map_err(ConfigError::Store)?;
        Ok(store)
    }

    pub fn open_replicated_store(&self) -> Result<StateStore, ConfigError> {
        drop(self.open_store()?);
        StateStore::open_replicated(&self.core.state_path).map_err(ConfigError::Store)
    }

    pub fn prepare(&self) -> Result<ConfiguredState, ConfigError> {
        self.realtime.validate().map_err(ConfigError::Field)?;
        self.sip.validate().map_err(ConfigError::Field)?;
        self.sms_hub.validate().map_err(ConfigError::Field)?;
        self.registration.validate()?;
        if !self.cell.neighbors.is_empty() {
            crate::l3::neighbors::encode(&self.cell.neighbors)
                .map_err(|_| ConfigError::Field("cell.neighbors"))?;
            let mut channels = std::collections::HashSet::new();
            for neighbor in &self.cell.neighbors {
                if neighbor.channel == self.cell.channel
                    || !channels.insert(neighbor.channel)
                    || neighbor.dvcc == crate::phy::color::RESERVED_TRAFFIC_DVCC
                    || neighbor.access_power_level > MAX_MOBILE_POWER_LEVEL
                {
                    return Err(ConfigError::Field("cell.neighbors"));
                }
                channel::frequencies(self.cell.band.band(), neighbor.channel)
                    .map_err(ConfigError::Channel)?;
            }
        }
        if let Some(operator) = &self.cell.operator {
            operator.validate()?;
        }
        if let Some(name) = &self.cell.network_name {
            crate::l3::broadcast::validate_network_name(name)
                .map_err(|_| ConfigError::Field("cell.network_name"))?;
        }

        {
            let mut optional = crate::runtime::serving::optional_messages()
                .map_err(|_| ConfigError::Field("service_menu"))?;
            if let Some(operator) = &self.cell.operator {
                optional.push(
                    operator
                        .message()
                        .encode()
                        .map_err(|_| ConfigError::Field("cell.operator.soc"))?
                        .to_vec(),
                );
            }
            if let Some(plan) = self.maca_plan().map_err(ConfigError::Maca)? {
                optional.extend_from_slice(plan.messages());
            }
            let content = crate::scheduler::broadcast::BroadcastContent::try_new(
                AssignedSlots::Full14,
                crate::runtime::serving::messages(self),
                optional,
            )
            .map_err(ConfigError::Broadcast)?;
            if !self.cell.neighbors.is_empty() {
                content
                    .with_neighbors(&self.cell.neighbors)
                    .map_err(ConfigError::Broadcast)?;
            }
        }
        if self.cell.dvcc == crate::phy::color::RESERVED_TRAFFIC_DVCC {
            return Err(ConfigError::Field("cell.dvcc"));
        }
        if self.cell.access_power_level > MAX_MOBILE_POWER_LEVEL
            || self.cell.traffic_power_level > MAX_MOBILE_POWER_LEVEL
        {
            return Err(ConfigError::Field("cell mobile power level"));
        }
        if self.calls.answer_timeout_seconds == 0
            || self.calls.answer_timeout_seconds > MAX_MOBILE_ALERT_SECONDS
        {
            return Err(ConfigError::Field("calls.answer_timeout_seconds"));
        }
        if self.calls.radio_loss_timeout_seconds == 0
            || self
                .calls
                .radio_loss_timeout_seconds
                .checked_mul(1000)
                .is_none()
        {
            return Err(ConfigError::Field("calls.radio_loss_timeout_seconds"));
        }
        let rate_hz = self.sample_rate_hz();
        if !rate_hz.is_multiple_of(SYMBOL_RATE_HZ)
            || !SUPPORTED_SAMPLES_PER_SYMBOL.contains(&(rate_hz / SYMBOL_RATE_HZ))
        {
            return Err(ConfigError::Field("radio.tx_sample_rate_hz"));
        }
        if self.radio.rx_sample_rate_hz.is_some() {
            return Err(ConfigError::Field("radio.rx_sample_rate_hz"));
        }

        if self.core.state_path.as_os_str().is_empty()
            || self.core.state_path == std::path::Path::new(":memory:")
        {
            return Err(ConfigError::Field("core.state_path"));
        }
        if self.cell.sid >= 1 << SID_BITS {
            return Err(ConfigError::Field("cell.sid"));
        }
        // IS-136.1-A Table 4-3 assigns the single full-rate DCCH to slots 1 and 4.
        if self.cell.control_slot != PRIMARY_CONTROL_SLOT {
            return Err(ConfigError::Field("cell.control_slot"));
        }
        let control_pair = AssignedSlots::Full14;
        let band = self.cell.band.band();
        let frequencies =
            channel::frequencies(band, self.cell.channel).map_err(ConfigError::Channel)?;
        let provisioning = self
            .subscribers
            .iter()
            .map(|subscriber| ProvisionedSubscriber {
                identity: subscriber.identity.identity(),
                number: subscriber.number.clone(),
            })
            .collect();
        let registry = LocalRegistry::try_new(provisioning).map_err(ConfigError::Registry)?;
        let traffic = TrafficResources::try_new(band, self.cell.channel, &[control_pair])
            .map_err(ConfigError::Allocation)?;
        Ok(ConfiguredState {
            frequencies,
            registry,
            traffic,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const YAML: &str = "radio:\n  kind: noop\ncell:\n  band: cellular800\n  channel: 1\n  sid: 42\n  dvcc: 1\nsubscribers:\n  - identity: {kind: min34, value: 1}\n    number: '1001'\n";
    #[test]
    fn time_source_defaults_to_local_system_and_rejects_other_sources() {
        let base = include_str!("../../../config/noop.yaml");
        assert!(matches!(
            Config::from_yaml(base).unwrap().time.source,
            TimeSource::LocalSystem
        ));
        assert!(Config::from_yaml(&format!("{base}\ntime:\n  source: local_system\n")).is_ok());
        assert!(Config::from_yaml(&format!("{base}\ntime:\n  source: ntp\n")).is_err());
        assert!(Config::from_yaml(&format!("{base}\ntime:\n  timezone: UTC\n")).is_err());
    }
    #[test]
    fn yaml_reuses_hal_backend_and_prepares_validated_resources() {
        let config = Config::from_yaml(YAML).unwrap();
        assert!(matches!(
            config.radio.device,
            sdr_hal::config::DeviceConfig::Noop {}
        ));

        let mut state = config.prepare().unwrap();
        assert_eq!(state.frequencies.forward_hz, 870030000);
        assert_eq!(state.registry.subscribers().len(), 1);
        assert_eq!(
            state.traffic.allocate_full_rate().unwrap().assigned_slots(),
            AssignedSlots::Full25
        );
    }
    #[test]
    fn shipped_example_and_uhd_schema_prepare_without_opening_hardware() {
        let config = Config::from_yaml(include_str!("../../../config/noop.yaml")).unwrap();
        let mut state = config.prepare().unwrap();
        assert!(state.registry.subscribers().is_empty());
        assert_eq!(
            state.traffic.allocate_full_rate().unwrap().assigned_slots(),
            AssignedSlots::Full25
        );
        assert_eq!(
            state.traffic.allocate_full_rate().unwrap().assigned_slots(),
            AssignedSlots::Full36
        );
        let template = Config::from_yaml(include_str!("../../../config/uhd.yaml")).unwrap();
        assert!(matches!(
            template.radio.device,
            sdr_hal::config::DeviceConfig::Uhd { .. }
        ));
        assert_eq!(template.cell.access_power_level, 7);
        let uhd = YAML.replace("kind: noop", "kind: uhd\n  device: ''\n  channel: 0\n  antenna: TX/RX\n  master_clock_rate: 20000000");
        assert!(matches!(
            Config::from_yaml(&uhd).unwrap().radio.device,
            sdr_hal::config::DeviceConfig::Uhd { .. }
        ));
    }

    #[test]
    fn opening_configured_store_reconciles_subscribers_without_starting_radio() {
        let mut config = Config::from_yaml(YAML).unwrap();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "136bts-config-{}-{stamp}.sqlite3",
            std::process::id()
        ));
        config.core.state_path = path.clone();
        {
            let store = config.open_store().unwrap();
            assert_eq!(store.enabled_subscribers().unwrap().len(), 1);
        }
        {
            let store = config.open_store().unwrap();
            assert_eq!(store.enabled_subscribers().unwrap()[0].number, "1001");
        }
        std::fs::remove_file(path).unwrap();
        assert!(Config::from_yaml(&format!("{YAML}core: {{state_path: ':memory:'}}\n")).is_err());
    }

    #[test]
    fn unknown_fields_duplicates_and_unsupported_operating_values_are_rejected() {
        for invalid in [
            YAML.replace("channel: 1", "channel: 990"),
            YAML.replace("sid: 42", "sid: 32768"),
            YAML.replace("dvcc: 1", "dvcc: 0"),
            YAML.replace("dvcc: 1", "dvcc: 1\n  control_slot: 2"),
            YAML.replace("dvcc: 1", "dvcc: 1\n  control_slot: 3"),
            YAML.replace("dvcc: 1", "dvcc: 1\n  control_slot: 4"),
            YAML.replace("kind: noop", "kind: noop\n  typo: 1"),
            format!("{YAML}modem: {{samples_per_symbol: 32}}\n"),
            format!("{YAML}calls: {{answer_timeout_seconds: 0}}\n"),
            format!("{YAML}calls: {{answer_timeout_seconds: 66}}\n"),
            format!("{YAML}calls: {{radio_loss_timeout_seconds: 0}}\n"),
            YAML.replace("dvcc: 1", "dvcc: 1\n  access_power_level: 11"),
            YAML.replace("kind: noop", "kind: noop\n  tx_sample_rate_hz: 200000"),
            YAML.replace("kind: noop", "kind: noop\n  tx_sample_rate_hz: 170100"),
            YAML.replace("kind: noop", "kind: noop\n  tx_sample_rate_hz: 801900"),
            YAML.replace("kind: noop", "kind: noop\n  rx_sample_rate_hz: 388800"),
            format!("{YAML}ignored_setting: true\n"),
            format!("{YAML}radio: {{kind: noop}}\n"),
        ] {
            assert!(
                Config::from_yaml(&invalid).is_err(),
                "accepted invalid config: {invalid}"
            );
        }
    }
}

#[cfg(test)]
#[test]
fn ringback_configuration_defaults_to_us_and_preserves_independent_sip_control() {
    let base = include_str!("../../../config/noop.yaml").replace("  ringback: us\n", "");
    let defaults = Config::from_yaml(&base).unwrap();
    assert_eq!(defaults.calls.ringback, RingbackStyle::Us);
    assert!(defaults.sip.generate_ringback);
    let configured = base
        .replace("calls:\n", "calls:\n  ringback: eu\n")
        .replace("sip:\n", "sip:\n  generate_ringback: false\n");
    let config = Config::from_yaml(&configured).unwrap();
    assert_eq!(config.calls.ringback, RingbackStyle::Eu);
    assert!(!config.sip.generate_ringback);
    assert!(Config::from_yaml(&configured.replace("ringback: eu", "ringback: invalid")).is_err());
}
