use super::{Direction, Reader, TrafficMessageError, put};
const FIRST: u8 = 0x51;
const SECOND: u8 = 0xa7;
const THIRD: u8 = 0xde;
const FOURTH: u8 = 0xa4;
const BER_BITS: usize = 3;
const RSSI_BITS: usize = 5;
const RSSI_MAX: u8 = 31;
const RSSI_BASE_DBM: i16 = -113;
const RSSI_STEP_DB: i16 = 2;
const BER_RANGES: [&str; 8] = [
    "<0.01",
    "0.01–<0.1",
    "0.1–<0.5",
    "0.5–<1.0",
    "1.0–<2.0",
    "2.0–<4.0",
    "4.0–<8.0",
    ">8.0",
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeighborGroup {
    Second,
    Third,
    Fourth,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownlinkQuality {
    pub ber_code: u8,
    pub rssi_code: u8,
}
impl DownlinkQuality {
    // TIA-136-133-E §2.4.1 Tables 1 and 3 contain quantized handset estimates.
    pub fn rssi_description(self) -> String {
        let value = RSSI_BASE_DBM + RSSI_STEP_DB * i16::from(self.rssi_code);
        match self.rssi_code {
            0 => format!("≤{value}"),
            RSSI_MAX => format!("≥{value}"),
            _ => value.to_string(),
        }
    }
    pub fn ber_range(self) -> &'static str {
        BER_RANGES
            .get(usize::from(self.ber_code))
            .copied()
            .unwrap_or("invalid")
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelQualityReport {
    First {
        current: DownlinkQuality,
        neighbor_rssi_codes: [u8; 6],
    },
    Later {
        group: NeighborGroup,
        neighbor_rssi_codes: [u8; 6],
    },
}
impl ChannelQualityReport {
    pub fn current(&self) -> Option<DownlinkQuality> {
        match self {
            Self::First { current, .. } => Some(*current),
            _ => None,
        }
    }
    pub(super) fn message_type(&self) -> u8 {
        match self {
            Self::First { .. } => FIRST,
            Self::Later { group, .. } => match group {
                NeighborGroup::Second => SECOND,
                NeighborGroup::Third => THIRD,
                NeighborGroup::Fourth => FOURTH,
            },
        }
    }
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        let neighbors = match self {
            Self::First {
                current,
                neighbor_rssi_codes,
            } => {
                if current.ber_code >= 1 << BER_BITS || current.rssi_code > RSSI_MAX {
                    return Err(TrafficMessageError::Field("current_channel_quality"));
                }
                put(bits, u64::from(current.ber_code), BER_BITS);
                put(bits, u64::from(current.rssi_code), RSSI_BITS);
                neighbor_rssi_codes
            }
            Self::Later {
                neighbor_rssi_codes,
                ..
            } => neighbor_rssi_codes,
        };
        for code in neighbors {
            if *code > RSSI_MAX {
                return Err(TrafficMessageError::Field("neighbor_rssi"));
            }
            put(bits, u64::from(*code), RSSI_BITS);
        }
        Ok(())
    }
}
// TIA-136-133-E §§5.1.2.2.3–6 define fixed 48-bit and 40-bit reports without Remaining Length.
pub(super) fn decode(
    kind: u8,
    reader: &mut Reader<'_>,
    direction: Direction,
) -> Result<Option<ChannelQualityReport>, TrafficMessageError> {
    if !matches!(kind, FIRST | SECOND | THIRD | FOURTH) {
        return Ok(None);
    }
    if direction != Direction::Reverse {
        return Err(TrafficMessageError::Field("channel_quality_direction"));
    }
    let current = if kind == FIRST {
        Some(DownlinkQuality {
            ber_code: reader.take(BER_BITS)? as u8,
            rssi_code: reader.take(RSSI_BITS)? as u8,
        })
    } else {
        None
    };
    let mut neighbors = [0; 6];
    for code in &mut neighbors {
        *code = reader.take(RSSI_BITS)? as u8;
    }
    Ok(Some(if let Some(current) = current {
        ChannelQualityReport::First {
            current,
            neighbor_rssi_codes: neighbors,
        }
    } else {
        ChannelQualityReport::Later {
            group: match kind {
                SECOND => NeighborGroup::Second,
                THIRD => NeighborGroup::Third,
                _ => NeighborGroup::Fourth,
            },
            neighbor_rssi_codes: neighbors,
        }
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handset_measurement_ranges_preserve_saturated_endpoints() {
        assert_eq!(
            DownlinkQuality {
                ber_code: 0,
                rssi_code: 0
            }
            .rssi_description(),
            "≤-113"
        );
        assert_eq!(
            DownlinkQuality {
                ber_code: 7,
                rssi_code: 31
            }
            .rssi_description(),
            "≥-51"
        );
        assert_eq!(
            DownlinkQuality {
                ber_code: 4,
                rssi_code: 17
            }
            .rssi_description(),
            "-79"
        );
        assert_eq!(
            DownlinkQuality {
                ber_code: 7,
                rssi_code: 31
            }
            .ber_range(),
            ">8.0"
        );
    }
}
