use crate::l3::maca::broadcast::{BroadcastKind, MacaBroadcast};
pub(super) fn name(message: &MacaBroadcast) -> &'static str {
    match message.kind {
        BroadcastKind::Basic => "Mobile Assisted Channel Allocation",
        BroadcastKind::MultiHyperband => "Mobile Assisted Channel Allocation (Multi Hyperband)",
    }
}
pub(super) fn details(message: &MacaBroadcast) -> String {
    let triggers = message.triggers;
    let restriction = match message.unrestricted {
        Some(true) => "inactive",
        Some(false) => "Origination/Page Response limited to 8 other channels",
        None => "omitted",
    };
    let mut text = format!(
        "Status={:?} Registration={} Origination={} PageResponse={} Audit={}\nMACA8_CONTROL={}",
        message.status,
        triggers.registration,
        triggers.origination,
        triggers.page_response,
        triggers.audit,
        restriction
    );
    if let Some(channels) = &message.current_channels {
        text.push_str(&format!("\nCurrent-band channels={channels:?}"));
    } else if message.kind == BroadcastKind::MultiHyperband {
        text.push_str("\nCurrent-band list falls back to the base MACA message");
    }
    for list in &message.other_channels {
        text.push_str(&format!("\nOther {:?}={:?}", list.hyperband, list.channels));
    }
    if let Some(enhanced) = message.enhanced {
        text.push_str(&format!(
            "\nDPC={} ExtendedMACA={}",
            enhanced.dpc, enhanced.extended
        ));
    } else {
        text.push_str("\nDPC=false ExtendedMACA=false");
    }
    if !message.ignored_bits.is_empty() {
        text.push_str(&format!("\nIgnoredBits={}", message.ignored_bits.len()));
    }
    if !message.unparsed_bits.is_empty() {
        text.push_str(&super::audit::tail(&message.unparsed_bits));
    }
    text
}
#[cfg(test)]
mod tests {
    #[test]
    fn broadcast_logs_decode_maca_without_confusing_reverse_capability_reports() {
        let (name, text) =
            super::super::decode::broadcast(&[0x05, 0xe4, 0x64, 0x20, 0x05, 0xf4, 0x1b]);
        assert_eq!(name, "Mobile Assisted Channel Allocation");
        assert!(text.contains("Status=Both"));
        assert!(text.contains("Current-band channels=[1, 1000]"));
        assert!(text.contains("ExtendedMACA=true"));
        let (name, text) = super::super::decode::broadcast(&[0x29, 0x98, 0xd0, 0x7d, 0x08, 0x80]);
        assert!(name.contains("Multi Hyperband"));
        assert!(text.contains("falls back"));
        assert!(text.contains("Pcs1900=[1000]"));
        assert_ne!(
            super::super::decode::control("Reverse", &[5, 0]).0,
            "Mobile Assisted Channel Allocation"
        );
    }
}
