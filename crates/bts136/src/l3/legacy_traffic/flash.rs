use super::{
    Direction, LENGTH_BITS, MAX_OPTIONAL_OCTETS, OCTET_BITS, PARAMETER_BITS, Reader,
    TrafficMessageError, put,
};
const NUMBER_TYPE_UNKNOWN: u8 = 0;
const ISDN_NUMBERING_PLAN: u8 = 1;
const NETWORK_SCREENING: u8 = 3;
const PRESENTATION_ALLOWED: u8 = 0;
const PRESENTATION_UNAVAILABLE: u8 = 2;
const FLASH: u8 = 0xf1;
const ACK: u8 = 0xa1;
const REQUEST_BITS: usize = 4;
const COUNT_BITS: usize = 6;
const FEATURE_BITS: usize = 4;
const TYPE_BITS: usize = 3;
const PLAN_BITS: usize = 4;
const INDICATOR_BITS: usize = 2;
const CALLING_NUMBER_RESERVED_BITS: usize = 5;
const CALLED_NUMBER_RESERVED_BITS: usize = 1;
const CALLING_NAME_RESERVED_BITS: usize = 4;
const MAX_NUMBER_CHARACTERS: usize = 15;
const MAX_NAME_CHARACTERS: usize = 60;
const MAX_KEYPAD_CHARACTERS: usize = 32;
const MAX_OTHER_MESSAGES: usize = 16;
const FEATURE: u64 = 2;
const KEYPAD: u64 = 3;
const CALLING_NUMBER: u64 = 4;
const CALLED_NUMBER: u64 = 5;
const VOICE_WAITING: u64 = 1;
const SIGNAL: u64 = 3;
const OTHER_WAITING: u64 = 5;
const CALLING_NAME: u64 = 6;
const LAST_PARAMETER: u64 = 1;
const PITCH_SHIFT: u32 = 6;
const MAX_PITCH: u8 = 2;
const CADENCE_MASK: u8 = 63;
const MAX_CADENCE: u8 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallingNumber {
    pub number_type: u8,
    pub numbering_plan: u8,
    pub presentation: u8,
    pub screening: u8,
    pub characters: String,
}
impl CallingNumber {
    pub fn network(number: &str) -> Result<Self, TrafficMessageError> {
        let value = Self {
            number_type: NUMBER_TYPE_UNKNOWN,
            numbering_plan: ISDN_NUMBERING_PLAN,
            presentation: PRESENTATION_ALLOWED,
            screening: NETWORK_SCREENING,
            characters: number.into(),
        };
        value.validate()?;
        Ok(value)
    }
    pub fn unavailable() -> Self {
        Self {
            number_type: NUMBER_TYPE_UNKNOWN,
            numbering_plan: NUMBER_TYPE_UNKNOWN,
            presentation: PRESENTATION_UNAVAILABLE,
            screening: NETWORK_SCREENING,
            characters: String::new(),
        }
    }
    fn validate(&self) -> Result<(), TrafficMessageError> {
        validate_number_header(self.number_type, self.numbering_plan)?;
        validate_indicators(self.presentation, self.screening)?;
        validate_text(&self.characters, MAX_NUMBER_CHARACTERS)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalledNumber {
    pub number_type: u8,
    pub numbering_plan: u8,
    pub characters: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallingName {
    pub presentation: u8,
    pub screening: u8,
    pub characters: String,
}
impl CallingName {
    pub fn network(name: &str) -> Result<Self, TrafficMessageError> {
        validate_text(name, MAX_NAME_CHARACTERS)?;
        Ok(Self {
            presentation: PRESENTATION_ALLOWED,
            screening: NETWORK_SCREENING,
            characters: name.into(),
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageWaiting {
    pub kind: u8,
    pub count: u8,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReverseFlashFields {
    pub feature: Option<Vec<u8>>,
    pub keypad: Option<String>,
    pub calling_number: Option<CallingNumber>,
    pub called_number: Option<CalledNumber>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallInformation {
    pub voice_messages: Option<Vec<u8>>,
    pub signals: Option<Vec<u8>>,
    pub calling_numbers: Vec<CallingNumber>,
    pub other_messages: Option<Vec<MessageWaiting>>,
    pub calling_name: Option<CallingName>,
    pub unparsed_bits: Vec<bool>,
}
impl CallInformation {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self) -> Result<(), TrafficMessageError> {
        self.encode_optional().map(|_| ())
    }
    pub fn last_parameter(&self) -> Option<u8> {
        if self.calling_name.is_some() {
            Some(CALLING_NAME as u8)
        } else if self.other_messages.is_some() {
            Some(OTHER_WAITING as u8)
        } else if !self.calling_numbers.is_empty() {
            Some(CALLING_NUMBER as u8)
        } else if self.signals.is_some() {
            Some(SIGNAL as u8)
        } else if self.voice_messages.is_some() {
            Some(VOICE_WAITING as u8)
        } else {
            None
        }
    }
    pub fn contains_parameter(&self, parameter: u8) -> bool {
        match u64::from(parameter) {
            VOICE_WAITING => self.voice_messages.is_some(),
            SIGNAL => self.signals.is_some(),
            CALLING_NUMBER => !self.calling_numbers.is_empty(),
            OTHER_WAITING => self.other_messages.is_some(),
            CALLING_NAME => self.calling_name.is_some(),
            _ => false,
        }
    }
    fn encode_optional(&self) -> Result<Vec<bool>, TrafficMessageError> {
        let mut bits = Vec::new();
        if let Some(values) = &self.voice_messages {
            counted(&mut bits, VOICE_WAITING, values, COUNT_BITS, 1)?;
        }
        if let Some(values) = &self.signals {
            for value in values {
                validate_signal(*value)?;
            }
            counted(&mut bits, SIGNAL, values, OCTET_BITS, 1)?;
        }
        if self.calling_numbers.len() > 2 {
            return Err(TrafficMessageError::Field("calling_number_count"));
        }
        for number in &self.calling_numbers {
            encode_calling(&mut bits, number)?;
        }
        if let Some(values) = &self.other_messages {
            if values.is_empty() || values.len() > MAX_OTHER_MESSAGES {
                return Err(TrafficMessageError::Field("other_messages_count"));
            }
            header(&mut bits, OTHER_WAITING, values.len());
            for value in values {
                checked(&mut bits, u64::from(value.kind), PARAMETER_BITS)?;
                checked(&mut bits, u64::from(value.count), COUNT_BITS)?;
            }
        }
        if let Some(name) = &self.calling_name {
            encode_name(&mut bits, CALLING_NAME, name)?;
        }
        bits.extend(&self.unparsed_bits);
        pad(&mut bits)?;
        Ok(bits)
    }
}
impl ReverseFlashFields {
    pub fn last_parameter(&self) -> Option<u8> {
        if self.called_number.is_some() {
            Some(CALLED_NUMBER as u8)
        } else if self.calling_number.is_some() {
            Some(CALLING_NUMBER as u8)
        } else if self.keypad.is_some() {
            Some(KEYPAD as u8)
        } else if self.feature.is_some() {
            Some(FEATURE as u8)
        } else {
            None
        }
    }
    fn encode_optional(&self) -> Result<Vec<bool>, TrafficMessageError> {
        let mut bits = Vec::new();
        if let Some(values) = &self.feature {
            counted(&mut bits, FEATURE, values, FEATURE_BITS, 1)?;
        }
        if let Some(keypad) = &self.keypad {
            validate_text(keypad, MAX_KEYPAD_CHARACTERS)?;
            header(&mut bits, KEYPAD, keypad.len());
            text(&mut bits, keypad);
        }
        if let Some(number) = &self.calling_number {
            encode_calling(&mut bits, number)?;
        }
        if let Some(number) = &self.called_number {
            validate_number_header(number.number_type, number.numbering_plan)?;
            validate_text(&number.characters, MAX_NUMBER_CHARACTERS)?;
            header(&mut bits, CALLED_NUMBER, 1 + number.characters.len());
            put(&mut bits, u64::from(number.number_type), TYPE_BITS);
            put(&mut bits, u64::from(number.numbering_plan), PLAN_BITS);
            put(&mut bits, 0, CALLED_NUMBER_RESERVED_BITS);
            text(&mut bits, &number.characters);
        }
        bits.extend(&self.unparsed_bits);
        pad(&mut bits)?;
        Ok(bits)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashAcknowledgment {
    pub request_number: u8,
    pub last_decoded_parameter: Option<u8>,
    pub unparsed_bits: Vec<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlashMessage {
    Forward {
        request_number: u8,
        information: CallInformation,
    },
    Reverse {
        request_number: u8,
        request: ReverseFlashFields,
    },
    Ack(FlashAcknowledgment),
}
impl FlashMessage {
    pub fn message_type(&self) -> u8 {
        if matches!(self, Self::Ack(_)) {
            ACK
        } else {
            FLASH
        }
    }
    pub fn request_number(&self) -> u8 {
        match self {
            Self::Forward { request_number, .. } | Self::Reverse { request_number, .. } => {
                *request_number
            }
            Self::Ack(ack) => ack.request_number,
        }
    }
    pub fn acknowledgment(&self) -> Option<Self> {
        let last = match self {
            Self::Forward { information, .. } => information.last_parameter(),
            Self::Reverse { request, .. } => request.last_parameter(),
            Self::Ack(_) => return None,
        };
        Some(Self::Ack(FlashAcknowledgment {
            request_number: self.request_number(),
            last_decoded_parameter: last,
            unparsed_bits: Vec::new(),
        }))
    }
    // TIA-136-133-E §§5.1.2.2.10,5.1.2.2.14,9.1.2.2.12,9.1.2.2.15.
    pub(super) fn encode_fields(&self, bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
        checked(bits, u64::from(self.request_number()), REQUEST_BITS)?;
        let optional = match self {
            Self::Forward { information, .. } => information.encode_optional()?,
            Self::Reverse { request, .. } => request.encode_optional()?,
            Self::Ack(ack) => {
                let mut optional = Vec::new();
                if let Some(last) = ack.last_decoded_parameter {
                    header(&mut optional, LAST_PARAMETER, 1);
                    checked(&mut optional, u64::from(last), PARAMETER_BITS)?;
                }
                optional.extend(&ack.unparsed_bits);
                pad(&mut optional)?;
                optional
            }
        };
        put(bits, (optional.len() / OCTET_BITS) as u64, LENGTH_BITS);
        bits.extend(optional);
        Ok(())
    }
}
// TIA-136-133-E §5.1.2.4.60 reserves pitch three and cadences thirteen through sixty-three.
pub(super) fn validate_signal(value: u8) -> Result<(), TrafficMessageError> {
    if value >> PITCH_SHIFT > MAX_PITCH || value & CADENCE_MASK > MAX_CADENCE {
        return Err(TrafficMessageError::Field("traffic_signal"));
    }
    Ok(())
}
fn header(bits: &mut Vec<bool>, parameter: u64, count: usize) {
    put(bits, parameter, PARAMETER_BITS);
    put(bits, count as u64, COUNT_BITS);
}
fn checked(bits: &mut Vec<bool>, value: u64, width: usize) -> Result<(), TrafficMessageError> {
    if value >= 1 << width {
        return Err(TrafficMessageError::Field("flash_field"));
    }
    put(bits, value, width);
    Ok(())
}
pub(super) fn counted(
    bits: &mut Vec<bool>,
    parameter: u64,
    values: &[u8],
    width: usize,
    maximum: usize,
) -> Result<(), TrafficMessageError> {
    if values.len() > maximum {
        return Err(TrafficMessageError::Field("flash_value_count"));
    }
    header(bits, parameter, values.len());
    for value in values {
        checked(bits, u64::from(*value), width)?;
    }
    Ok(())
}
fn validate_number_header(kind: u8, plan: u8) -> Result<(), TrafficMessageError> {
    if kind >= 1 << TYPE_BITS || plan >= 1 << PLAN_BITS {
        Err(TrafficMessageError::Field("calling_number_header"))
    } else {
        Ok(())
    }
}
fn validate_indicators(presentation: u8, screening: u8) -> Result<(), TrafficMessageError> {
    if presentation > 2 || screening >= 1 << INDICATOR_BITS {
        Err(TrafficMessageError::Field("presentation_screening"))
    } else {
        Ok(())
    }
}
fn validate_text(text: &str, maximum: usize) -> Result<(), TrafficMessageError> {
    if !text.is_ascii() || text.len() > maximum {
        Err(TrafficMessageError::Field("flash_characters"))
    } else {
        Ok(())
    }
}
fn text(bits: &mut Vec<bool>, text: &str) {
    for byte in text.bytes() {
        put(bits, u64::from(byte), OCTET_BITS);
    }
}
pub(super) fn pad(bits: &mut Vec<bool>) -> Result<(), TrafficMessageError> {
    bits.resize(bits.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
    if bits.len() / OCTET_BITS > MAX_OPTIONAL_OCTETS {
        Err(TrafficMessageError::OptionalLength)
    } else {
        Ok(())
    }
}
pub(super) fn encode_calling(
    bits: &mut Vec<bool>,
    number: &CallingNumber,
) -> Result<(), TrafficMessageError> {
    number.validate()?;
    header(bits, CALLING_NUMBER, 2 + number.characters.len());
    put(bits, u64::from(number.number_type), TYPE_BITS);
    put(bits, u64::from(number.numbering_plan), PLAN_BITS);
    put(bits, 0, CALLING_NUMBER_RESERVED_BITS);
    put(bits, u64::from(number.presentation), INDICATOR_BITS);
    put(bits, u64::from(number.screening), INDICATOR_BITS);
    text(bits, &number.characters);
    Ok(())
}
fn read_text(
    reader: &mut Reader<'_>,
    count: usize,
    maximum: usize,
) -> Result<String, TrafficMessageError> {
    if count > maximum {
        return Err(TrafficMessageError::Field("flash_characters"));
    }
    let bytes = (0..count)
        .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
        .collect::<Result<Vec<_>, _>>()?;
    if !bytes.is_ascii() {
        return Err(TrafficMessageError::Field("flash_characters"));
    }
    String::from_utf8(bytes).map_err(|_| TrafficMessageError::Field("flash_characters"))
}
pub(super) fn encode_name(
    bits: &mut Vec<bool>,
    parameter: u64,
    name: &CallingName,
) -> Result<(), TrafficMessageError> {
    validate_indicators(name.presentation, name.screening)?;
    validate_text(&name.characters, MAX_NAME_CHARACTERS)?;
    header(bits, parameter, 1 + name.characters.len());
    put(bits, 0, CALLING_NAME_RESERVED_BITS);
    put(bits, u64::from(name.presentation), INDICATOR_BITS);
    put(bits, u64::from(name.screening), INDICATOR_BITS);
    text(bits, &name.characters);
    Ok(())
}
pub(super) fn read_name(
    reader: &mut Reader<'_>,
    count: usize,
) -> Result<CallingName, TrafficMessageError> {
    if count < 1 {
        return Err(TrafficMessageError::Field("calling_name_length"));
    }
    reader.take(CALLING_NAME_RESERVED_BITS)?;
    let presentation = reader.take(INDICATOR_BITS)? as u8;
    let screening = reader.take(INDICATOR_BITS)? as u8;
    validate_indicators(presentation, screening)?;
    Ok(CallingName {
        presentation,
        screening,
        characters: read_text(reader, count - 1, MAX_NAME_CHARACTERS)?,
    })
}
pub(super) fn read_calling(
    reader: &mut Reader<'_>,
    count: usize,
) -> Result<CallingNumber, TrafficMessageError> {
    if count < 2 {
        return Err(TrafficMessageError::Field("calling_number_length"));
    }
    let number_type = reader.take(TYPE_BITS)? as u8;
    let numbering_plan = reader.take(PLAN_BITS)? as u8;
    // TIA-136-133-E §§5,5.1.2.3.17 requires ignoring received RSVD bits.
    reader.take(CALLING_NUMBER_RESERVED_BITS)?;
    let presentation = reader.take(INDICATOR_BITS)? as u8;
    let screening = reader.take(INDICATOR_BITS)? as u8;
    let characters = read_text(reader, count - 2, MAX_NUMBER_CHARACTERS)?;
    let number = CallingNumber {
        number_type,
        numbering_plan,
        presentation,
        screening,
        characters,
    };
    number.validate()?;
    Ok(number)
}

pub(super) fn decode(
    kind: u8,
    direction: Direction,
    reader: &mut Reader<'_>,
) -> Result<Option<FlashMessage>, TrafficMessageError> {
    if !matches!(kind, FLASH | ACK) {
        return Ok(None);
    }
    let request_number = reader.take(REQUEST_BITS)? as u8;
    let length = reader.take(LENGTH_BITS)? as usize * OCTET_BITS;
    let end = reader.offset + length;
    if reader.bits.len() < end {
        return Err(TrafficMessageError::Truncated { needed_bits: end });
    }
    let mut fields = Reader {
        bits: &reader.bits[reader.offset..end],
        offset: 0,
    };
    reader.offset = end;
    let mut reverse = ReverseFlashFields::default();
    let mut forward = CallInformation::default();
    let mut ack = FlashAcknowledgment {
        request_number,
        last_decoded_parameter: None,
        unparsed_bits: Vec::new(),
    };
    let parsed = (|| {
        let mut previous = 0;
        while fields.bits.len() - fields.offset >= OCTET_BITS {
            let start = fields.offset;
            let parameter = fields.take(PARAMETER_BITS)?;
            let known = if kind == ACK {
                parameter == LAST_PARAMETER
            } else if direction == Direction::Reverse {
                (FEATURE..=CALLED_NUMBER).contains(&parameter)
            } else {
                matches!(
                    parameter,
                    VOICE_WAITING | SIGNAL | CALLING_NUMBER | OTHER_WAITING | CALLING_NAME
                )
            };
            if !known {
                let tail = fields.bits[start..].to_vec();
                if kind == ACK {
                    ack.unparsed_bits = tail;
                } else if direction == Direction::Reverse {
                    reverse.unparsed_bits = tail;
                } else {
                    forward.unparsed_bits = tail;
                }
                fields.offset = fields.bits.len();
                break;
            }
            if parameter < previous
                || (parameter == previous
                    && !(kind == FLASH
                        && direction == Direction::Forward
                        && parameter == CALLING_NUMBER))
            {
                return Err(TrafficMessageError::Field("flash_parameter_order"));
            }
            previous = parameter;
            let count = fields.take(COUNT_BITS)? as usize;
            if kind == ACK {
                if count != 1 {
                    return Err(TrafficMessageError::Field("last_decoded_parameter_count"));
                }
                ack.last_decoded_parameter = Some(fields.take(PARAMETER_BITS)? as u8);
                continue;
            }
            if direction == Direction::Reverse {
                match parameter {
                    FEATURE => {
                        if count > 1 {
                            return Err(TrafficMessageError::Field("feature_count"));
                        }
                        reverse.feature = Some(
                            (0..count)
                                .map(|_| fields.take(FEATURE_BITS).map(|v| v as u8))
                                .collect::<Result<_, _>>()?,
                        );
                    }
                    KEYPAD => {
                        reverse.keypad = Some(read_text(&mut fields, count, MAX_KEYPAD_CHARACTERS)?)
                    }
                    CALLING_NUMBER => {
                        reverse.calling_number = Some(read_calling(&mut fields, count)?)
                    }
                    CALLED_NUMBER => {
                        if count < 1 {
                            return Err(TrafficMessageError::Field("called_number_length"));
                        }
                        let number_type = fields.take(TYPE_BITS)? as u8;
                        let numbering_plan = fields.take(PLAN_BITS)? as u8;
                        fields.take(CALLED_NUMBER_RESERVED_BITS)?;
                        reverse.called_number = Some(CalledNumber {
                            number_type,
                            numbering_plan,
                            characters: read_text(&mut fields, count - 1, MAX_NUMBER_CHARACTERS)?,
                        });
                    }
                    _ => unreachable!(),
                }
            } else {
                match parameter {
                    VOICE_WAITING | SIGNAL => {
                        if count > 1 {
                            return Err(TrafficMessageError::Field("flash_value_count"));
                        }
                        let width = if parameter == VOICE_WAITING {
                            COUNT_BITS
                        } else {
                            OCTET_BITS
                        };
                        let values = (0..count)
                            .map(|_| fields.take(width).map(|v| v as u8))
                            .collect::<Result<Vec<_>, _>>()?;
                        if parameter == VOICE_WAITING {
                            forward.voice_messages = Some(values)
                        } else {
                            for value in &values {
                                validate_signal(*value)?;
                            }
                            forward.signals = Some(values)
                        }
                    }
                    CALLING_NUMBER => {
                        if forward.calling_numbers.len() == 2 {
                            return Err(TrafficMessageError::Field("calling_number_count"));
                        }
                        forward
                            .calling_numbers
                            .push(read_calling(&mut fields, count)?);
                    }
                    OTHER_WAITING => {
                        if count == 0 || count > MAX_OTHER_MESSAGES {
                            return Err(TrafficMessageError::Field("other_messages_count"));
                        }
                        forward.other_messages = Some(
                            (0..count)
                                .map(|_| {
                                    Ok(MessageWaiting {
                                        kind: fields.take(PARAMETER_BITS)? as u8,
                                        count: fields.take(COUNT_BITS)? as u8,
                                    })
                                })
                                .collect::<Result<_, TrafficMessageError>>()?,
                        );
                    }
                    CALLING_NAME => forward.calling_name = Some(read_name(&mut fields, count)?),
                    _ => unreachable!(),
                }
            }
        }
        if fields.bits[fields.offset..].iter().any(|bit| *bit) {
            return Err(TrafficMessageError::Field("flash_padding"));
        }
        Ok(())
    })();
    parsed.map_err(|error| match error {
        TrafficMessageError::Truncated { .. } => {
            TrafficMessageError::Field("flash_optional_length")
        }
        other => other,
    })?;
    Ok(Some(if kind == ACK {
        FlashMessage::Ack(ack)
    } else if direction == Direction::Reverse {
        FlashMessage::Reverse {
            request_number,
            request: reverse,
        }
    } else {
        FlashMessage::Forward {
            request_number,
            information: forward,
        }
    }))
}
#[cfg(test)]
mod tests;
