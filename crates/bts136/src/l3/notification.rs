mod confirmation;
pub use confirmation::SpachConfirmation;
use std::{error::Error as StdError, fmt};
const NOTIFICATION: u8 = 33;
pub const CONFIRMATION_TYPE: u8 = 15;
pub const SMS_TYPE: u8 = 26;
pub const MACA_TYPE: u8 = 5;
const TYPE_SHIFT: u32 = 2;
const PADDING_MASK: u8 = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidConfirmation;
impl fmt::Display for InvalidConfirmation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unsupported SPACH confirmation")
    }
}
impl StdError for InvalidConfirmation {}
// IS-136.1-A §§6.4.3.19 and 6.4.4.15. Mandatory SMS notification and confirmation fields only.
pub fn sms_notification() -> Vec<u8> {
    vec![NOTIFICATION, SMS_TYPE << TYPE_SHIFT]
}
pub fn sms_confirmation(bytes: &[u8]) -> Result<(), InvalidConfirmation> {
    match bytes {
        [CONFIRMATION_TYPE, kind]
            if *kind >> TYPE_SHIFT == SMS_TYPE && *kind & PADDING_MASK == 0 =>
        {
            Ok(())
        }
        _ => Err(InvalidConfirmation),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_notification_and_confirmation_fields_reject_other_procedures_and_options() {
        assert_eq!(sms_notification(), [0x21, 0x68]);
        assert!(sms_confirmation(&[0x0f, 0x68]).is_ok());
        for bytes in [
            vec![0x0f, 0x60],
            vec![0x0f, 0x69],
            vec![0x0f],
            vec![0x0f, 0x68, 0],
            vec![0x21, 0x68],
        ] {
            assert!(sms_confirmation(&bytes).is_err());
        }
    }
}
