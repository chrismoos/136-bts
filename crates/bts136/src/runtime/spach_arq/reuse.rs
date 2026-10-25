use crate::l3::broadcast::AccessParameters;
pub(super) const TDMA_BLOCK_MS: u64 = 20;
const MAX_BUSY_LOOKUPS: u64 = 10;
const BUSY_DELAY_BLOCKS: u64 = 6;
const FIRST_RETRY_DELAY_BLOCKS: u64 = 6;
const LATER_RETRY_DELAY_BLOCKS: u64 = 20;
const LOOKUP_ALIGNMENT_BLOCKS: u64 = 1;
const ACCESS_DIALOGUE_BLOCKS: u64 = 6;
const ARQ_INACTIVITY_BLOCKS: u64 = 32;
const ARQ_FINAL_WAIT_BLOCKS: u64 = 32;
const MAX_RETRIES_CODE: u8 = 7;
const STATUS_ACCESS_PROCEDURES: u64 = 2;
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReusePolicy {
    pub quiet_ms: u64,
}
impl ReusePolicy {
    pub(crate) fn quiet_blocks(self) -> u64 {
        self.quiet_ms.div_ceil(TDMA_BLOCK_MS)
    }
    pub(crate) fn full_rate_contention(
        parameters: &AccessParameters,
    ) -> Result<Self, super::Error> {
        if parameters.max_retries_code > MAX_RETRIES_CODE {
            return Err(super::Error::Limits);
        }
        // TIA-136-123-E §6.118 maps code n to n+1 access attempts.
        let retries = u64::from(parameters.max_retries_code);
        let attempts = retries + 1;
        let lookups = if parameters.max_busy_reserved_code {
            MAX_BUSY_LOOKUPS
        } else {
            1
        };
        // TIA/EIA-136-121-A §9.2 bounds idle lookup through returned SCF to one six-block access cycle.
        let attempt_blocks =
            lookups * (BUSY_DELAY_BLOCKS + LOOKUP_ALIGNMENT_BLOCKS) + ACCESS_DIALOGUE_BLOCKS;
        // TIA/EIA-136-122-B §3.3.1 uses a six-block first retry delay when more than one retry is allowed.
        let retry_blocks = match retries {
            0 => 0,
            1 => LATER_RETRY_DELAY_BLOCKS,
            _ => FIRST_RETRY_DELAY_BLOCKS + (retries - 1) * LATER_RETRY_DELAY_BLOCKS,
        };
        // TIA/EIA-136-122-B §§4.2.1–2 allow a solicited status followed by an unsolicited status and two 32-block waits.
        let quiet_blocks = STATUS_ACCESS_PROCEDURES * (attempts * attempt_blocks + retry_blocks)
            + ARQ_INACTIVITY_BLOCKS
            + ARQ_FINAL_WAIT_BLOCKS;
        Ok(Self {
            quiet_ms: quiet_blocks * TDMA_BLOCK_MS,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_profile_and_all_finite_retry_codes_have_bounded_reuse_intervals() {
        let config = crate::config::Config::from_yaml(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/two-phones.yaml"
        )))
        .unwrap();
        let crate::l3::broadcast::MandatoryBroadcast::AccessParameters(mut parameters) =
            crate::runtime::serving::messages(&config)[1].clone()
        else {
            panic!()
        };
        assert_eq!(
            ReusePolicy::full_rate_contention(&parameters)
                .unwrap()
                .quiet_ms,
            5200
        );
        for (code, expected) in [1800, 3120, 3880, 5200, 6520, 7840, 9160, 10480]
            .into_iter()
            .enumerate()
        {
            parameters.max_retries_code = code as u8;
            for busy in [false, true] {
                parameters.max_busy_reserved_code = busy;
                let policy = ReusePolicy::full_rate_contention(&parameters).unwrap();
                let extra = if busy {
                    2 * (code as u64 + 1) * 9 * 7 * 20
                } else {
                    0
                };
                assert_eq!(policy.quiet_ms, expected + extra);
                let sender = super::super::Sender::<()>::after_restart(1, policy).unwrap();
                assert!(sender.echo_guard_ms >= policy.quiet_ms);
            }
        }
        parameters.max_retries_code = MAX_RETRIES_CODE + 1;
        assert!(matches!(
            ReusePolicy::full_rate_contention(&parameters),
            Err(super::super::Error::Limits)
        ));
    }
}
