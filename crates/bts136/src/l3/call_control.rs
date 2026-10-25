mod response;
use super::registration::RejectCause;
pub use response::{ExtendedCause, Response, ResponseKind, ResponseOptions, Tone};
const PAGE: u8 = 24;
const REORDER: u8 = 32;
const RELEASE: u8 = 31;
const TYPE_BITS: usize = 6;
const IS136_PROTOCOL: u8 = 0;
const DIGITAL_SPEECH: u8 = 1;
const FIELD_SHIFT: u32 = 4;
// IS-136.1-A §§6.4.3.9 and 6.4.3.17. Absent optional fields and reorder tone zero.
pub fn digital_page() -> Vec<u8> {
    vec![PAGE, DIGITAL_SPEECH << FIELD_SHIFT]
}
pub fn reorder(cause: RejectCause) -> Vec<u8> {
    vec![REORDER, (cause as u8) << FIELD_SHIFT]
}
// TIA-136-123-E §§3.6,5.3.16 uses Release, rather than Reorder, while the paged phone waits for an order.
pub fn release(cause: RejectCause) -> Vec<u8> {
    vec![RELEASE, (cause as u8) << FIELD_SHIFT]
}
pub fn decode_response(bytes: &[u8]) -> Result<Response, super::control_fields::Error> {
    Response::decode(bytes)
}
#[cfg(test)]
mod tests;
