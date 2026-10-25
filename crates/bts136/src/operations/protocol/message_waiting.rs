use super::audit::{address, hex, tail};
use crate::l3::{message_waiting::MessageWaiting, notification::SpachConfirmation};
pub(super) fn details(message: &MessageWaiting) -> String {
    let mut text = format!(
        "Waiting={}",
        message
            .entries
            .iter()
            .map(|entry| format!("{}:{}", entry.kind, entry.count))
            .collect::<Vec<_>>()
            .join(",")
    );
    if let Some(value) = &message.subaddress {
        text.push_str(&address(value));
    }
    if let Some(value) = &message.display {
        text.push_str(&format!(
            "\nDisplayIRA[{}]={}",
            value.len(),
            value.escape_default()
        ));
    }
    if let Some(value) = &message.multilingual_display {
        text.push_str(&format!(
            "\nDisplay[{}/{}]={}",
            value.encoding,
            value.bytes.len(),
            hex(&value.bytes)
        ));
    }
    if !message.ignored_bits.is_empty() {
        text.push_str(&format!(
            "\nIgnored optional bits={}",
            message.ignored_bits.len()
        ));
    }
    if !message.unparsed_bits.is_empty() {
        text.push_str(&tail(&message.unparsed_bits));
    }
    text
}
pub(super) fn confirmation(value: &SpachConfirmation) -> String {
    let mut text = format!("ConfirmedType={}", value.confirmed_type);
    if let Some(value) = &value.subaddress {
        text.push_str(&address(value));
    }
    if !value.ignored_bits.is_empty() {
        text.push_str(&format!(
            "\nIgnored optional bits={}",
            value.ignored_bits.len()
        ));
    }
    if !value.unparsed_bits.is_empty() {
        text.push_str(&tail(&value.unparsed_bits));
    }
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::{
        audit::MultilingualDisplay, legacy_traffic::Subaddress, message_waiting::Waiting,
    };
    #[test]
    fn full_message_waiting_trace_keeps_maximum_displays_and_unknown_suffix_within_record_budget() {
        let mut message = MessageWaiting {
            entries: (0..=7)
                .filter(|kind| *kind != 3)
                .map(|kind| Waiting { kind, count: 63 })
                .collect(),
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
        let length = message.encode().unwrap().len();
        message.unparsed_bits = vec![true; (255 - length) * 8];
        let wire = message.encode().unwrap();
        assert_eq!(wire.len(), 255);
        let (name, text) = super::super::decode::control("Forward", &wire);
        assert_eq!(name, "Message Waiting");
        assert!(
            text.len() <= super::super::MAX_RECORD_BYTES,
            "{}",
            text.len()
        );
        assert!(text.contains("Waiting=0:63,1:63,2:63,4:63,5:63,6:63,7:63"));
        assert!(text.contains("Display[18/82]="));
        assert!(
            text.ends_with(&format!("UnknownBits=434:{}0", "F".repeat(108))),
            "{text}"
        );
    }
}
