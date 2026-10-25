use super::audit::{address, tail};
use crate::l3::control_management::ControlManagement;

pub(super) fn details(message: &ControlManagement) -> String {
    let (mut text, subaddress, ignored, unparsed) = match message {
        ControlManagement::DirectedRetry(message) => {
            let mut text = format!("LastTry={}", message.last_try);
            if let Some(flags) = message.rcf_auth {
                text.push_str(&format!(
                    " ReadControlFiller={} Authentication={}",
                    flags.read_control_filler, flags.authentication
                ));
            }
            if let Some(dtx) = message.dtx {
                text.push_str(&format!(" AnalogDTX={dtx:?}"));
            }
            for channel in &message.channels {
                text.push_str(&format!(
                    "\nRetryChannel={:?}/{}",
                    channel.hyperband, channel.channel
                ));
            }
            (
                text,
                &message.subaddress,
                &message.ignored_bits,
                &message.unparsed_bits,
            )
        }
        ControlManagement::ParameterUpdate(message) => (
            format!("RequestNumber={}", message.request_number),
            &message.subaddress,
            &message.ignored_bits,
            &message.unparsed_bits,
        ),
    };
    if let Some(subaddress) = subaddress {
        text.push_str(&address(subaddress));
    }
    if !ignored.is_empty() {
        text.push_str(&format!("\nIgnoredBits={}", ignored.len()));
    }
    if !unparsed.is_empty() {
        text.push_str(&tail(unparsed));
    }
    text
}

#[cfg(test)]
mod tests {
    #[test]
    fn logs_name_retry_channels_analog_flags_and_parameter_request_numbers() {
        let (name, text) =
            super::super::decode::control("Forward", &[0x16, 0x8c, 0x51, 0xb5, 0x54]);
        assert_eq!(name, "Directed Retry");
        assert!(text.contains("LastTry=true"));
        assert!(text.contains("ReadControlFiller=true Authentication=false"));
        assert!(text.contains("AnalogDTX=Attenuation8Db"));
        assert!(text.contains("RetryChannel=Pcs1900/1365"));
        let (name, text) =
            super::super::decode::control("Forward", &[0x19, 0xa1, 0x02, 0x14, 0xab]);
        assert_eq!(name, "Parameter Update");
        assert!(text.contains("RequestNumber=10"));
        assert!(text.contains("AB"));
        let (name, _) = super::super::decode::control("Reverse", &[0x16, 0]);
        assert_ne!(name, "Directed Retry");
    }
}
