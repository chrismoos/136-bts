use crate::l3::call_queue::{QueueBody, QueueMessage};
pub(super) fn name(message: &QueueMessage) -> &'static str {
    match message.body {
        QueueBody::Disconnect => "Queue Disconnect",
        QueueBody::DisconnectAck => "Queue Disconnect Ack",
        QueueBody::Update(_) => "Queue Update",
    }
}
pub(super) fn details(message: &QueueMessage) -> String {
    let mut text = String::new();
    if let QueueBody::Update(update) = &message.body {
        if let Some(position) = update.position {
            text.push_str(&format!("QueuePosition={position}"));
        }
        if let Some(channels) = &update.current_channels {
            text.push_str(&format!("\nMACA current hyperband={channels:?}"));
        }
        for list in &update.other_channels {
            text.push_str(&format!("\nMACA {:?}={:?}", list.hyperband, list.channels));
        }
        if let Some(display) = &update.display {
            text.push_str(&format!("\nDisplayIRA={}", display.escape_default()));
        }
        if let Some(display) = &update.multilingual_display {
            text.push_str(&format!(
                "\nDisplay[{}]={}",
                display.encoding,
                super::audit::hex(&display.bytes)
            ));
        }
    }
    if let Some(address) = &message.subaddress {
        text.push_str(&super::audit::address(address));
    }
    if !message.ignored_bits.is_empty() {
        text.push_str(&format!("\nIgnoredBits={}", message.ignored_bits.len()));
    }
    if !message.unparsed_bits.is_empty() {
        text.push_str(&super::audit::tail(&message.unparsed_bits));
    }
    if text.is_empty() {
        text.push_str("No optional fields");
    }
    text
}
#[cfg(test)]
mod tests {
    #[test]
    fn queue_logs_keep_direction_positions_measurement_targets_and_displays() {
        let wire = [
            0x2c, 0x15, 0x30, 0x7b, 0xe8, 0x83, 0xe8, 0x90, 0x32, 0, 0x3a, 0x90,
        ];
        let (name, text) = super::super::decode::control("Forward", &wire);
        assert_eq!(name, "Queue Update");
        assert!(text.contains("QueuePosition=5"));
        assert!(text.contains("current hyperband=[991]"));
        assert!(text.contains("Pcs1900=[1000]"));
        assert!(text.contains("Display[4]=03A9"));
        assert_eq!(
            super::super::decode::control("Reverse", &[19]).0,
            "Queue Disconnect"
        );
        assert_eq!(
            super::super::decode::control("Forward", &[43]).0,
            "Queue Disconnect Ack"
        );
        assert_ne!(
            super::super::decode::control("Forward", &[19]).0,
            "Queue Disconnect"
        );
        assert!(
            super::super::decode::control("Forward", &[44, 0x10])
                .0
                .starts_with("Unknown message")
        );
    }
}
