use super::audit::{address, hex, tail};
use crate::l3::call_control::Response;
pub(super) fn details(response: &Response) -> String {
    let mut text = format!("Reject cause: {:?}", response.cause);
    if response.kind == crate::l3::call_control::ResponseKind::RegistrationReject {
        if let Some(time) = response.options.reject_time {
            text.push_str(&format!(
                "\nRejectTime lower={} upper={} (100 SF units)",
                time.lower(),
                time.upper()
            ));
            if time.until_power_down() {
                text.push_str(" Until phone power-down");
            }
        } else {
            text.push_str(&format!(
                "\nRejectTime default={} SF",
                crate::l3::registration::DEFAULT_REJECT_SUPERFRAMES
            ));
        }
    }
    if let Some(value) = response.options.cause_extension {
        text.push_str(&format!(" ExtendedCause={value:?}"));
    }
    if let Some(value) = response.tone {
        text.push_str(&format!(" Tone={value:?}"));
    }
    if let Some(value) = response.options.signal {
        text.push_str(&format!(
            "\nPitch={:?} Cadence={} DurationCode={}",
            value.pitch, value.cadence, value.duration_code
        ));
    }
    if let Some(value) = &response.options.subaddress {
        text.push_str(&address(value));
    }
    if let Some(value) = &response.options.display {
        text.push_str(&format!(
            "\nDisplayIRA[{}]={}",
            value.len(),
            value.escape_default()
        ));
    }
    if let Some(value) = &response.options.multilingual_display {
        text.push_str(&format!(
            "\nDisplay[{}/{}]={}",
            value.encoding,
            value.bytes.len(),
            hex(&value.bytes)
        ));
    }
    if !response.options.ignored_bits.is_empty() {
        text.push_str(&format!(
            "\nIgnoredBits={}",
            response.options.ignored_bits.len()
        ));
    }
    if !response.options.unparsed_bits.is_empty() {
        text.push_str(&tail(&response.options.unparsed_bits));
    }
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::{
        audit::MultilingualDisplay,
        call_control::{ExtendedCause, ResponseOptions},
        legacy_traffic::Subaddress,
        registration::RejectCause,
        user_alert::Signal,
    };
    #[test]
    fn maximum_control_release_trace_keeps_extensions_displays_signal_and_unknown_tail() {
        let mut response = Response::release(RejectCause::Extension);
        response.options = ResponseOptions {
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
            cause_extension: Some(ExtendedCause::DataPrivacyNotAvailable),
            ..Default::default()
        };
        let length = response.encode().unwrap().len();
        response.options.unparsed_bits = vec![true; (255 - length) * 8];
        response.options.unparsed_bits[..4].copy_from_slice(&[false, true, false, false]);
        let wire = response.encode().unwrap();
        assert_eq!(wire.len(), 255);
        let (name, text) = super::super::decode::control("Forward", &wire);
        assert_eq!(name, "Release");
        assert!(text.len() <= super::super::MAX_RECORD_BYTES, "{text}");
        assert!(text.contains("ExtendedCause=DataPrivacyNotAvailable"));
        assert!(text.contains("Cadence=1 DurationCode=15"));
        assert!(text.contains("DisplayIRA[82]="));
        assert!(text.contains("Display[18/82]="));
        assert!(text.contains("UnknownBits="));
        assert_eq!(
            super::super::decode::control("Reverse", &wire).0,
            "Unknown message 0x1F"
        );
    }
}

#[cfg(test)]
mod registration_reject_tests {
    #[test]
    fn registration_reject_trace_decodes_time_displays_and_cause_extension() {
        let (name, text) =
            super::super::decode::control("Forward", &[0x1e, 0xf1, 0x24, 0x80, 0x14, 0x13, 0x01]);
        assert_eq!(name, "Registration response");
        assert!(text.contains("RejectTime lower=2 upper=4"));
        assert!(text.contains("DisplayIRA[1]=A"));
        assert!(text.contains("ExtendedCause=DataPrivacyNotAvailable"));
        let (_, text) = super::super::decode::control("Forward", &[0x1e, 0x10]);
        assert!(text.contains("default=2048 SF"));
        let (_, text) = super::super::decode::control("Forward", &[0x1e, 0x11, 0xf2]);
        assert!(text.contains("Until phone power-down"));
        let (name, _) = super::super::decode::control("Forward", &[0x1e, 0x11, 0x42]);
        assert!(name.starts_with("Unknown message"));
    }
}
