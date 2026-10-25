use crate::l3::registration::{AcceptOptions, RegistrationRequest};
pub(super) fn request(value: &RegistrationRequest) -> String {
    let options = &value.options;
    let mut text = format!(
        "{:?} SCM={} Protocol={:?}",
        value.registration_type, value.scm, value.protocol_version
    );
    if let Some(pfc) = options.pfc {
        text.push_str(&format!("\nRequested PFC={}", pfc + 1));
    }
    if let Some(sid) = options.sid_report {
        text.push_str(&format!("\nPrevious SID={sid}"));
    }
    if let Some(number) = options.number_request {
        text.push_str(&format!("\nDirectory number requested={}", number == 1));
    }
    if let Some(mode) = options.encryption {
        text.push_str(&format!(
            "\nEncryption algorithm={} domain={} key={}",
            mode.algorithm, mode.domain, mode.key
        ));
    }
    if let Some(system) = options.selected_system {
        text.push_str(&format!("\nSelected PSID/RSID={system}"));
    }
    if let Some(system) = options.system_value {
        text.push_str(&format!("\nSelected PSID/RSID value=0x{system:X}"));
    }
    if let Some(soc) = options.target_soc {
        text.push_str(&format!("\nTarget SOC={soc}"));
    }
    if let Some(group) = &options.group {
        text.push_str(&format!("\nGroup={group:?}"));
    }
    if options.c_number.is_some() {
        text.push_str("\nC-Number present (forwarding)");
    }
    if let Some(number) = &options.multilingual_number {
        text.push_str(&format!(
            "\nMultilingual C-Number: type={} plan={} encoding={} octets={}",
            number.number_type,
            number.numbering_plan,
            number.encoding,
            number.bytes.len()
        ));
    }
    if let Some(address) = &options.subaddress {
        text.push_str(&format!(
            "\nSubaddress: kind={} odd={} octets={}",
            address.kind,
            address.odd,
            address.address.len()
        ));
    }
    if !options.ignored_bits.is_empty() {
        text.push_str(&format!(
            "\nIgnored optional bits={}",
            options.ignored_bits.len()
        ));
    }
    if !options.unparsed_bits.is_empty() {
        text.push_str(&format!(
            "\nUnparsed optional bits={}",
            options.unparsed_bits.len()
        ));
    }
    text
}
pub(super) fn accept(value: &AcceptOptions) -> String {
    let mut text = "Accept".to_owned();
    if let Some(pfc) = value.pfc {
        text.push_str(&format!("\nAssigned PFC={}", pfc + 1));
    }
    if let Some(address) = &value.directory_address {
        text.push_str(&format!(
            "\nDirectory address: type={} plan={} {}",
            address.number_type,
            address.numbering_plan,
            match &address.address {
                crate::l3::address::AddressValue::Tbcd(value) => value.clone(),
                crate::l3::address::AddressValue::BinaryInternet(value) => value.to_string(),
                crate::l3::address::AddressValue::Ira(bytes)
                | crate::l3::address::AddressValue::DpcSsn(bytes) =>
                    bytes.iter().map(|byte| format!("{byte:02X}")).collect(),
            }
        ));
    }
    if !value.ignored_bits.is_empty() {
        text.push_str(&format!(
            "\nIgnored optional bits={}",
            value.ignored_bits.len()
        ));
    }
    if !value.unparsed_bits.is_empty() {
        text.push_str(&format!(
            "\nUnparsed optional bits={}",
            value.unparsed_bits.len()
        ));
    }
    text
}
pub(super) fn serial(value: &crate::l3::registration::SerialNumber) -> String {
    let mut text = format!("ESN=0x{:08X}", value.esn);
    if let Some(esnx) = value.esnx {
        text.push_str(&format!("\nESNX=0x{esnx:06X}"));
    }
    if let Some(esn) = value.s_esn {
        text.push_str(&format!("\nS-ESN=0x{esn:08X}"));
    }
    if !value.ignored_bits.is_empty() {
        text.push_str(&format!(
            "\nIgnored optional bits={}",
            value.ignored_bits.len()
        ));
    }
    if !value.unparsed_bits.is_empty() {
        text.push_str(&format!(
            "\nUnparsed optional bits={}",
            value.unparsed_bits.len()
        ));
    }
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registration_trace_includes_requests_and_assignments() {
        let request =
            RegistrationRequest::decode(&[0x0c, 0x1a, 0xa9, 0x07, 0x10, 0x05, 0x4e, 0x42]).unwrap();
        let text = super::request(&request);
        assert!(text.contains("Requested PFC=1"));
        assert!(text.contains("Previous SID=42"));
        assert!(text.contains("Directory number requested=true"));
        let value = AcceptOptions::decode(&[0x1d, 0x11, 0x20, 0x60, 0x63, 0x54, 0x20]).unwrap();
        let text = accept(&value);
        assert!(text.contains("Assigned PFC=1"));
        assert!(text.contains("1001"));
    }
}
