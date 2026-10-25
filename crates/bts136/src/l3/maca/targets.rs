use super::*;
const CHANNEL_BITS: usize = 11;
pub(in crate::l3) const MAX_OTHER_CHANNELS: usize = 15;
const MAX_OTHER_BANDS: usize = 3;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperbandChannels {
    pub hyperband: Hyperband,
    pub channels: Vec<u16>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasurementTargets {
    pub(super) serving: ControlChannel,
    pub(super) current: Vec<u16>,
    pub(super) other: Vec<HyperbandChannels>,
}
impl MeasurementTargets {
    pub fn try_new(
        serving: ControlChannel,
        current: Vec<u16>,
        other: Vec<HyperbandChannels>,
    ) -> Result<Self, Error> {
        serving
            .validate()
            .map_err(|_| Error::Field("maca_serving_channel"))?;
        validate_target_lists(
            if current.is_empty() {
                None
            } else {
                Some(&current)
            },
            &other,
        )?;
        if current.contains(&serving.channel)
            || other.iter().any(|list| list.hyperband == serving.hyperband)
        {
            return Err(Error::Field("maca_serving_channel"));
        }
        Ok(Self {
            serving,
            current,
            other,
        })
    }
    pub fn serving(&self) -> ControlChannel {
        self.serving
    }
    pub fn current_channels(&self) -> &[u16] {
        &self.current
    }
    pub fn other_channels(&self) -> &[HyperbandChannels] {
        &self.other
    }
}
pub(in crate::l3) fn validate_maca_channels(channels: &[u16]) -> Result<(), Error> {
    if channels.is_empty()
        || channels.len() > MAX_OTHER_CHANNELS
        || channels
            .iter()
            .any(|channel| u64::from(*channel) >= 1 << CHANNEL_BITS)
    {
        return Err(Error::Field("maca_channels"));
    }
    Ok(())
}
pub(in crate::l3) fn validate_target_lists(
    current: Option<&[u16]>,
    other: &[HyperbandChannels],
) -> Result<(), Error> {
    let mut total = 0;
    if let Some(channels) = current {
        validate_maca_channels(channels)?;
        total += channels.len();
    }
    if other.len() > MAX_OTHER_BANDS {
        return Err(Error::Field("maca_hyperband_count"));
    }
    let mut seen = 0u8;
    for list in other {
        let mask = 1 << (list.hyperband as u8);
        if seen & mask != 0 {
            return Err(Error::Field("duplicate_maca_hyperband"));
        }
        seen |= mask;
        validate_maca_channels(&list.channels)?;
        total += list.channels.len();
    }
    if total > MAX_OTHER_CHANNELS {
        return Err(Error::Field("maca_total_channels"));
    }
    Ok(())
}

pub(in crate::l3) fn write_maca_channels(
    bits: &mut Vec<bool>,
    channels: &[u16],
) -> Result<(), Error> {
    const COUNT_BITS: usize = 4;
    validate_maca_channels(channels)?;
    put(bits, (channels.len() - 1) as u64, COUNT_BITS)?;
    for channel in channels {
        put(bits, u64::from(*channel), CHANNEL_BITS)?;
    }
    Ok(())
}
pub(in crate::l3) fn read_maca_channels(reader: &mut Reader<'_>) -> Result<Vec<u16>, Error> {
    const COUNT_BITS: usize = 4;
    let count = reader.take(COUNT_BITS)? as usize + 1;
    if count > MAX_OTHER_CHANNELS {
        return Err(Error::Field("maca_channels"));
    }
    (0..count)
        .map(|_| reader.take(CHANNEL_BITS).map(|value| value as u16))
        .collect()
}
