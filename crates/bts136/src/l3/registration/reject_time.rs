use crate::l3::control_fields::Error;

pub const DEFAULT_REJECT_SUPERFRAMES: u16 = 2048;
const POWER_DOWN_BOUNDARY: u8 = 15;
const SUPERFRAMES_PER_BOUNDARY: u16 = 100;
const RANDOM_DENOMINATOR: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectTime {
    lower: u8,
    upper: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationRetry {
    Superframes(u16),
    UntilPowerDown,
}
impl RejectTime {
    pub fn try_new(lower: u8, upper: u8) -> Result<Self, Error> {
        if lower > POWER_DOWN_BOUNDARY
            || upper > POWER_DOWN_BOUNDARY
            || (lower != POWER_DOWN_BOUNDARY && upper != POWER_DOWN_BOUNDARY && lower > upper)
        {
            return Err(Error::Field("reject_time_boundaries"));
        }
        Ok(Self { lower, upper })
    }
    pub fn lower(self) -> u8 {
        self.lower
    }
    pub fn upper(self) -> u8 {
        self.upper
    }
    pub fn until_power_down(self) -> bool {
        self.lower == POWER_DOWN_BOUNDARY || self.upper == POWER_DOWN_BOUNDARY
    }
    /// TIA-136-123-E §6.200 permits uniformly selected RANDNO from 0.1 through 1.0.
    pub fn retry_after(self, random_tenths: u8) -> Result<RegistrationRetry, Error> {
        if !(1..=RANDOM_DENOMINATOR as u8).contains(&random_tenths) {
            return Err(Error::Field("reject_time_random"));
        }
        if self.until_power_down() {
            return Ok(RegistrationRetry::UntilPowerDown);
        }
        let lower = u16::from(self.lower) * SUPERFRAMES_PER_BOUNDARY;
        let spread = u16::from(self.upper - self.lower) * SUPERFRAMES_PER_BOUNDARY;
        Ok(RegistrationRetry::Superframes(
            lower + spread * u16::from(random_tenths) / RANDOM_DENOMINATOR,
        ))
    }
}

#[cfg(test)]
mod tests;
