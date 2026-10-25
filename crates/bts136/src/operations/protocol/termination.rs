use super::audit::tail;
use crate::l3::legacy_traffic::{
    AcknowledgmentInformation, ControlChannel, ReleaseFields, ReleaseInformation,
};
fn channels(values: &[ControlChannel]) -> String {
    let mut text = String::new();
    for (index, value) in values.iter().enumerate() {
        text.push_str(&format!(
            "\nControlChannel[{index}] Hyperband={:?} Channel={} DVCC={}",
            value.hyperband, value.channel, value.dvcc
        ));
    }
    text
}
pub(super) fn release(information: &ReleaseInformation) -> String {
    let mut text = match &information.fields {
        ReleaseFields::None => "Release information absent".into(),
        ReleaseFields::Reason(reason) => format!("Mobile release reason={reason:?}"),
        ReleaseFields::ControlChannels(values) => format!("Return to control{}", channels(values)),
    };
    if !information.unparsed_bits.is_empty() {
        text.push_str(&tail(&information.unparsed_bits));
    }
    text
}
pub(super) fn acknowledgment(
    ack_message_type: u8,
    information: &AcknowledgmentInformation,
) -> String {
    let mut text = format!("AcknowledgedType=0x{ack_message_type:02X}");
    if let Some(parameter) = information.last_decoded_parameter {
        text.push_str(&format!(" LastDecodedParameter=0x{parameter:X}"));
    }
    text.push_str(&channels(&information.control_channels));
    if !information.unparsed_bits.is_empty() {
        text.push_str(&tail(&information.unparsed_bits));
    }
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::legacy_traffic::{Hyperband, ReleaseReason, TrafficMessage};
    const RELEASE_HEADER_OCTETS: usize = 2;
    const MAX_RELEASE_OCTETS: usize =
        crate::l3::legacy_traffic::MAX_OPTIONAL_OCTETS + RELEASE_HEADER_OCTETS;
    #[test]
    fn termination_logs_keep_reasons_targets_and_maximum_unknown_suffixes() {
        let reason = ReleaseInformation {
            fields: ReleaseFields::Reason(ReleaseReason::PowerDown),
            ..Default::default()
        };
        assert_eq!(release(&reason), "Mobile release reason=PowerDown");
        let targets = vec![
            ControlChannel {
                hyperband: Hyperband::Imt2000,
                channel: 2047,
                dvcc: 255
            };
            3
        ];
        let mut information = ReleaseInformation {
            fields: ReleaseFields::ControlChannels(targets.clone()),
            ..Default::default()
        };
        let initial = TrafficMessage::Release {
            information: information.clone(),
        }
        .encode()
        .unwrap()
        .len()
            / 8;
        information.unparsed_bits = vec![true; (MAX_RELEASE_OCTETS - initial) * 8];
        information.unparsed_bits[..4].copy_from_slice(&[false, true, false, false]);
        let message = TrafficMessage::Release { information };
        assert_eq!(message.encode().unwrap().len(), MAX_RELEASE_OCTETS * 8);
        let text = super::super::decode::traffic_details("Forward", &message);
        assert!(text.len() <= super::super::MAX_RECORD_BYTES);
        assert!(text.contains("ControlChannel[2] Hyperband=Imt2000 Channel=2047 DVCC=255"));
        assert!(text.contains("UnknownBits="));
        let information = AcknowledgmentInformation {
            last_decoded_parameter: Some(15),
            control_channels: targets,
            ..Default::default()
        };
        let text = acknowledgment(ReleaseInformation::MESSAGE_TYPE, &information);
        assert!(text.contains("AcknowledgedType=0xDA LastDecodedParameter=0xF"));
        assert!(text.contains("ControlChannel[2]"));
    }
}
