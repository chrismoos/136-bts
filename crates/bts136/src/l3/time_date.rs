use super::broadcast::WireError;

pub const MESSAGE_TYPE: u8 = 16;
pub const UNIX_TO_1980_SECONDS: i64 = 315_532_800;
const MAX_OFFSET_MINUTES: i16 = 720;
const DIRECTION_BIT: u16 = 1 << 11;
const OFFSET_SHIFT: u32 = 1;
const MINUTES_MASK: u16 = 0x3ff;
const DST_MASK: u16 = 1;
const PACKET_OCTETS: usize = 7;
const PADDING_MASK: u8 = 0x0f;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeZoneOffset {
    minutes: i16,
    daylight_saving: bool,
}
impl TimeZoneOffset {
    pub fn try_new(minutes: i16, daylight_saving: bool) -> Result<Self, WireError> {
        if !(-MAX_OFFSET_MINUTES..=MAX_OFFSET_MINUTES).contains(&minutes) {
            return Err(WireError::Field("time_zone_offset_minutes"));
        }
        Ok(Self {
            minutes,
            daylight_saving,
        })
    }
    pub fn minutes(self) -> i16 {
        self.minutes
    }
    pub fn daylight_saving(self) -> bool {
        self.daylight_saving
    }
    pub(crate) fn bits(self) -> u16 {
        (if self.minutes > 0 { DIRECTION_BIT } else { 0 })
            | (self.minutes.unsigned_abs() << OFFSET_SHIFT)
            | u16::from(self.daylight_saving)
    }
    pub(crate) fn from_bits(bits: u16) -> Result<Self, WireError> {
        if bits >> 12 != 0 {
            return Err(WireError::Field("time_zone_offset"));
        }
        let magnitude = ((bits >> OFFSET_SHIFT) & MINUTES_MASK) as i16;
        Self::try_new(
            if bits & DIRECTION_BIT != 0 {
                magnitude
            } else {
                -magnitude
            },
            bits & DST_MASK != 0,
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeAndDate {
    pub seconds_since_1980: u32,
    pub time_zone: TimeZoneOffset,
}
impl TimeAndDate {
    // TIA-136-123-E §§5.1.2.2.8, 6.250–251 use GMT seconds since 1980 and a sign/magnitude minute offset followed by DST.
    pub fn encode(self) -> [u8; PACKET_OCTETS] {
        let seconds = self.seconds_since_1980.to_be_bytes();
        let offset = self.time_zone.bits() << 4;
        [
            MESSAGE_TYPE,
            seconds[0],
            seconds[1],
            seconds[2],
            seconds[3],
            (offset >> 8) as u8,
            offset as u8,
        ]
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() != PACKET_OCTETS {
            return Err(WireError::Truncated);
        }
        if bytes[0] != MESSAGE_TYPE {
            return Err(WireError::MessageType(bytes[0]));
        }
        if bytes[6] & PADDING_MASK != 0 {
            return Err(WireError::Field("time_date_padding"));
        }
        Ok(Self {
            seconds_since_1980: u32::from_be_bytes(
                bytes[1..5].try_into().expect("four counter octets"),
            ),
            time_zone: TimeZoneOffset::from_bits(
                (u16::from(bytes[5]) << 4) | u16::from(bytes[6] >> 4),
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_time_and_signed_offset_vectors_preserve_padding_and_dst() {
        for (minutes, dst, tail) in [
            (-420, false, [0x34, 0x80]),
            (-240, true, [0x1e, 0x10]),
            (330, false, [0xa9, 0x40]),
            (0, false, [0, 0]),
        ] {
            let value = TimeAndDate {
                seconds_since_1980: 0x12345678,
                time_zone: TimeZoneOffset::try_new(minutes, dst).unwrap(),
            };
            let wire = [0x10, 0x12, 0x34, 0x56, 0x78, tail[0], tail[1]];
            assert_eq!(value.encode(), wire);
            assert_eq!(TimeAndDate::decode(&wire).unwrap(), value);
        }
    }
    #[test]
    fn reserved_offsets_padding_and_bad_packet_lengths_are_rejected() {
        for minutes in [-721, 721, 840] {
            assert!(TimeZoneOffset::try_new(minutes, false).is_err());
        }
        let packet = TimeAndDate {
            seconds_since_1980: u32::MAX,
            time_zone: TimeZoneOffset::try_new(720, true).unwrap(),
        }
        .encode();
        assert!(TimeAndDate::decode(&packet).is_ok());
        for length in 0..packet.len() {
            assert!(TimeAndDate::decode(&packet[..length]).is_err());
        }
        let mut bad = packet;
        bad[6] |= 1;
        assert!(TimeAndDate::decode(&bad).is_err());
        let mut bad = packet;
        bad[5] = 0xff;
        bad[6] = 0xf0;
        assert!(TimeAndDate::decode(&bad).is_err());
    }
}
