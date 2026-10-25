use serde::Deserialize;
use std::net::SocketAddr;
const SMPP_PORT: u16 = 2775;
const SYSTEM_ID_MAX: usize = 15;
const PASSWORD_MAX: usize = 8;
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BindRole {
    #[default]
    Server,
    Client,
}
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmsHubConfig {
    pub enabled: bool,
    pub role: BindRole,
    pub listen_addr: SocketAddr,
    pub hub_addr: SocketAddr,
    pub system_id: String,
    pub password: String,
}
impl Default for SmsHubConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            role: BindRole::Server,
            listen_addr: SocketAddr::from(([127, 0, 0, 1], SMPP_PORT)),
            hub_addr: SocketAddr::from(([100, 71, 0, 1], SMPP_PORT)),
            system_id: String::new(),
            password: String::new(),
        }
    }
}
impl std::fmt::Debug for SmsHubConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmsHubConfig")
            .field("enabled", &self.enabled)
            .field("role", &self.role)
            .field("listen_addr", &self.listen_addr)
            .field("hub_addr", &self.hub_addr)
            .field("system_id", &self.system_id)
            .finish_non_exhaustive()
    }
}
impl SmsHubConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.enabled {
            return Ok(());
        }
        if self.system_id.is_empty()
            || self.system_id.len() > SYSTEM_ID_MAX
            || !self.system_id.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err("sms_hub.system_id requires 1 to 15 printable ASCII characters");
        }
        if self.password.is_empty()
            || self.password.len() > PASSWORD_MAX
            || !self.password.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err("sms_hub.password requires 1 to 8 printable ASCII characters");
        }
        if self.listen_addr.port() == 0 || self.hub_addr.port() == 0 {
            return Err("sms_hub addresses require nonzero TCP ports");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_config_needs_no_credentials_and_enabled_config_validates_wire_limits() {
        let disabled: SmsHubConfig = serde_yaml_ng::from_str("enabled: false").unwrap();
        assert!(disabled.validate().is_ok());
        let mut config: SmsHubConfig = serde_yaml_ng::from_str(
            "enabled: true\nrole: client\nsystem_id: test-site\npassword: pass1234",
        )
        .unwrap();
        assert!(config.validate().is_ok());
        assert!(!format!("{config:?}").contains("pass1234"));
        config.password.push('x');
        assert!(config.validate().is_err());
        config.password = "pass1234".into();
        config.system_id = "x".repeat(16);
        assert!(config.validate().is_err());
    }
}
