use super::{LENGTH_BITS, MAX_OPTIONAL_OCTETS, OCTET_BITS, Reader, TrafficMessageError, put};
use crate::l3::broadcast::ProtocolVersion;
const SERIAL: u64 = 1;
const CALL_MODE: u64 = 2;
const TERMINAL: u64 = 3;
const PRIVACY: u64 = 4;
const ENCRYPTION: u64 = 5;
const TASK: u64 = 6;
const EXTENSION: u64 = 7;
const SERIAL_EXPANSION: u64 = 1;
const EQUIPMENT: u64 = 2;
const PARAMETER_BITS: usize = 4;
const COUNT_BITS: usize = 6;
const SERIAL_BITS: usize = 32;
const CALL_MODE_BITS: usize = 5;
const TERMINAL_BITS: usize = 32;
const FLAG_BITS: usize = 1;
const TASK_BITS: usize = 3;
const EXPANSION_BITS: usize = 24;
const EQUIPMENT_BITS: usize = 56;
const VERSION_BITS: usize = 4;
const FIRMWARE_BITS: usize = 6;
const OVERLOAD_BITS: usize = 4;
const MAX_VALUES: usize = (1 << COUNT_BITS) - 1;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficTask {
    WaitingOrder,
    WaitingAnswer,
    Conversation,
}
impl TrafficTask {
    fn code(self) -> u64 {
        match self {
            Self::WaitingOrder => 0,
            Self::WaitingAnswer => 1,
            Self::Conversation => 2,
        }
    }
    fn decode(code: u64) -> Result<Self, TrafficMessageError> {
        match code {
            0 => Ok(Self::WaitingOrder),
            1 => Ok(Self::WaitingAnswer),
            2 => Ok(Self::Conversation),
            _ => Err(TrafficMessageError::Field("task")),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalInformation {
    pub protocol_version: ProtocolVersion,
    pub manufacturer: u8,
    pub model: u8,
    pub firmware: u8,
    pub access_overload_class: u8,
    pub local_control: bool,
    pub registration: bool,
}
impl TerminalInformation {
    // TIA-136-133-E §5.1.2.4.69 lists these subfields in most-significant-bit order.
    fn encode(self) -> Result<u64, TrafficMessageError> {
        if self.firmware >= 1 << FIRMWARE_BITS || self.access_overload_class >= 1 << OVERLOAD_BITS {
            return Err(TrafficMessageError::Field("terminal_information"));
        }
        let mut bits = Vec::new();
        for (value, width) in [
            (u64::from(self.protocol_version.code()), VERSION_BITS),
            (u64::from(self.manufacturer), OCTET_BITS),
            (u64::from(self.model), OCTET_BITS),
            (u64::from(self.firmware), FIRMWARE_BITS),
            (u64::from(self.access_overload_class), OVERLOAD_BITS),
            (u64::from(self.local_control), FLAG_BITS),
            (u64::from(self.registration), FLAG_BITS),
        ] {
            put(&mut bits, value, width);
        }
        Ok(bits
            .into_iter()
            .fold(0, |value, bit| value * 2 + u64::from(bit)))
    }
    fn decode(value: u64) -> Result<Self, TrafficMessageError> {
        let mut bits = Vec::new();
        put(&mut bits, value, TERMINAL_BITS);
        let mut reader = Reader {
            bits: &bits,
            offset: 0,
        };
        Ok(Self {
            protocol_version: ProtocolVersion::decode(reader.take(VERSION_BITS)? as u8)
                .map_err(|_| TrafficMessageError::Field("terminal_protocol_version"))?,
            manufacturer: reader.take(OCTET_BITS)? as u8,
            model: reader.take(OCTET_BITS)? as u8,
            firmware: reader.take(FIRMWARE_BITS)? as u8,
            access_overload_class: reader.take(OVERLOAD_BITS)? as u8,
            local_control: reader.take(FLAG_BITS)? != 0,
            registration: reader.take(FLAG_BITS)? != 0,
        })
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusFields {
    pub serial_numbers: Option<Vec<u32>>,
    pub call_modes: Option<Vec<u8>>,
    pub terminals: Option<Vec<TerminalInformation>>,
    pub privacy_modes: Option<Vec<bool>>,
    pub encryption_modes: Option<Vec<bool>>,
    pub tasks: Option<Vec<TrafficTask>>,
    pub serial_expansion: Option<u32>,
    pub equipment_identities: Option<Vec<u64>>,
    pub unparsed_bits: Vec<bool>,
}
impl StatusFields {
    pub(crate) fn update(&mut self, incoming: &Self) {
        if incoming.serial_numbers.is_some() {
            self.serial_numbers.clone_from(&incoming.serial_numbers);
        }
        if incoming.call_modes.is_some() {
            self.call_modes.clone_from(&incoming.call_modes);
        }
        if incoming.terminals.is_some() {
            self.terminals.clone_from(&incoming.terminals);
        }
        if incoming.privacy_modes.is_some() {
            self.privacy_modes.clone_from(&incoming.privacy_modes);
        }
        if incoming.encryption_modes.is_some() {
            self.encryption_modes.clone_from(&incoming.encryption_modes);
        }
        if incoming.tasks.is_some() {
            self.tasks.clone_from(&incoming.tasks);
        }
        if incoming.serial_expansion.is_some() {
            self.serial_expansion = incoming.serial_expansion;
        }
        if incoming.equipment_identities.is_some() {
            self.equipment_identities
                .clone_from(&incoming.equipment_identities);
        }
        self.unparsed_bits.clone_from(&incoming.unparsed_bits);
    }
    pub(super) fn validate_request(&self) -> Result<(), TrafficMessageError> {
        if self
            .serial_numbers
            .as_ref()
            .is_some_and(|values| !values.is_empty())
            || self
                .terminals
                .as_ref()
                .is_some_and(|values| !values.is_empty())
            || self.serial_expansion.is_some()
            || self
                .equipment_identities
                .as_ref()
                .is_some_and(|values| !values.is_empty())
        {
            return Err(TrafficMessageError::Field("status_request_fields"));
        }
        Ok(())
    }
    pub fn identity_query() -> Self {
        Self {
            serial_numbers: Some(Vec::new()),
            call_modes: Some(Vec::new()),
            terminals: Some(Vec::new()),
            ..Default::default()
        }
    }
    pub(crate) fn answers_identity_query(&self) -> bool {
        self.serial_numbers
            .as_ref()
            .is_some_and(|values| !values.is_empty())
            && self
                .call_modes
                .as_ref()
                .is_some_and(|values| !values.is_empty())
            && self
                .terminals
                .as_ref()
                .is_some_and(|values| !values.is_empty())
    }
    // TIA-136-133-E §§5.1.2.2.13 and 9.1.2.2.17 use value counts, followed by final zero padding.
    pub(super) fn encode_octets(&self) -> Result<Vec<u8>, TrafficMessageError> {
        let mut bits = Vec::new();
        if let Some(values) = &self.serial_numbers {
            write_values(
                &mut bits,
                SERIAL,
                SERIAL_BITS,
                values.iter().map(|value| u64::from(*value)),
            )?;
        }
        if let Some(values) = &self.call_modes {
            write_values(
                &mut bits,
                CALL_MODE,
                CALL_MODE_BITS,
                values.iter().map(|value| u64::from(*value)),
            )?;
        }
        if let Some(values) = &self.terminals {
            write_values(
                &mut bits,
                TERMINAL,
                TERMINAL_BITS,
                values
                    .iter()
                    .map(|value| value.encode())
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter(),
            )?;
        }
        if let Some(values) = &self.privacy_modes {
            write_values(
                &mut bits,
                PRIVACY,
                FLAG_BITS,
                values.iter().map(|value| u64::from(*value)),
            )?;
        }
        if let Some(values) = &self.encryption_modes {
            write_values(
                &mut bits,
                ENCRYPTION,
                FLAG_BITS,
                values.iter().map(|value| u64::from(*value)),
            )?;
        }
        if let Some(values) = &self.tasks {
            write_values(
                &mut bits,
                TASK,
                TASK_BITS,
                values.iter().map(|value| value.code()),
            )?;
        }
        if let Some(value) = self.serial_expansion {
            if u64::from(value) >= 1 << EXPANSION_BITS {
                return Err(TrafficMessageError::Field("serial_expansion"));
            }
            put(&mut bits, EXTENSION, PARAMETER_BITS);
            put(&mut bits, SERIAL_EXPANSION, PARAMETER_BITS);
            put(&mut bits, (EXPANSION_BITS / OCTET_BITS) as u64, LENGTH_BITS);
            put(&mut bits, u64::from(value), EXPANSION_BITS);
        }
        if let Some(values) = &self.equipment_identities {
            put(&mut bits, EXTENSION, PARAMETER_BITS);
            write_values(&mut bits, EQUIPMENT, EQUIPMENT_BITS, values.iter().copied())?;
        }
        bits.extend_from_slice(&self.unparsed_bits);
        bits.resize(bits.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
        if bits.len() > MAX_OPTIONAL_OCTETS * OCTET_BITS {
            return Err(TrafficMessageError::OptionalLength);
        }
        Ok(bits
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|byte| byte.iter().fold(0, |value, bit| value * 2 + u8::from(*bit)))
            .collect())
    }
}
fn write_values(
    bits: &mut Vec<bool>,
    parameter: u64,
    width: usize,
    values: impl ExactSizeIterator<Item = u64>,
) -> Result<(), TrafficMessageError> {
    if values.len() > MAX_VALUES {
        return Err(TrafficMessageError::Field("status_value_count"));
    }
    put(bits, parameter, PARAMETER_BITS);
    put(bits, values.len() as u64, COUNT_BITS);
    for value in values {
        if value >= 1 << width {
            return Err(TrafficMessageError::Field("status_value"));
        }
        put(bits, value, width);
    }
    Ok(())
}
pub(super) fn decode(octets: &[u8]) -> Result<StatusFields, TrafficMessageError> {
    if octets.len() > MAX_OPTIONAL_OCTETS {
        return Err(TrafficMessageError::OptionalLength);
    }
    let bits: Vec<_> = octets
        .iter()
        .flat_map(|byte| (0..OCTET_BITS).rev().map(move |bit| byte & (1 << bit) != 0))
        .collect();
    let mut reader = Reader {
        bits: &bits,
        offset: 0,
    };
    let mut fields = StatusFields::default();
    let mut last_parameter = 0;
    while reader.offset < bits.len() {
        if bits.len() - reader.offset < OCTET_BITS && bits[reader.offset..].iter().all(|bit| !bit) {
            break;
        }
        let start = reader.offset;
        let parameter = reader.take(PARAMETER_BITS)?;
        if parameter == 0 {
            return Err(TrafficMessageError::Field("status_parameter"));
        }
        if parameter == EXTENSION {
            let auxiliary = reader.take(PARAMETER_BITS)?;
            if matches!(auxiliary, SERIAL_EXPANSION | EQUIPMENT) {
                let rank = EXTENSION + auxiliary;
                if rank < last_parameter {
                    return Err(TrafficMessageError::Field("status_parameter_order"));
                }
                last_parameter = rank;
            }
            match auxiliary {
                SERIAL_EXPANSION => {
                    if fields.serial_expansion.is_some() {
                        return Err(TrafficMessageError::Field("duplicate_status_parameter"));
                    }
                    if reader.take(LENGTH_BITS)? != (EXPANSION_BITS / OCTET_BITS) as u64 {
                        return Err(TrafficMessageError::Field("serial_expansion_length"));
                    }
                    fields.serial_expansion = Some(reader.take(EXPANSION_BITS)? as u32);
                }
                EQUIPMENT => read_values(
                    &mut reader,
                    &mut fields.equipment_identities,
                    EQUIPMENT_BITS,
                    Ok,
                )?,
                _ => {
                    fields.unparsed_bits = bits[start..].to_vec();
                    break;
                }
            }
            continue;
        }
        if parameter <= TASK {
            if parameter < last_parameter {
                return Err(TrafficMessageError::Field("status_parameter_order"));
            }
            last_parameter = parameter;
        }
        match parameter {
            SERIAL => read_values(
                &mut reader,
                &mut fields.serial_numbers,
                SERIAL_BITS,
                |value| Ok(value as u32),
            )?,
            CALL_MODE => read_values(
                &mut reader,
                &mut fields.call_modes,
                CALL_MODE_BITS,
                |value| Ok(value as u8),
            )?,
            TERMINAL => read_values(
                &mut reader,
                &mut fields.terminals,
                TERMINAL_BITS,
                TerminalInformation::decode,
            )?,
            PRIVACY => read_values(&mut reader, &mut fields.privacy_modes, FLAG_BITS, |value| {
                Ok(value != 0)
            })?,
            ENCRYPTION => read_values(
                &mut reader,
                &mut fields.encryption_modes,
                FLAG_BITS,
                |value| Ok(value != 0),
            )?,
            TASK => read_values(
                &mut reader,
                &mut fields.tasks,
                TASK_BITS,
                TrafficTask::decode,
            )?,
            _ => {
                fields.unparsed_bits = bits[start..].to_vec();
                break;
            }
        }
    }
    Ok(fields)
}
fn read_values<T>(
    reader: &mut Reader<'_>,
    slot: &mut Option<Vec<T>>,
    width: usize,
    convert: impl Fn(u64) -> Result<T, TrafficMessageError>,
) -> Result<(), TrafficMessageError> {
    if slot.is_some() {
        return Err(TrafficMessageError::Field("duplicate_status_parameter"));
    }
    let count = reader.take(COUNT_BITS)? as usize;
    let values = (0..count)
        .map(|_| convert(reader.take(width)?))
        .collect::<Result<Vec<_>, _>>()?;
    *slot = Some(values);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::legacy_traffic::{self, TrafficMessage};
    fn wire(text: &str) -> Vec<bool> {
        text.chars()
            .filter(|bit| *bit != ' ')
            .map(|bit| bit == '1')
            .collect()
    }
    fn terminal() -> TerminalInformation {
        TerminalInformation {
            protocol_version: ProtocolVersion::Is136RevA,
            manufacturer: 0x22,
            model: 0x33,
            firmware: 17,
            access_overload_class: 2,
            local_control: true,
            registration: false,
        }
    }
    #[test]
    fn status_query_and_report_match_independent_bit_vectors() {
        let query = TrafficMessage::StatusRequest(StatusFields::identity_query());
        let expected = wire("00 11110111 000100 0001 000000 0010 000000 0011 000000 00");
        assert_eq!(query.encode().unwrap(), expected);
        assert_eq!(expected.len(), 48);
        let report = TrafficMessage::Status(StatusFields {
            serial_numbers: Some(vec![0x12345678]),
            call_modes: Some(vec![3]),
            terminals: Some(vec![terminal()]),
            ..Default::default()
        });
        assert_eq!(terminal().encode().unwrap(), 0x4223344a);
        let reported = wire(
            "00 10010111 001101 0001 000001 00010010001101000101011001111000 0010 000001 00011 0011 000001 01000010001000110011010001001010 00000",
        );
        assert_eq!(report.encode().unwrap(), reported);
        assert_eq!(reported.len(), 120);
        for (message, bits, direction) in [
            (query, expected, crate::l3::relay::Direction::Forward),
            (report, reported, crate::l3::relay::Direction::Reverse),
        ] {
            let decoded = legacy_traffic::decode_prefix_in(&bits, direction).unwrap();
            assert_eq!(decoded.message, message);
            assert_eq!(decoded.consumed_bits, bits.len());
            for length in 0..bits.len() {
                assert!(legacy_traffic::decode_prefix_in(&bits[..length], direction).is_err());
            }
        }
    }
    #[test]
    fn status_extended_identities_and_modes_keep_counts_and_padding() {
        let fields = StatusFields {
            privacy_modes: Some(vec![false, true]),
            encryption_modes: Some(vec![false]),
            tasks: Some(vec![
                TrafficTask::WaitingOrder,
                TrafficTask::WaitingAnswer,
                TrafficTask::Conversation,
            ]),
            serial_expansion: Some(0xaabbcc),
            equipment_identities: Some(vec![0x123456789abcde]),
            ..Default::default()
        };
        let encoded = fields.encode_octets().unwrap();
        assert_eq!(decode(&encoded).unwrap(), fields);
        let max = StatusFields {
            serial_numbers: Some(vec![1; 15]),
            ..Default::default()
        };
        assert_eq!(decode(&max.encode_octets().unwrap()).unwrap(), max);
        let too_many = StatusFields {
            serial_numbers: Some(vec![1; 16]),
            ..Default::default()
        };
        assert_eq!(
            too_many.encode_octets(),
            Err(TrafficMessageError::OptionalLength)
        );
        for fields in [
            StatusFields {
                call_modes: Some(vec![32]),
                ..Default::default()
            },
            StatusFields {
                serial_expansion: Some(1 << 24),
                ..Default::default()
            },
            StatusFields {
                equipment_identities: Some(vec![1 << 56]),
                ..Default::default()
            },
            StatusFields {
                terminals: Some(vec![TerminalInformation {
                    firmware: 64,
                    ..terminal()
                }]),
                ..Default::default()
            },
        ] {
            assert!(fields.encode_octets().is_err());
        }
    }
    #[test]
    fn malformed_status_counts_duplicates_reserved_tasks_and_unknown_tails_are_distinguished() {
        for (bits, error) in [
            ("0001 000001 00000000 000000", "truncated"),
            ("0001 000000 0001 000000 0000", "duplicate"),
            ("0011 000000 0001 000000 0000", "order"),
            ("0110 000001 111 000", "task"),
            ("0111 0001 000010 0000000000000000 00", "expansion"),
            ("0000 0000", "parameter"),
        ] {
            let bits = wire(bits);
            assert_eq!(bits.len() % OCTET_BITS, 0, "{error}");
            let bytes: Vec<_> = bits
                .as_chunks::<OCTET_BITS>()
                .0
                .iter()
                .map(|byte| byte.iter().fold(0, |value, bit| value * 2 + u8::from(*bit)))
                .collect();
            assert!(decode(&bytes).is_err(), "{error}");
        }
        let unknown = vec![0x90, 0xaa];
        let decoded = decode(&unknown).unwrap();
        assert_eq!(decoded.unparsed_bits.len(), 16);
        assert_eq!(decoded.encode_octets().unwrap(), unknown);
        let mut partial = StatusFields {
            serial_numbers: Some(vec![1]),
            ..Default::default()
        };
        partial.update(&StatusFields {
            tasks: Some(vec![TrafficTask::Conversation]),
            ..Default::default()
        });
        assert_eq!(partial.serial_numbers, Some(vec![1]));
        assert_eq!(partial.tasks, Some(vec![TrafficTask::Conversation]));
        assert!(!partial.answers_identity_query());
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use crate::{
        l3::legacy_traffic::{self, TrafficMessage},
        link::traffic::{SignalingWord, TrafficLinkError, TrafficMessageReceiver},
    };
    #[test]
    fn a_complete_status_with_an_impossible_element_length_is_discarded_immediately() {
        let bits: Vec<_> = "001001011100010000010000010000000000000000000000"
            .chars()
            .map(|bit| bit == '1')
            .collect();
        assert_eq!(bits.len(), 48);
        assert_eq!(
            legacy_traffic::decode_prefix(&bits),
            Err(TrafficMessageError::Field("status_element_length"))
        );
        let mut receiver = TrafficMessageReceiver::default();
        assert_eq!(
            receiver.push(&SignalingWord {
                continuation: false,
                contents: bits.try_into().unwrap()
            }),
            Err(TrafficLinkError::Message(TrafficMessageError::Field(
                "status_element_length"
            )))
        );
        assert_eq!(
            receiver
                .push(&SignalingWord {
                    continuation: true,
                    contents: [false; 48]
                })
                .unwrap(),
            None
        );
        let query = TrafficMessage::Status(StatusFields::identity_query());
        let words = crate::link::traffic::encode(&query).unwrap();
        assert_eq!(receiver.push(&words[0]).unwrap(), Some(query));
        assert!(
            TrafficMessage::StatusRequest(StatusFields {
                serial_numbers: Some(vec![1]),
                ..Default::default()
            })
            .encode()
            .is_err()
        );
    }
}
