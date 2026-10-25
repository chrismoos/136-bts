use super::audit::{address, hex, tail};
use crate::l3::test_registration::{NameList, Request, Response};

pub(super) fn request(message: &Request) -> String {
    let mut text = format!("SelectedMap=0x{:04X}", message.selected_systems);
    if let Some(value) = message.number_request {
        text.push_str(&format!(" DirectoryNumberRequested={value}"));
    }
    if let Some(value) = &message.subaddress {
        text.push_str(&address(value));
    }
    append_tail(&mut text, &message.ignored_bits, &message.unparsed_bits);
    text
}
pub(super) fn response(message: &Response) -> String {
    let mut text = format!("AcceptedMap=0x{:04X}", message.accepted_systems);
    if let Some(value) = &message.subaddress {
        text.push_str(&address(value));
    }
    if let Some(value) = &message.system_id {
        text.push_str(&format!("\nSystemID={}", value.escape_default()));
    }
    if let Some(value) = &message.names {
        append_names(&mut text, value);
    }
    if let Some(value) = &message.directory {
        text.push_str(&format!("\nDirectory={value:?}"));
    }
    if let Some(value) = &message.multilingual_id {
        text.push_str(&format!(
            "\nMultilingualID[{}]={}",
            value.encoding,
            hex(&value.bytes)
        ));
    }
    if let Some(value) = &message.multilingual_names {
        append_names(&mut text, value);
    }
    append_tail(&mut text, &message.ignored_bits, &message.unparsed_bits);
    text
}
fn append_names(text: &mut String, names: &NameList) {
    match names.entries() {
        Ok(entries) => {
            for (index, name) in entries
                .iter()
                .enumerate()
                .filter(|(_, name)| !name.is_empty())
            {
                let value = if names.encoding().is_none() {
                    String::from_utf8_lossy(name).escape_default().to_string()
                } else {
                    hex(name)
                };
                text.push_str(&format!(
                    "\nSystemName[{};encoding={:?}]={value}",
                    index + 1,
                    names.encoding()
                ));
            }
        }
        Err(error) => text.push_str(&format!("\nInvalidNameList={error}")),
    }
}
fn append_tail(text: &mut String, ignored: &[bool], unparsed: &[bool]) {
    if !ignored.is_empty() {
        text.push_str(&format!("\nIgnoredBits={}", ignored.len()));
    }
    if !unparsed.is_empty() {
        text.push_str(&tail(unparsed));
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn test_registration_logs_are_directional_and_describe_maps_names_and_number_request() {
        let (name, text) = super::super::decode::control("Reverse", &[0x11, 0x80, 1, 0x21]);
        assert_eq!(name, "Test Registration");
        assert!(text.contains("SelectedMap=0x8001"));
        assert!(text.contains("DirectoryNumberRequested=true"));
        let (name, text) =
            super::super::decode::control("Forward", &[0x23, 0, 1, 0x90, 0x21, 0x41, 0]);
        assert_eq!(name, "Test Registration Response");
        assert!(text.contains("AcceptedMap=0x0001"));
        assert!(text.contains("SystemName[1;encoding=None]=A"));
        assert_ne!(
            super::super::decode::control("Forward", &[0x11, 0x80, 1, 0x21]).0,
            "Test Registration"
        );
        assert_ne!(
            super::super::decode::control("Reverse", &[0x23, 0, 1]).0,
            "Test Registration Response"
        );
    }
}
