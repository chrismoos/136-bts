use crate::l3::security::{SecurityBody, SecurityMessage};
pub(super) fn name(message: &SecurityMessage) -> &'static str {
    match message.body {
        SecurityBody::Authentication { .. } => "Authentication",
        SecurityBody::BaseStationChallenge { .. } => "Base Station Challenge Order",
        SecurityBody::BaseStationConfirmation { .. } => "Base Station Challenge Order Confirmation",
        SecurityBody::SsdUpdateOrder { .. } => "SSD Update Order",
        SecurityBody::SsdUpdateConfirmation { .. } => "SSD Update Order Confirmation",
        SecurityBody::UniqueChallengeOrder { .. } => "Unique Challenge Order",
        SecurityBody::UniqueChallengeConfirmation { .. } => "Unique Challenge Order Confirmation",
    }
}
pub(super) fn details(message: &SecurityMessage) -> String {
    let mut text = match message.body {
        SecurityBody::Authentication {
            count,
            randc,
            authr,
        } => format!("COUNT={count} RANDC=0x{randc:02X} AUTHR=0x{authr:05X}"),
        SecurityBody::BaseStationChallenge { randbs } => format!("RANDBS=0x{randbs:08X}"),
        SecurityBody::BaseStationConfirmation { authbs } => format!("AUTHBS=0x{authbs:05X}"),
        SecurityBody::SsdUpdateOrder { randssd } => format!("RANDSSD=0x{randssd:014X}"),
        SecurityBody::SsdUpdateConfirmation { status } => format!("SSDUpdateStatus={status:?}"),
        SecurityBody::UniqueChallengeOrder { randu } => format!("RANDU=0x{randu:06X}"),
        SecurityBody::UniqueChallengeConfirmation { authu } => format!("AUTHU=0x{authu:05X}"),
    };
    if let Some(address) = &message.options.subaddress {
        text.push_str(&super::audit::address(address));
    }
    if !message.options.ignored_bits.is_empty() {
        text.push_str(&format!(
            "\nIgnoredBits={}",
            message.options.ignored_bits.len()
        ));
    }
    if !message.options.unparsed_bits.is_empty() {
        text.push_str(&super::audit::tail(&message.options.unparsed_bits));
    }
    text
}
#[cfg(test)]
mod tests {
    #[test]
    fn security_logs_decode_fixed_vectors_in_their_defined_direction() {
        for (direction, wire, name, field) in [
            (
                "Reverse",
                vec![2, 0x56, 0x96, 0xaa, 0xaa],
                "Authentication",
                "COUNT=21 RANDC=0xA5 AUTHR=0x2AAAA",
            ),
            (
                "Reverse",
                vec![3, 0x12, 0x34, 0x56, 0x78],
                "Base Station Challenge Order",
                "RANDBS=0x12345678",
            ),
            (
                "Forward",
                vec![19, 0xaa, 0xaa, 0x80],
                "Base Station Challenge Order Confirmation",
                "AUTHBS=0x2AAAA",
            ),
            (
                "Forward",
                vec![34, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde],
                "SSD Update Order",
                "RANDSSD=0x123456789ABCDE",
            ),
            (
                "Reverse",
                vec![16, 0x40],
                "SSD Update Order Confirmation",
                "SSDUpdateStatus=AuthbsMismatch",
            ),
            (
                "Forward",
                vec![36, 0x12, 0x34, 0x56],
                "Unique Challenge Order",
                "RANDU=0x123456",
            ),
            (
                "Reverse",
                vec![18, 0xaa, 0xaa, 0x80],
                "Unique Challenge Order Confirmation",
                "AUTHU=0x2AAAA",
            ),
        ] {
            let (actual, text) = super::super::decode::control(direction, &wire);
            assert_eq!(actual, name);
            assert!(text.contains(field), "{text}");
            let opposite = if direction == "Forward" {
                "Reverse"
            } else {
                "Forward"
            };
            assert_ne!(super::super::decode::control(opposite, &wire).0, name);
        }
    }
}
