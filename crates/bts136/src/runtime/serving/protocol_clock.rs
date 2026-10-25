use std::{error::Error, fmt};

const MILLISECONDS_PER_SECOND: u128 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClockError {
    ZeroRate,
    BeforeOrigin,
    Range,
}
impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "protocol radio clock failed: {self:?}")
    }
}
impl Error for ClockError {}

pub(super) struct ProtocolClock {
    origin_tick: u64,
    tick_rate_hz: u64,
}
impl ProtocolClock {
    pub(super) fn try_new(origin_tick: u64, tick_rate_hz: u64) -> Result<Self, ClockError> {
        if tick_rate_hz == 0 {
            return Err(ClockError::ZeroRate);
        }
        Ok(Self {
            origin_tick,
            tick_rate_hz,
        })
    }
    pub(super) fn milliseconds(&self, hardware_tick: u64) -> Result<u64, ClockError> {
        let elapsed_ticks = hardware_tick
            .checked_sub(self.origin_tick)
            .ok_or(ClockError::BeforeOrigin)?;
        u64::try_from(
            u128::from(elapsed_ticks) * MILLISECONDS_PER_SECOND / u128::from(self.tick_rate_hz),
        )
        .map_err(|_| ClockError::Range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK_RATE_HZ: u64 = 24_883_200;
    const ORIGIN_TICK: u64 = 8_000_000;

    #[test]
    fn scheduled_and_observed_events_share_one_epoch_without_callback_delay() {
        let clock = ProtocolClock::try_new(ORIGIN_TICK, TICK_RATE_HZ).unwrap();
        let completion_tick = ORIGIN_TICK + TICK_RATE_HZ / 5;
        let observation_tick = ORIGIN_TICK + TICK_RATE_HZ * 2;
        assert_eq!(clock.milliseconds(ORIGIN_TICK), Ok(0));
        assert_eq!(clock.milliseconds(completion_tick), Ok(200));
        assert_eq!(clock.milliseconds(observation_tick), Ok(2000));
        assert_eq!(clock.milliseconds(completion_tick), Ok(200));
    }

    #[test]
    fn fractional_ticks_round_down_without_intermediate_overflow() {
        let clock = ProtocolClock::try_new(0, TICK_RATE_HZ).unwrap();
        assert_eq!(clock.milliseconds(TICK_RATE_HZ - 1), Ok(999));
        assert_eq!(clock.milliseconds(TICK_RATE_HZ), Ok(1000));
        assert_eq!(
            clock.milliseconds(u64::MAX),
            Ok((u128::from(u64::MAX) * MILLISECONDS_PER_SECOND / u128::from(TICK_RATE_HZ)) as u64)
        );
    }

    #[test]
    fn invalid_rates_pre_origin_ticks_and_unrepresentable_times_are_rejected() {
        assert!(matches!(
            ProtocolClock::try_new(0, 0),
            Err(ClockError::ZeroRate)
        ));
        let clock = ProtocolClock::try_new(ORIGIN_TICK, TICK_RATE_HZ).unwrap();
        assert_eq!(
            clock.milliseconds(ORIGIN_TICK - 1),
            Err(ClockError::BeforeOrigin)
        );
        let clock = ProtocolClock::try_new(0, 1).unwrap();
        assert_eq!(clock.milliseconds(u64::MAX), Err(ClockError::Range));
    }
}
