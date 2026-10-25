use super::audit::{address, hex, tail};
use crate::l3::user_alert::{CONTINUOUS_DURATION, UserAlert};
pub(super) fn details(alert: &UserAlert) -> String {
    let signal = alert.signal.unwrap_or_default();
    let cycles = if signal.duration_code == CONTINUOUS_DURATION {
        "continuous".into()
    } else {
        (signal.duration_code + 1).to_string()
    };
    let mut text = format!(
        "Pitch={:?} Cadence={} Cycles={} SignalPresent={}",
        signal.pitch,
        signal.cadence,
        cycles,
        alert.signal.is_some()
    );
    if let Some(value) = &alert.subaddress {
        text.push_str(&address(value));
    }
    if let Some(value) = &alert.display {
        text.push_str(&format!(
            "\nDisplayIRA[{}]={}",
            value.len(),
            value.escape_default()
        ));
    }
    if let Some(value) = &alert.multilingual_display {
        text.push_str(&format!(
            "\nDisplay[{}/{}]={}",
            value.encoding,
            value.bytes.len(),
            hex(&value.bytes)
        ));
    }
    if !alert.ignored_bits.is_empty() {
        text.push_str(&format!(
            "\nIgnored optional bits={}",
            alert.ignored_bits.len()
        ));
    }
    if !alert.unparsed_bits.is_empty() {
        text.push_str(&tail(&alert.unparsed_bits));
    }
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::{audit::MultilingualDisplay, legacy_traffic::Subaddress, user_alert::Signal};
    #[test]
    fn maximum_alert_trace_retains_duration_displays_and_unknown_suffix() {
        let mut alert = UserAlert {
            signal: Some(Signal::default()),
            subaddress: Some(Subaddress {
                kind: 2,
                odd: true,
                address: vec![255; 20],
            }),
            display: Some("\x1b".repeat(82)),
            multilingual_display: Some(MultilingualDisplay {
                encoding: 18,
                bytes: vec![255; 82],
            }),
            ..Default::default()
        };
        let length = alert.encode().unwrap().len();
        alert.unparsed_bits = vec![true; (255 - length) * 8];
        let wire = alert.encode().unwrap();
        assert_eq!(wire.len(), 255);
        let (name, text) = super::super::decode::control("Forward", &wire);
        assert_eq!(name, "User Alert");
        assert!(
            text.len() <= super::super::MAX_RECORD_BYTES,
            "{}",
            text.len()
        );
        assert!(text.contains("Cycles=continuous"));
        assert!(text.contains("Display[18/82]="));
        assert!(
            text.ends_with(&format!("UnknownBits=492:{}0", "F".repeat(122))),
            "{text}"
        );
    }
}
