use super::{
    CallingName, CallingNumber, OCTET_BITS, PARAMETER_BITS, Reader, TrafficMessageError, flash,
};
const SIGNAL: u64 = 1;
const CALLING_NUMBER: u64 = 4;
const CALLING_NAME: u64 = 5;
const COUNT_BITS: usize = 6;
const MAX_CALLING_NUMBERS: usize = 2;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlertInformation {
    pub signals: Option<Vec<u8>>,
    pub calling_numbers: Vec<CallingNumber>,
    pub calling_name: Option<CallingName>,
    pub unparsed_bits: Vec<bool>,
}
impl AlertInformation {
    pub fn validate(&self) -> Result<(), TrafficMessageError> {
        self.encode_octets().map(|_| ())
    }
    // TIA-136-133-E §9.1.2.2.2 uses Signal/name parameter codes one/five without a Request Number.
    pub(super) fn encode_octets(&self) -> Result<Vec<u8>, TrafficMessageError> {
        let mut bits = Vec::new();
        if let Some(values) = &self.signals {
            for value in values {
                flash::validate_signal(*value)?;
            }
            flash::counted(&mut bits, SIGNAL, values, OCTET_BITS, 1)?;
        }
        if self.calling_numbers.len() > MAX_CALLING_NUMBERS {
            return Err(TrafficMessageError::Field("calling_number_count"));
        }
        for number in &self.calling_numbers {
            flash::encode_calling(&mut bits, number)?;
        }
        if let Some(name) = &self.calling_name {
            flash::encode_name(&mut bits, CALLING_NAME, name)?;
        }
        bits.extend(&self.unparsed_bits);
        flash::pad(&mut bits)?;
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
pub(super) fn decode(bytes: &[u8]) -> Result<AlertInformation, TrafficMessageError> {
    let bits: Vec<_> = bytes
        .iter()
        .flat_map(|byte| {
            (0..OCTET_BITS)
                .rev()
                .map(move |shift| byte & (1 << shift) != 0)
        })
        .collect();
    let mut reader = Reader {
        bits: &bits,
        offset: 0,
    };
    let mut information = AlertInformation::default();
    let mut previous = 0;
    while bits.len() - reader.offset >= OCTET_BITS {
        let start = reader.offset;
        let parameter = reader.take(PARAMETER_BITS)?;
        if !matches!(parameter, SIGNAL | CALLING_NUMBER | CALLING_NAME) {
            information.unparsed_bits = bits[start..].to_vec();
            reader.offset = bits.len();
            break;
        }
        if parameter < previous || (parameter == previous && parameter != CALLING_NUMBER) {
            return Err(TrafficMessageError::Field("alert_parameter_order"));
        }
        previous = parameter;
        let count = reader.take(COUNT_BITS)? as usize;
        match parameter {
            SIGNAL => {
                if count > 1 {
                    return Err(TrafficMessageError::Field("alert_signal_count"));
                }
                let values = (0..count)
                    .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
                    .collect::<Result<Vec<_>, _>>()?;
                for value in &values {
                    flash::validate_signal(*value)?;
                }
                information.signals = Some(values);
            }
            CALLING_NUMBER => {
                if information.calling_numbers.len() >= MAX_CALLING_NUMBERS {
                    return Err(TrafficMessageError::Field("calling_number_count"));
                }
                information
                    .calling_numbers
                    .push(flash::read_calling(&mut reader, count)?);
            }
            CALLING_NAME => information.calling_name = Some(flash::read_name(&mut reader, count)?),
            _ => unreachable!(),
        }
    }
    if bits[reader.offset..].iter().any(|bit| *bit) {
        return Err(TrafficMessageError::Field("alert_padding"));
    }
    Ok(information)
}
#[cfg(test)]
mod tests;
