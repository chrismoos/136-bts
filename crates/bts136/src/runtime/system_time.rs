use crate::l3::time_date::{TimeAndDate, TimeZoneOffset, UNIX_TO_1980_SECONDS};
use jiff::{Timestamp, tz::TimeZone};
use std::{error::Error, fmt, time::SystemTime};
const SECONDS_PER_MINUTE: i32 = 60;
const MILLISECONDS_PER_SECOND: i64 = 1000;
#[derive(Debug)]
pub(crate) enum TimeError {
    Zone(jiff::Error),
    Range,
    Offset(crate::l3::broadcast::WireError),
}
impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zone(error) => write!(f, "system timezone: {error}"),
            Self::Range => {
                f.write_str("system time or timezone is outside the IS-136 time representation")
            }
            Self::Offset(error) => error.fmt(f),
        }
    }
}
impl Error for TimeError {}
pub(crate) fn sample(delay_ms: i64) -> Result<TimeAndDate, TimeError> {
    let zone = TimeZone::try_system().map_err(TimeError::Zone)?;
    let unix_ms = i64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| TimeError::Range)?
            .as_millis(),
    )
    .map_err(|_| TimeError::Range)?;
    at(
        &zone,
        unix_ms.checked_add(delay_ms).ok_or(TimeError::Range)?,
    )
}
fn at(zone: &TimeZone, unix_ms: i64) -> Result<TimeAndDate, TimeError> {
    let seconds = unix_ms.div_euclid(MILLISECONDS_PER_SECOND);
    let timestamp = Timestamp::from_second(seconds).map_err(TimeError::Zone)?;
    let info = zone.to_offset_info(timestamp);
    let offset = info.offset().seconds();
    if offset % SECONDS_PER_MINUTE != 0 {
        return Err(TimeError::Range);
    }
    Ok(TimeAndDate {
        seconds_since_1980: u32::try_from(
            seconds
                .checked_sub(UNIX_TO_1980_SECONDS)
                .ok_or(TimeError::Range)?,
        )
        .map_err(|_| TimeError::Range)?,
        time_zone: TimeZoneOffset::try_new(
            i16::try_from(offset / SECONDS_PER_MINUTE).map_err(|_| TimeError::Range)?,
            info.dst().is_dst(),
        )
        .map_err(TimeError::Offset)?,
    })
}
pub(crate) fn for_superframe(
    epoch_ms: u64,
    sf: u64,
    time_slot_phase: u8,
    now_ms: u64,
) -> Result<TimeAndDate, TimeError> {
    let target_ms = sf
        .checked_mul(super::timers::SUPERFRAME_MS)
        .and_then(|value| value.checked_add(epoch_ms))
        .and_then(|value| {
            value.checked_add(
                u64::from(time_slot_phase)
                    * (super::timers::SUPERFRAME_MS / crate::scheduler::broadcast::CONTROL_SLOTS),
            )
        })
        .ok_or(TimeError::Range)?;
    let delay =
        i64::try_from(i128::from(target_ms) - i128::from(now_ms)).map_err(|_| TimeError::Range)?;
    sample(delay)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn time(zone: &str, date: &str) -> TimeAndDate {
        at(
            &TimeZone::get(zone).unwrap(),
            date.parse::<Timestamp>().unwrap().as_millisecond(),
        )
        .unwrap()
    }
    #[test]
    fn system_zone_rules_preserve_phoenix_nepal_and_seasonal_dst() {
        for date in ["2026-01-15T12:00:00Z", "2026-07-15T12:00:00Z"] {
            let value = time("America/Phoenix", date);
            assert_eq!(value.time_zone.minutes(), -420);
            assert!(!value.time_zone.daylight_saving());
        }
        assert_eq!(
            time("America/New_York", "2026-01-15T12:00:00Z").time_zone,
            TimeZoneOffset::try_new(-300, false).unwrap()
        );
        assert_eq!(
            time("America/New_York", "2026-07-15T12:00:00Z").time_zone,
            TimeZoneOffset::try_new(-240, true).unwrap()
        );
        assert_eq!(
            time("Asia/Kathmandu", "2026-07-15T12:00:00Z")
                .time_zone
                .minutes(),
            345
        );
        assert_eq!(time("UTC", "1980-01-01T00:00:00Z").seconds_since_1980, 0);
        assert!(
            at(
                &TimeZone::get("Pacific/Kiritimati").unwrap(),
                1_790_000_000_000
            )
            .is_err()
        );
    }
}
