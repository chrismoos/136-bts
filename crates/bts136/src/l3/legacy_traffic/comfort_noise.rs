use super::{ProtocolVersion, Reader, TrafficMessage, TrafficMessageError, put};
use crate::l3::relay::Direction;
const MESSAGE_TYPE: u8 = 0xa6;
const LSF_BITS: usize = 26;
const ENERGY_BITS: usize = 8;
const RESC_BITS: usize = 4;
/// TIA-136-133-E §5.1.2.2.34 includes the TIA-136-410 LSF, energy and RESC indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComfortNoiseParameters {
    pub lsf_residual_vector: u32,
    pub energy_quantization_index: u8,
    pub resc_parameters: u8,
}
impl ComfortNoiseParameters {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), TrafficMessageError> {
        if version.code() < ProtocolVersion::Pv1.code() {
            return Err(TrafficMessageError::Field("comfort_noise_version"));
        }
        Ok(())
    }
    pub fn encode_for_version(
        &self,
        version: ProtocolVersion,
    ) -> Result<Vec<bool>, TrafficMessageError> {
        self.validate_version(version)?;
        TrafficMessage::ComfortNoise(*self).encode()
    }
    pub(super) fn message_type(&self) -> u8 {
        MESSAGE_TYPE
    }
    // TIA-136-133-E §5.1.2.2.34 defines exactly 48 bits without Remaining Length.
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        if self.lsf_residual_vector >= 1 << LSF_BITS || self.resc_parameters >= 1 << RESC_BITS {
            return Err(TrafficMessageError::Field("comfort_noise_parameters"));
        }
        put(bits, u64::from(self.lsf_residual_vector), LSF_BITS);
        put(bits, u64::from(self.energy_quantization_index), ENERGY_BITS);
        put(bits, u64::from(self.resc_parameters), RESC_BITS);
        Ok(())
    }
}
pub(super) fn decode(
    kind: u8,
    reader: &mut Reader<'_>,
    direction: Direction,
    version: Option<ProtocolVersion>,
) -> Result<Option<ComfortNoiseParameters>, TrafficMessageError> {
    if kind != MESSAGE_TYPE {
        return Ok(None);
    }
    if direction != Direction::Reverse {
        return Err(TrafficMessageError::Field("comfort_noise_direction"));
    }
    let parameters = ComfortNoiseParameters {
        lsf_residual_vector: reader.take(LSF_BITS)? as u32,
        energy_quantization_index: reader.take(ENERGY_BITS)? as u8,
        resc_parameters: reader.take(RESC_BITS)? as u8,
    };
    if let Some(version) = version {
        parameters.validate_version(version)?;
    }
    Ok(Some(parameters))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        l3::legacy_traffic::decode_prefix_in_version,
        link::traffic::{self, TrafficMessageReceiver},
    };
    fn vector() -> Vec<bool> {
        [0x29u8, 0x81, 0x23, 0x45, 0x69, 0xab]
            .into_iter()
            .flat_map(|byte| (0..u8::BITS).rev().map(move |bit| byte & (1 << bit) != 0))
            .collect()
    }
    fn parameters() -> ComfortNoiseParameters {
        ComfortNoiseParameters {
            lsf_residual_vector: 0x123456,
            energy_quantization_index: 0x9a,
            resc_parameters: 0xb,
        }
    }
    #[test]
    fn comfort_noise_matches_a_fixed_complete_word_without_optional_length() {
        let bits = vector();
        let decoded =
            decode_prefix_in_version(&bits, Direction::Reverse, ProtocolVersion::Pv1).unwrap();
        assert_eq!(decoded.message, TrafficMessage::ComfortNoise(parameters()));
        assert_eq!(decoded.consumed_bits, 48);
        assert_eq!(
            parameters()
                .encode_for_version(ProtocolVersion::Pv1)
                .unwrap(),
            bits
        );
    }
    #[test]
    fn comfort_noise_rejects_every_truncation_wrong_direction_and_older_versions() {
        let bits = vector();
        for length in 0..bits.len() {
            assert!(matches!(
                decode_prefix_in_version(&bits[..length], Direction::Reverse, ProtocolVersion::Pv1),
                Err(TrafficMessageError::Truncated { .. })
            ));
        }
        assert!(decode_prefix_in_version(&bits, Direction::Forward, ProtocolVersion::Pv1).is_err());
        for version in [ProtocolVersion::Is136Rev0, ProtocolVersion::Is136RevA] {
            assert!(parameters().encode_for_version(version).is_err());
            assert!(decode_prefix_in_version(&bits, Direction::Reverse, version).is_err());
        }
        for version in [
            ProtocolVersion::Pv1,
            ProtocolVersion::Pv2,
            ProtocolVersion::Pv3,
            ProtocolVersion::Pv4,
            ProtocolVersion::Pv5,
            ProtocolVersion::Pv6,
            ProtocolVersion::Pv7,
        ] {
            assert!(parameters().encode_for_version(version).is_ok());
        }
    }
    #[test]
    fn comfort_noise_field_limits_preserve_all_quantization_bits() {
        let maximum = ComfortNoiseParameters {
            lsf_residual_vector: (1 << LSF_BITS) - 1,
            energy_quantization_index: u8::MAX,
            resc_parameters: (1 << RESC_BITS) - 1,
        };
        let bits = maximum.encode_for_version(ProtocolVersion::Pv2).unwrap();
        assert_eq!(
            decode_prefix_in_version(&bits, Direction::Reverse, ProtocolVersion::Pv2)
                .unwrap()
                .message,
            TrafficMessage::ComfortNoise(maximum)
        );
        for invalid in [
            ComfortNoiseParameters {
                lsf_residual_vector: 1 << LSF_BITS,
                ..maximum
            },
            ComfortNoiseParameters {
                resc_parameters: 1 << RESC_BITS,
                ..maximum
            },
        ] {
            assert!(invalid.encode_for_version(ProtocolVersion::Pv2).is_err());
        }
    }
    #[test]
    fn traffic_word_receiver_enforces_direction_and_version_then_recovers() {
        let message = TrafficMessage::ComfortNoise(parameters());
        let words = traffic::encode(&message).unwrap();
        assert_eq!(words.len(), 1);
        let mut receiver =
            TrafficMessageReceiver::with_version(Direction::Reverse, ProtocolVersion::Pv2);
        assert_eq!(receiver.push(&words[0]).unwrap(), Some(message));
        for (direction, version) in [
            (Direction::Forward, ProtocolVersion::Pv2),
            (Direction::Reverse, ProtocolVersion::Is136RevA),
        ] {
            let mut receiver = TrafficMessageReceiver::with_version(direction, version);
            assert!(receiver.push(&words[0]).is_err());
            let release = TrafficMessage::Release {
                information: Default::default(),
            };
            let words = traffic::encode(&release).unwrap();
            assert_eq!(receiver.push(&words[0]).unwrap(), Some(release));
        }
    }
}
