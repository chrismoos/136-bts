use std::{error::Error, fmt};

const CHANNEL_SPACING_HZ: u64 = 30_000;
const CELLULAR_REVERSE_BASE_HZ: u64 = 825_000_000;
const CELLULAR_FORWARD_BASE_HZ: u64 = 870_000_000;
const CELLULAR_FIRST: u16 = 1;
const CELLULAR_LAST: u16 = 799;
const CELLULAR_WRAP_FIRST: u16 = 991;
const CELLULAR_WRAP_LAST: u16 = 1023;
const PCS_REVERSE_BASE_HZ: u64 = 1_849_980_000;
const PCS_FORWARD_BASE_HZ: u64 = 1_930_020_000;
const PCS_FIRST: u16 = 2;
const PCS_LAST: u16 = 1998;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Cellular800,
    Pcs1900,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplexFrequencies {
    pub reverse_hz: u64,
    pub forward_hz: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelError {
    pub band: Band,
    pub channel: u16,
}
impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "channel {} is not usable in {:?}",
            self.channel, self.band
        )
    }
}
impl Error for ChannelError {}

// IS-136.2-A Tables 2.1.1.1.1-1 and 2.1.1.1.2-1. Unused boundary channels are rejected.
pub fn frequencies(band: Band, channel: u16) -> Result<DuplexFrequencies, ChannelError> {
    match band {
        Band::Cellular800 if (CELLULAR_FIRST..=CELLULAR_LAST).contains(&channel) => {
            let offset = u64::from(channel) * CHANNEL_SPACING_HZ;
            Ok(DuplexFrequencies {
                reverse_hz: CELLULAR_REVERSE_BASE_HZ + offset,
                forward_hz: CELLULAR_FORWARD_BASE_HZ + offset,
            })
        }
        Band::Cellular800 if (CELLULAR_WRAP_FIRST..=CELLULAR_WRAP_LAST).contains(&channel) => {
            let offset = u64::from(CELLULAR_WRAP_LAST - channel) * CHANNEL_SPACING_HZ;
            Ok(DuplexFrequencies {
                reverse_hz: CELLULAR_REVERSE_BASE_HZ - offset,
                forward_hz: CELLULAR_FORWARD_BASE_HZ - offset,
            })
        }
        Band::Pcs1900 if (PCS_FIRST..=PCS_LAST).contains(&channel) => {
            let offset = u64::from(channel) * CHANNEL_SPACING_HZ;
            Ok(DuplexFrequencies {
                reverse_hz: PCS_REVERSE_BASE_HZ + offset,
                forward_hz: PCS_FORWARD_BASE_HZ + offset,
            })
        }
        _ => Err(ChannelError { band, channel }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn published_boundary_frequencies_and_duplex_offsets_match_tables() {
        for (band, channel, reverse_hz, forward_hz) in [
            (Band::Cellular800, 1, 825_030_000, 870_030_000),
            (Band::Cellular800, 333, 834_990_000, 879_990_000),
            (Band::Cellular800, 334, 835_020_000, 880_020_000),
            (Band::Cellular800, 799, 848_970_000, 893_970_000),
            (Band::Cellular800, 991, 824_040_000, 869_040_000),
            (Band::Cellular800, 1023, 825_000_000, 870_000_000),
            (Band::Pcs1900, 2, 1_850_040_000, 1_930_080_000),
            (Band::Pcs1900, 499, 1_864_950_000, 1_944_990_000),
            (Band::Pcs1900, 666, 1_869_960_000, 1_950_000_000),
            (Band::Pcs1900, 1998, 1_909_920_000, 1_989_960_000),
        ] {
            assert_eq!(
                frequencies(band, channel).unwrap(),
                DuplexFrequencies {
                    reverse_hz,
                    forward_hz
                }
            );
        }
        for channel in 0..=2048 {
            let cellular = frequencies(Band::Cellular800, channel);
            assert_eq!(
                cellular.is_ok(),
                (1..=799).contains(&channel) || (991..=1023).contains(&channel)
            );
            if let Ok(pair) = cellular {
                assert_eq!(pair.forward_hz - pair.reverse_hz, 45_000_000);
            }
            let pcs = frequencies(Band::Pcs1900, channel);
            assert_eq!(pcs.is_ok(), (2..=1998).contains(&channel));
            if let Ok(pair) = pcs {
                assert_eq!(pair.forward_hz - pair.reverse_hz, 80_040_000);
            }
        }
    }
}
