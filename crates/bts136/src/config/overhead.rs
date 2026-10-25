use super::ConfigError;
use crate::l3::{
    broadcast::{MAX_REG_PERIOD_CODE, REG_PERIOD_UNIT_SUPERFRAMES},
    operator_identity::OperatorIdentity as Message,
};
use serde::Deserialize;
const DEFAULT_PERIOD_SUPERFRAMES: u32 = 940;
const MAX_RNUM: u16 = 1023;

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RegistrationConfig {
    pub allow_unknown: bool,
    pub home: bool,
    pub roaming: bool,
    pub period_superframes: Option<u32>,
    pub area: Option<u16>,
}
impl Default for RegistrationConfig {
    fn default() -> Self {
        Self {
            allow_unknown: true,
            home: true,
            roaming: true,
            period_superframes: Some(DEFAULT_PERIOD_SUPERFRAMES),
            area: None,
        }
    }
}
impl RegistrationConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if let Some(period) = self.period_superframes
            && (period == 0
                || period % REG_PERIOD_UNIT_SUPERFRAMES != 0
                || period / REG_PERIOD_UNIT_SUPERFRAMES > u32::from(MAX_REG_PERIOD_CODE) + 1)
        {
            return Err(ConfigError::Field("registration.period_superframes"));
        }
        if self.area.is_some_and(|area| area > MAX_RNUM) {
            return Err(ConfigError::Field("registration.area"));
        }
        Ok(())
    }
    pub(crate) fn period_code(&self) -> Option<u16> {
        self.period_superframes
            .map(|period| (period / REG_PERIOD_UNIT_SUPERFRAMES).saturating_sub(1) as u16)
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorIdentity {
    pub soc: u16,
    #[serde(default)]
    pub bsmc: u8,
}
impl OperatorIdentity {
    pub(crate) fn message(&self) -> Message {
        Message {
            soc: self.soc,
            bsmc: self.bsmc,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        self.message()
            .encode()
            .map(|_| ())
            .map_err(|_| ConfigError::Field("cell.operator.soc"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registration_policy_defaults_and_wire_limits() {
        let config = RegistrationConfig::default();
        assert!(config.allow_unknown && config.home && config.roaming);
        let closed: RegistrationConfig = serde_yaml_ng::from_str("allow_unknown: false").unwrap();
        assert!(!closed.allow_unknown);
        assert_eq!(config.period_code(), Some(9));
        for period in [0, 1, 95, 48_129, u32::MAX] {
            assert!(
                RegistrationConfig {
                    period_superframes: Some(period),
                    ..RegistrationConfig::default()
                }
                .validate()
                .is_err()
            );
        }
        for period in [94, 188, 48_128] {
            assert!(
                RegistrationConfig {
                    period_superframes: Some(period),
                    ..RegistrationConfig::default()
                }
                .validate()
                .is_ok()
            );
        }
        assert!(
            RegistrationConfig {
                area: Some(1024),
                ..RegistrationConfig::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            RegistrationConfig {
                area: Some(1023),
                period_superframes: None,
                ..RegistrationConfig::default()
            }
            .validate()
            .is_ok()
        );
    }
    #[test]
    fn invalid_neighbors_and_operator_identity_fail_before_radio_preparation() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/two-phones.yaml"
        ));
        for channels in [vec![1], vec![2, 2], vec![0]] {
            let mut config = super::super::Config::from_yaml(source).unwrap();
            config.cell.neighbors = channels
                .into_iter()
                .map(crate::l3::neighbors::tests::neighbor)
                .collect();
            assert!(config.prepare().is_err());
        }
        let mut config = super::super::Config::from_yaml(source).unwrap();
        config.cell.operator = Some(OperatorIdentity { soc: 4096, bsmc: 0 });
        assert!(config.prepare().is_err());
        config.cell.operator = Some(OperatorIdentity { soc: 0, bsmc: 0 });
        config.cell.neighbors = (2..=25)
            .map(crate::l3::neighbors::tests::neighbor)
            .collect();
        config.registration.area = Some(1023);
        config.registration.period_superframes = None;
        assert!(config.prepare().is_ok());
    }
}
