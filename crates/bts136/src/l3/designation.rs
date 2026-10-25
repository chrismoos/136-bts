use super::broadcast::ProtocolVersion;
use std::{error::Error, fmt};
const PD_BITS: usize = 2;
const TYPE_BITS: usize = 6;
const DVCC_BITS: usize = 8;
const DMAC_BITS: usize = 4;
const CHANNEL_BITS: usize = 11;
const SLOT_BITS: usize = 4;
const FLAG_BITS: usize = 1;
const VERSION_BITS: usize = 4;
const ALIGNMENT_BITS: usize = 5;
const OCTET_BITS: usize = 8;
const DESIGNATION: u64 = 21;
const MANDATORY_BITS: usize = 46;
const LAST_VOICE_CODER: u8 = 5;
const NO_PRIVACY: u8 = 0;
const CMEA_PRIVACY: u8 = 1;
const SCEMA_PRIVACY: u8 = 2;
const SOC_PRIVACY: u8 = 4;
const VOICE_PARAMETER: u64 = 1;
const PARAMETER_BITS: usize = 4;
const VOICE_BITS: usize = 3;
const MAINTAIN_ALIGNMENT: u8 = 31;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AssignedSlots {
    Full14 = 1,
    Full25 = 2,
    Full36 = 3,
    Double14And25 = 4,
    Double14And36 = 5,
    Double25And36 = 6,
    Half1 = 9,
    Half2 = 10,
    Half3 = 11,
    Half4 = 12,
    Half5 = 13,
    Half6 = 14,
    TripleFull = 15,
}
impl AssignedSlots {
    // IS-136.1-A §6.5 ATS. Bit zero denotes physical slot 1.
    pub fn slot_mask(self) -> u8 {
        match self {
            Self::Full14 => 0b001001,
            Self::Full25 => 0b010010,
            Self::Full36 => 0b100100,
            Self::Double14And25 => 0b011011,
            Self::Double14And36 => 0b101101,
            Self::Double25And36 => 0b110110,
            Self::Half1 => 0b000001,
            Self::Half2 => 0b000010,
            Self::Half3 => 0b000100,
            Self::Half4 => 0b001000,
            Self::Half5 => 0b010000,
            Self::Half6 => 0b100000,
            Self::TripleFull => 0b111111,
        }
    }

    fn decode(code: u8) -> Result<Self, DesignationError> {
        const SLOTS: [AssignedSlots; 13] = [
            AssignedSlots::Full14,
            AssignedSlots::Full25,
            AssignedSlots::Full36,
            AssignedSlots::Double14And25,
            AssignedSlots::Double14And36,
            AssignedSlots::Double25And36,
            AssignedSlots::Half1,
            AssignedSlots::Half2,
            AssignedSlots::Half3,
            AssignedSlots::Half4,
            AssignedSlots::Half5,
            AssignedSlots::Half6,
            AssignedSlots::TripleFull,
        ];
        SLOTS
            .into_iter()
            .find(|slot| *slot as u8 == code)
            .ok_or(DesignationError::Field("assigned_slots"))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeAlignment {
    OffsetUnits(u8),
    Maintain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Designation {
    pub dvcc: u8,
    pub dmac: u8,
    pub channel: u16,
    pub assigned_slots: AssignedSlots,
    pub shortened_burst: bool,
    pub protocol_version: ProtocolVersion,
    pub time_alignment: TimeAlignment,
    pub delay_interval_compensation: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceMode {
    pub coder: u8,
    pub privacy: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedDesignation {
    pub message: Designation,
    pub voice_mode: Option<VoiceMode>,
    pub consumed_bits: usize,
    /// Unrecognized optional fields and final padding retain their wire bits.
    pub remaining_bits: Vec<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignationError {
    Truncated { needed_bits: usize },
    Protocol,
    MessageType,
    Field(&'static str),
}
impl fmt::Display for DesignationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid digital traffic designation: {self:?}")
    }
}
impl Error for DesignationError {}
impl Designation {
    // IS-136.1-A §6.4.3.6. Absent optional fields select current hyperband and VSELP without privacy.
    pub fn encode(self) -> Result<Vec<u8>, DesignationError> {
        self.encode_with_voice(None)
    }
    pub fn encode_with_voice(self, voice: Option<VoiceMode>) -> Result<Vec<u8>, DesignationError> {
        if voice.is_some_and(|v| {
            v.coder > LAST_VOICE_CODER
                || !matches!(
                    v.privacy,
                    NO_PRIVACY | CMEA_PRIVACY | SCEMA_PRIVACY | SOC_PRIVACY
                )
        }) {
            return Err(DesignationError::Field("voice_mode"));
        }
        if self.dvcc == crate::phy::color::RESERVED_TRAFFIC_DVCC {
            return Err(DesignationError::Field("dvcc"));
        }
        if self.dmac >= 1 << DMAC_BITS {
            return Err(DesignationError::Field("dmac"));
        }
        if self.channel >= 1 << CHANNEL_BITS {
            return Err(DesignationError::Field("channel"));
        }
        let alignment = match self.time_alignment {
            TimeAlignment::OffsetUnits(units) if units < MAINTAIN_ALIGNMENT => units,
            TimeAlignment::OffsetUnits(_) => return Err(DesignationError::Field("time_alignment")),
            TimeAlignment::Maintain => MAINTAIN_ALIGNMENT,
        };
        let mut bits = Vec::new();
        for (value, width) in [
            (0, PD_BITS),
            (DESIGNATION, TYPE_BITS),
            (u64::from(self.dvcc), DVCC_BITS),
            (u64::from(self.dmac), DMAC_BITS),
            (u64::from(self.channel), CHANNEL_BITS),
            (self.assigned_slots as u64, SLOT_BITS),
            (u64::from(self.shortened_burst), FLAG_BITS),
            (u64::from(self.protocol_version.code()), VERSION_BITS),
            (u64::from(alignment), ALIGNMENT_BITS),
            (u64::from(self.delay_interval_compensation), FLAG_BITS),
        ] {
            bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
        }
        if let Some(voice) = voice {
            for (value, width) in [
                (VOICE_PARAMETER, PARAMETER_BITS),
                (u64::from(voice.coder), VOICE_BITS),
                (u64::from(voice.privacy), VOICE_BITS),
            ] {
                bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
            }
        }
        bits.resize(bits.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
        Ok(bits
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|octet| {
                octet
                    .iter()
                    .fold(0, |value, bit| value * 2 + u8::from(*bit))
            })
            .collect())
    }
}
pub fn decode_prefix(octets: &[u8]) -> Result<DecodedDesignation, DesignationError> {
    let bits: Vec<bool> = octets
        .iter()
        .flat_map(|octet| {
            (0..OCTET_BITS)
                .rev()
                .map(move |bit| octet & (1 << bit) != 0)
        })
        .collect();
    let mut offset = 0;
    let mut take = |width: usize| {
        let end = offset + width;
        let field = bits
            .get(offset..end)
            .ok_or(DesignationError::Truncated { needed_bits: end })?;
        offset = end;
        Ok::<u64, DesignationError>(
            field
                .iter()
                .fold(0, |value, bit| value * 2 + u64::from(*bit)),
        )
    };
    if take(PD_BITS)? != 0 {
        return Err(DesignationError::Protocol);
    }
    if take(TYPE_BITS)? != DESIGNATION {
        return Err(DesignationError::MessageType);
    }
    let dvcc = take(DVCC_BITS)? as u8;
    if dvcc == crate::phy::color::RESERVED_TRAFFIC_DVCC {
        return Err(DesignationError::Field("dvcc"));
    }
    let dmac = take(DMAC_BITS)? as u8;
    let channel = take(CHANNEL_BITS)? as u16;
    let assigned_slots = AssignedSlots::decode(take(SLOT_BITS)? as u8)?;
    let shortened_burst = take(FLAG_BITS)? != 0;
    let protocol_version = ProtocolVersion::decode(take(VERSION_BITS)? as u8)
        .map_err(|_| DesignationError::Field("protocol_version"))?;
    let alignment = take(ALIGNMENT_BITS)? as u8;
    let time_alignment = if alignment == MAINTAIN_ALIGNMENT {
        TimeAlignment::Maintain
    } else {
        TimeAlignment::OffsetUnits(alignment)
    };
    let delay_interval_compensation = take(FLAG_BITS)? != 0;
    let voice_mode = if bits
        .get(MANDATORY_BITS..MANDATORY_BITS + PARAMETER_BITS)
        .is_some_and(|field| {
            field.iter().fold(0u64, |v, &b| v * 2 + u64::from(b)) == VOICE_PARAMETER
        }) {
        take(PARAMETER_BITS)?;
        Some(VoiceMode {
            coder: take(VOICE_BITS)? as u8,
            privacy: take(VOICE_BITS)? as u8,
        })
    } else {
        None
    };
    Ok(DecodedDesignation {
        message: Designation {
            dvcc,
            dmac,
            channel,
            assigned_slots,
            shortened_burst,
            protocol_version,
            time_alignment,
            delay_interval_compensation,
        },
        voice_mode,
        consumed_bits: offset,
        remaining_bits: bits[offset..].to_vec(),
    })
}
#[cfg(test)]
mod tests {
    #[test]
    fn zero_dvcc_cannot_be_assigned_in_either_wire_direction() {
        let invalid = Designation {
            dvcc: 0,
            ..message()
        };
        assert_eq!(invalid.encode(), Err(DesignationError::Field("dvcc")));
        let mut bytes = message().encode().unwrap();
        bytes[1] = 0;
        assert_eq!(decode_prefix(&bytes), Err(DesignationError::Field("dvcc")));
    }

    use super::*;
    fn message() -> Designation {
        Designation {
            dvcc: 42,
            dmac: 3,
            channel: 1,
            assigned_slots: AssignedSlots::Full14,
            shortened_burst: true,
            protocol_version: ProtocolVersion::Is136RevA,
            time_alignment: TimeAlignment::Maintain,
            delay_interval_compensation: false,
        }
    }
    #[test]
    fn wire_vector_and_alignment_sentinel_match_standard_fields() {
        let bits = "00010101 00101010 0011 00000000001 0001 1 0100 11111 0".replace(' ', "");
        let mut padded = bits.clone();
        padded.push_str("00");
        let expected: Vec<u8> = padded
            .as_bytes()
            .chunks(8)
            .map(|octet| {
                octet
                    .iter()
                    .fold(0, |value, bit| value * 2 + u8::from(*bit == b'1'))
            })
            .collect();
        assert_eq!(message().encode().unwrap(), expected);
        let decoded = decode_prefix(&expected).unwrap();
        assert_eq!(decoded.message, message());
        assert_eq!(decoded.consumed_bits, 46);
        assert_eq!(decoded.remaining_bits, [false; 2]);
        for units in 0..31 {
            let message = Designation {
                time_alignment: TimeAlignment::OffsetUnits(units),
                ..message()
            };
            assert_eq!(
                decode_prefix(&message.encode().unwrap()).unwrap().message,
                message
            );
        }
    }
    #[test]
    fn reserved_slot_codes_truncation_and_bounds_are_rejected() {
        for code in [0, 7, 8] {
            assert_eq!(
                AssignedSlots::decode(code),
                Err(DesignationError::Field("assigned_slots"))
            );
        }
        assert_eq!(
            Designation {
                dmac: 16,
                ..message()
            }
            .encode(),
            Err(DesignationError::Field("dmac"))
        );
        assert_eq!(
            Designation {
                channel: 2048,
                ..message()
            }
            .encode(),
            Err(DesignationError::Field("channel"))
        );
        assert_eq!(
            Designation {
                time_alignment: TimeAlignment::OffsetUnits(31),
                ..message()
            }
            .encode(),
            Err(DesignationError::Field("time_alignment"))
        );
        assert!(matches!(
            decode_prefix(&message().encode().unwrap()[..5]),
            Err(DesignationError::Truncated { .. })
        ));
    }
    #[test]
    fn acelp_voice_mode_is_explicit_before_octet_padding() {
        let encoded = message()
            .encode_with_voice(Some(VoiceMode {
                coder: 2,
                privacy: 0,
            }))
            .unwrap();
        let wire = "00010101001010100011000000000010001101001111100001010000";
        let expected: Vec<_> = wire
            .as_bytes()
            .as_chunks::<8>()
            .0
            .iter()
            .map(|octet| {
                octet
                    .iter()
                    .fold(0u8, |value, &bit| value * 2 + u8::from(bit == b'1'))
            })
            .collect();
        assert_eq!(encoded, expected);
        let decoded = decode_prefix(&encoded).unwrap();
        assert_eq!(decoded.message, message());
        assert_eq!(
            decoded.voice_mode,
            Some(VoiceMode {
                coder: 2,
                privacy: 0
            })
        );
        assert_eq!(decoded.consumed_bits, 56);
        assert!(decoded.remaining_bits.is_empty());
        assert_eq!(
            decode_prefix(&message().encode().unwrap())
                .unwrap()
                .voice_mode,
            None
        );
        for voice in [
            VoiceMode {
                coder: 6,
                privacy: 0,
            },
            VoiceMode {
                coder: 2,
                privacy: 7,
            },
        ] {
            assert_eq!(
                message().encode_with_voice(Some(voice)),
                Err(DesignationError::Field("voice_mode"))
            );
        }
    }
}
