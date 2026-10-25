use crate::l3::broadcast::AccessParameters;

const BLOCK_MS: u64 = 20;
const ACCESS_CYCLE_BLOCKS: u64 = 6;
const MAX_RETRIES: u8 = 7;
const MAX_REPETITIONS: u8 = 3;
const MAX_BUSY_LOOKUPS: u64 = 10;
const BUSY_DELAY_BLOCKS: u64 = 6;
const LOOKUP_ALIGNMENT_BLOCKS: u64 = 1;
const INITIAL_DELAY_BLOCKS: u64 = 31;
// TIA/EIA-136-122-B §3.3.1.2 bounds a reserved Layer 3 grant wait before contention fallback.
const RESERVED_WAIT_BLOCKS: u64 = 32;
const FIRST_RETRY_BLOCKS: u64 = 6;
const LATER_RETRY_BLOCKS: u64 = 20;
// TIA-136-123-E Table 15 and TIA/EIA-136-122-B §2.1.4 bound R-DATA plus Serial Number to two 255-octet messages.
const NORMAL_MESSAGE_FRAMES: u64 = 43;
const ABBREVIATED_MESSAGE_FRAMES: u64 = 56;
const LAYER3_TRANSMISSIONS: u64 = 2;
const TRAFFIC_INTERVAL_MS: u64 = 20;
// TIA-136-133-E §4.7.3 permits filler octets before the first message octet.
const TRAFFIC_MESSAGE_WORDS: u64 = crate::l3::legacy_traffic::MAX_MESSAGE_BITS
    .div_ceil(crate::link::traffic::WORD_BITS) as u64
    + 1;
const TRAFFIC_MESSAGE_MS: u64 = (TRAFFIC_MESSAGE_WORDS * crate::phy::sacch::WORD_SLOTS as u64
    + crate::phy::traffic::SACCH_BITS as u64
    + 1)
    * TRAFFIC_INTERVAL_MS;
// TIA-136-133-E §4.4.4 and Table 24 permit two R-DATA transmissions, including the slower SACCH path.
const TRAFFIC_RETRY_MS: u64 = LAYER3_TRANSMISSIONS
    * (TRAFFIC_MESSAGE_MS + super::super::timers::SMS_ORIGINATION_MS)
    + super::super::timers::SUPERFRAME_MS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Error {
    Limits,
}
#[derive(Debug, Clone, Copy)]
pub(super) struct AccessLifetime {
    pub maximum_access_ms: u64,
    pub retry_guard_ms: u64,
}
impl AccessLifetime {
    fn from_limits(retries: u8, lookups: u64, repetitions: u8, frames: u64) -> Self {
        let retries = u64::from(retries);
        // TIA/EIA-136-122-B §§3.3.1.2–4 permit first-burst and later-burst repetitions on reserved access.
        let occurrences = frames * (u64::from(repetitions) + 1);
        let attempt_blocks = occurrences * ACCESS_CYCLE_BLOCKS
            + lookups * (BUSY_DELAY_BLOCKS + LOOKUP_ALIGNMENT_BLOCKS)
            + INITIAL_DELAY_BLOCKS
            + RESERVED_WAIT_BLOCKS;
        let retry_blocks = match retries {
            0 => 0,
            1 => LATER_RETRY_BLOCKS,
            _ => FIRST_RETRY_BLOCKS + (retries - 1) * LATER_RETRY_BLOCKS,
        };
        let maximum_access_ms = ((retries + 1) * attempt_blocks + retry_blocks) * BLOCK_MS;
        // TIA-136-123-E §3.9 starts PPT_ORIG after access success and permits one Layer 3 resend.
        let retry_guard_ms = LAYER3_TRANSMISSIONS
            * (maximum_access_ms + super::super::timers::SMS_ORIGINATION_MS)
            + super::super::timers::SUPERFRAME_MS;
        Self {
            maximum_access_ms,
            retry_guard_ms,
        }
    }
    pub fn maximum() -> Self {
        let mut lifetime = Self::from_limits(
            MAX_RETRIES,
            MAX_BUSY_LOOKUPS,
            MAX_REPETITIONS,
            ABBREVIATED_MESSAGE_FRAMES,
        );
        lifetime.retry_guard_ms =
            lifetime.retry_guard_ms.max(TRAFFIC_RETRY_MS) + super::super::timers::SUPERFRAME_MS;
        lifetime
    }
    pub fn from_parameters(
        parameters: &AccessParameters,
        maximum_rx_age_ms: u64,
    ) -> Result<Self, Error> {
        if parameters.max_retries_code > MAX_RETRIES || parameters.max_repetitions > MAX_REPETITIONS
        {
            return Err(Error::Limits);
        }
        let mut lifetime = Self::from_limits(
            parameters.max_retries_code,
            if parameters.max_busy_reserved_code {
                MAX_BUSY_LOOKUPS
            } else {
                1
            },
            parameters.max_repetitions,
            if parameters.abbreviated_burst {
                ABBREVIATED_MESSAGE_FRAMES
            } else {
                NORMAL_MESSAGE_FRAMES
            },
        );
        lifetime.retry_guard_ms = lifetime
            .retry_guard_ms
            .max(TRAFFIC_RETRY_MS)
            .checked_add(maximum_rx_age_ms)
            .ok_or(Error::Limits)?;
        Ok(lifetime)
    }
}

#[cfg(test)]
mod tests;
