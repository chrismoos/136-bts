use super::receiver::{Block, Continuity};
use std::{error::Error as StdError, fmt};
const NANOSECONDS_PER_SECOND: i128 = 1_000_000_000;
const NANOSECONDS_PER_MILLISECOND: i128 = 1_000_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    Accept { segment: u64, reset: bool },
    Discard { age_ms: u64, reset: bool },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    Limits,
    ClockRange,
    SegmentRange,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RX age window failed: {self:?}")
    }
}
impl StdError for Error {}
pub(crate) struct ReceiveWindow {
    tx_clock_hz: i128,
    rx_clock_hz: i128,
    offset_scaled_ns: i128,
    limit_scaled_ns: i128,
    milliseconds_scale: i128,
    source_segment: Option<u64>,
    segment: u64,
    dropping: bool,
}
impl ReceiveWindow {
    pub(crate) fn try_new(
        tx_clock_hz: u64,
        rx_clock_hz: u64,
        rx_to_tx_delay_ns: i64,
        maximum_age_ms: u64,
    ) -> Result<Self, Error> {
        if tx_clock_hz == 0 || rx_clock_hz == 0 || maximum_age_ms == 0 {
            return Err(Error::Limits);
        }
        let tx_clock_hz = i128::from(tx_clock_hz);
        let rx_clock_hz = i128::from(rx_clock_hz);
        let scale = tx_clock_hz
            .checked_mul(rx_clock_hz)
            .ok_or(Error::ClockRange)?;
        let milliseconds_scale = scale
            .checked_mul(NANOSECONDS_PER_MILLISECOND)
            .ok_or(Error::ClockRange)?;
        let limit_scaled_ns = milliseconds_scale
            .checked_mul(i128::from(maximum_age_ms))
            .ok_or(Error::ClockRange)?;
        let offset_scaled_ns = scale
            .checked_mul(i128::from(rx_to_tx_delay_ns))
            .ok_or(Error::ClockRange)?;
        Ok(Self {
            tx_clock_hz,
            rx_clock_hz,
            offset_scaled_ns,
            limit_scaled_ns,
            milliseconds_scale,
            source_segment: None,
            segment: 0,
            dropping: false,
        })
    }
    pub(crate) fn admit(&mut self, block: &Block, now_tx_tick: u64) -> Result<Admission, Error> {
        let now = i128::from(now_tx_tick)
            .checked_mul(self.rx_clock_hz)
            .ok_or(Error::ClockRange)?;
        let first = i128::from(block.time_ticks)
            .checked_mul(self.tx_clock_hz)
            .ok_or(Error::ClockRange)?;
        let age = now
            .checked_sub(first)
            .and_then(|age| age.checked_mul(NANOSECONDS_PER_SECOND))
            .and_then(|age| age.checked_sub(self.offset_scaled_ns))
            .ok_or(Error::ClockRange)?;
        let changed = self
            .source_segment
            .is_some_and(|segment| segment != block.segment)
            || !matches!(block.continuity, Continuity::First | Continuity::Continuous);
        let discard = age >= self.limit_scaled_ns;
        let reset = if discard {
            !self.dropping
        } else {
            changed && !self.dropping
        };
        if reset {
            self.segment = self.segment.checked_add(1).ok_or(Error::SegmentRange)?;
        }
        self.source_segment = Some(block.segment);
        self.dropping = discard;
        if discard {
            let age_ms = u64::try_from(age / self.milliseconds_scale).unwrap_or(u64::MAX);
            Ok(Admission::Discard { age_ms, reset })
        } else {
            Ok(Admission::Accept {
                segment: self.segment,
                reset,
            })
        }
    }
}
#[cfg(test)]
mod tests;
