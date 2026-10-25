use super::audit::{address, hex, tail};
use crate::l3::page::Page;
pub(super) fn details(page: &Page) -> String {
    let mut text = format!("Service={:?}", page.service);
    if let Some(signal) = page.signal {
        text.push_str(&format!(
            "\nPitch={:?} Cadence={} DurationCode={}",
            signal.pitch, signal.cadence, signal.duration_code
        ));
    }
    if let Some(value) = page.presentation {
        text.push_str(&format!(
            "\nPresentation={} Screening={}",
            value.presentation, value.screening
        ));
    }
    for (name, value) in [
        ("CallingNumber", &page.calling_number),
        ("CalledParty", &page.called_party),
    ] {
        if let Some(value) = value {
            text.push_str(&format!("\n{name}={}", super::access_call::address(value)));
        }
    }
    for (name, value) in [
        ("CalledSubaddress", &page.called_subaddress),
        ("CallingSubaddress", &page.calling_subaddress),
    ] {
        if let Some(value) = value {
            text.push_str(&format!("\n{name}{}", address(value)));
        }
    }
    if let Some(value) = &page.display {
        text.push_str(&format!(
            "\nDisplayIRA[{}]={}",
            value.len(),
            value.escape_default()
        ));
    }
    if let Some(value) = &page.multilingual_display {
        text.push_str(&format!(
            "\nDisplay[{}/{}]={}",
            value.encoding,
            value.bytes.len(),
            hex(&value.bytes)
        ));
    }
    if !page.ignored_bits.is_empty() {
        text.push_str(&format!("\nIgnoredBits={}", page.ignored_bits.len()));
    }
    if !page.unparsed_bits.is_empty() {
        text.push_str(&tail(&page.unparsed_bits));
    }
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::{
        address::{AddressInfo, AddressValue},
        user_alert::Signal,
    };
    #[test]
    fn page_logs_retain_maximum_wire_payload_and_typed_optional_fields() {
        let mut page = Page {
            signal: Some(Signal::default()),
            display: Some("\x1b".repeat(82)),
            calling_number: Some(AddressInfo {
                number_type: 0,
                numbering_plan: 1,
                address: AddressValue::Tbcd("123456".into()),
            }),
            ..Default::default()
        };
        let length = page.encode().unwrap().len();
        page.unparsed_bits = vec![true; (255 - length) * 8];
        page.unparsed_bits[..4].copy_from_slice(&[false, true, false, false]);
        let wire = page.encode().unwrap();
        assert_eq!(wire.len(), 255);
        let (name, text) = super::super::decode::control("Forward", &wire);
        assert_eq!(name, "Page");
        assert!(text.len() <= super::super::MAX_RECORD_BYTES, "{text}");
        assert!(text.contains("CallingNumber=0/1:123456"));
        assert!(text.contains("Cadence=1 DurationCode=15"));
        assert!(text.contains("DisplayIRA[82]="));
        assert!(text.contains("UnknownBits="));
        assert!(text.ends_with('0'));
        assert_eq!(
            super::super::decode::control("Reverse", &wire).0,
            "Unknown message 0x18"
        );
    }
}
