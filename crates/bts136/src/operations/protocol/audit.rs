use crate::l3::audit::{AuditConfirmation, AuditOrder};
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}
pub(super) fn tail(bits: &[bool]) -> String {
    let hex: String = bits
        .chunks(4)
        .map(|part| {
            let value = part
                .iter()
                .fold(0u8, |value, bit| value << 1 | u8::from(*bit))
                << (4 - part.len());
            format!("{value:X}")
        })
        .collect();
    format!("\nUnknownBits={}:{}", bits.len(), hex)
}
pub(super) fn address(address: &crate::l3::legacy_traffic::Subaddress) -> String {
    format!(
        "\nSubaddress={}/odd{}:{}",
        address.kind,
        u8::from(address.odd),
        hex(&address.address)
    )
}
pub(super) fn order(value: &AuditOrder) -> String {
    let mut text = format!(
        "ForceRegistration={} DebugDisplayAllowed={}",
        value.force_registration, value.debug_display_allowed
    );
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
pub(super) fn confirmation(value: &AuditConfirmation) -> String {
    let mut text = format!("PFC={}", value.pfc_minus_one + 1);
    if let Some(system) = value.selected_system {
        text.push_str(&format!(" SelectedSystem={system}"));
    }
    if let Some(group) = &value.group {
        text.push_str(&format!(" Group={}:0x{:X}", group.kind, group.value));
    }
    if let Some(value) = &value.subaddress {
        text.push_str(&address(value));
    }
    if let Some(display) = &value.display {
        text.push_str(&format!(
            "\nDisplayIRA[{}]={}",
            display.len(),
            display.escape_default()
        ));
    }
    if let Some(display) = &value.multilingual_display {
        text.push_str(&format!(
            "\nDisplay[{}/{}]={}",
            display.encoding,
            display.bytes.len(),
            hex(&display.bytes)
        ));
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
        audit::{MultilingualDisplay, UserGroupId},
        legacy_traffic::Subaddress,
    };
    #[test]
    fn maximum_audit_trace_preserves_the_final_field_within_record_limit() {
        let message = AuditConfirmation {
            pfc_minus_one: 7,
            selected_system: Some(15),
            group: Some(UserGroupId {
                kind: 3,
                value: (1 << 50) - 1,
            }),
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
            ignored_bits: Vec::new(),
            unparsed_bits: vec![true; 255 * 8 - 1601],
        };
        let bytes = message.encode().unwrap();
        assert_eq!(bytes.len(), 255);
        let text = confirmation(&AuditConfirmation::decode(&bytes).unwrap());
        assert!(
            text.len() <= super::super::MAX_RECORD_BYTES,
            "{}",
            text.len()
        );
        assert!(text.ends_with("FFE"));
    }
}
