// IS-136.1-A §6.6 Tables 6-14 and 6-15. These are mobile procedure limits, not configurable network policy.
pub const SUPERFRAME_MS: u64 = 640;
pub const REGISTRATION_MS: u64 = 25 * SUPERFRAME_MS;
pub const ORIGINATION_ACCESS_MS: u64 = 25 * SUPERFRAME_MS;
pub const PAGE_ACCESS_MS: u64 = 15 * SUPERFRAME_MS;
pub const ORIGINATION_RESPONSE_MS: u64 = 20 * SUPERFRAME_MS;
pub const SMS_ORIGINATION_MS: u64 = 20 * SUPERFRAME_MS;
/// How long an origination waits for the called phone. It covers the called phone's page
/// access time and ends early enough that the caller's Reorder or channel designation can
/// still reach it before its ORIG_TMR expires.
pub const ORIGINATION_PAGE_WAIT_MS: u64 = PAGE_ACCESS_MS + SUPERFRAME_MS;
const _: () = assert!(ORIGINATION_PAGE_WAIT_MS < ORIGINATION_RESPONSE_MS);
pub const SPACH_MS: u64 = 10 * SUPERFRAME_MS;
pub const WAIT_FOR_ORDER_MS: u64 = 10 * SUPERFRAME_MS;
pub const MOBILE_RESPONSE_WINDOW_MS: u64 = SUPERFRAME_MS;
