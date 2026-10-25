use serde::Deserialize;
use std::net::{IpAddr, SocketAddr};
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SipConfig {
    pub enabled: bool,
    pub generate_ringback: bool,
    pub listen_addr: SocketAddr,
    pub transport: String,
    pub request_uri_template: String,
    pub from_domain: String,
    pub caller_id_override: Option<String>,
    pub rtp_listen_addr: IpAddr,
    pub rtp_advertise_addr: Option<IpAddr>,
    pub rtp_port_range: [u16; 2],
    pub auth: Option<Auth>,
    pub registration: Option<Registration>,
    pub nat: Nat,
    pub logging: Logging,
    pub jitter_buffer_ms: u64,
    pub max_call_seconds: u32,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    pub username: String,
    pub password: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub registrar_uri: String,
    pub from_uri: String,
    pub contact_user: String,
    #[serde(default = "registration_expiry")]
    pub expires_secs: u32,
}
fn registration_expiry() -> u32 {
    300
}
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Nat {
    pub mode: String,
    pub stun_server: Option<String>,
    pub stun_timeout_ms: u64,
    pub rtp_latch_packets: u8,
    pub rtp_latch_interval_ms: u64,
}
impl Default for Nat {
    fn default() -> Self {
        Self {
            mode: "disabled".into(),
            stun_server: None,
            stun_timeout_ms: 1000,
            rtp_latch_packets: 5,
            rtp_latch_interval_ms: 20,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    pub sip_trace: bool,
    pub sip_sdp: bool,
}
impl std::fmt::Debug for SipConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SipConfig")
            .field("enabled", &self.enabled)
            .field("listen_addr", &self.listen_addr)
            .finish_non_exhaustive()
    }
}
impl Default for SipConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            generate_ringback: true,
            listen_addr: "127.0.0.1:5060".parse().expect("constant socket"),
            transport: "udp".into(),
            request_uri_template: "sip:{called}@127.0.0.1:5061".into(),
            from_domain: "136-bts.local".into(),
            caller_id_override: None,
            rtp_listen_addr: "127.0.0.1".parse().expect("constant address"),
            rtp_advertise_addr: None,
            rtp_port_range: [16000, 16031],
            auth: None,
            registration: None,
            nat: Nat::default(),
            logging: Logging::default(),
            jitter_buffer_ms: 60,
            max_call_seconds: 300,
        }
    }
}
impl SipConfig {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !self.enabled {
            return Ok(());
        }
        if !cfg!(feature = "sip-backend") {
            return Err("sip.enabled requires the sip-backend build feature");
        }
        if self.listen_addr.ip().is_unspecified() || self.listen_addr.port() == 0 {
            return Err("sip.listen_addr requires a concrete address and nonzero port");
        }
        if !matches!(self.transport.as_str(), "udp" | "tcp" | "tls") {
            return Err("sip.transport must be udp, tcp, or tls");
        }
        if !self.request_uri_template.starts_with("sip:")
            || !self.request_uri_template.contains("{called}")
            || self.request_uri_template.contains(['\r', '\n', '\0'])
        {
            return Err("sip.request_uri_template requires a SIP URI containing {called}");
        }
        if self.from_domain.is_empty() || self.from_domain.contains(['\r', '\n', '\0', '@', ' ']) {
            return Err("sip.from_domain is invalid");
        }
        if self.caller_id_override.as_ref().is_some_and(|n| !number(n)) {
            return Err("sip.caller_id_override requires a phone number");
        }
        if self.rtp_listen_addr.is_unspecified()
            || self.rtp_advertise_addr.is_some_and(|a| {
                a.is_unspecified()
                    || a.is_multicast()
                    || a.is_ipv4() != self.rtp_listen_addr.is_ipv4()
            })
            || self.rtp_port_range[0] == 0
            || self.rtp_port_range[0] > self.rtp_port_range[1]
        {
            return Err("sip RTP address or port range is invalid");
        }
        if self.jitter_buffer_ms >= 180 || self.max_call_seconds == 0 {
            return Err(
                "sip jitter buffer must be below 180 ms and max_call_seconds must be positive",
            );
        }
        #[cfg(feature = "sip-backend")]
        super::native::gateway_config(self).map_err(|_| "sip gateway configuration is invalid")?;
        Ok(())
    }
}
pub(crate) fn number(s: &str) -> bool {
    let digits = s.strip_prefix('+').unwrap_or(s);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_sip_needs_no_peer_or_native_support() {
        let config: SipConfig = serde_yaml_ng::from_str("enabled: false").unwrap();
        assert!(config.validate().is_ok());
    }
    #[test]
    fn debug_output_excludes_credentials() {
        let config = SipConfig {
            auth: Some(Auth {
                username: "operator".into(),
                password: "secret-value".into(),
            }),
            ..Default::default()
        };
        assert!(!format!("{config:?}").contains("secret-value"));
    }
    #[cfg(feature = "sip-backend")]
    #[test]
    fn gateway_receives_auth_registration_and_nat_settings() {
        let config: SipConfig = serde_yaml_ng::from_str(
            r#"
enabled: true
auth:
  username: operator
  password: placeholder
registration:
  registrar_uri: sip:127.0.0.1:5061
  from_uri: sip:operator@136-bts.local
  contact_user: operator
  expires_secs: 600
nat:
  mode: disabled
"#,
        )
        .unwrap();
        config.validate().unwrap();
        let gateway = super::super::native::gateway_config(&config).unwrap();
        assert_eq!(gateway.sip.auth.username, "operator");
        assert_eq!(gateway.sip.auth.password.as_deref(), Some("placeholder"));
        assert!(gateway.sip.registration.enabled);
        assert_eq!(gateway.sip.registration.expires_secs, 600);
        assert!(!gateway.sip.registration.allow_unauthenticated);
        let mut invalid = config;
        invalid.nat.mode = "invalid".into();
        assert!(invalid.validate().is_err());
    }
}

#[cfg(test)]
#[test]
fn operator_sip_fragment_parses_with_disabled_defaults() {
    let fragment: std::collections::BTreeMap<String, SipConfig> =
        serde_yaml_ng::from_str(include_str!("../../../../config/sip.example.yaml")).unwrap();
    let config = &fragment["sip"];
    assert!(!config.enabled);
    assert!(config.validate().is_ok());
}

#[cfg(all(test, feature = "sip-backend"))]
#[test]
fn operator_sip_fragment_supports_authenticated_registration_and_stun() {
    let mut fragment: std::collections::BTreeMap<String, SipConfig> =
        serde_yaml_ng::from_str(include_str!("../../../../config/sip.example.yaml")).unwrap();
    let mut config = fragment.remove("sip").unwrap();
    config.enabled = true;
    config.validate().unwrap();
    assert!(config.auth.is_some());
    assert!(config.registration.is_some());
    let gateway = super::native::gateway_config(&config).unwrap();
    assert!(gateway.sip.registration.enabled);
    assert!(!gateway.sip.registration.allow_unauthenticated);
    assert_eq!(gateway.nat.mode, "stun_latch");
    assert!(gateway.nat.stun_server().unwrap().is_some());

    config.auth = None;
    config.registration = None;
    config.nat = Nat::default();
    config.validate().unwrap();
    let gateway = super::native::gateway_config(&config).unwrap();
    assert!(!gateway.sip.registration.enabled);
    assert!(gateway.sip.auth.password.is_none());
    assert!(gateway.nat.stun_server().unwrap().is_none());
}
