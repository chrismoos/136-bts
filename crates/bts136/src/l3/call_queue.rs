pub use super::control_fields::Error;
pub use super::maca::HyperbandChannels;
use super::maca::targets::{
    read_maca_channels as read_channels, write_maca_channels as write_channels,
};
use super::maca::{MeasurementTargets, validate_target_lists};
use super::{
    audit::{
        MULTILINGUAL_HEADER_OCTETS, MultilingualDisplay, display_octets, text,
        validate_multilingual,
    },
    broadcast::ProtocolVersion,
    control_fields::{
        self, OCTET_BITS, PARAMETER_BITS, Reader, pack, put, read_subaddress, write_subaddress,
    },
    legacy_traffic::{ControlChannel, Hyperband, Subaddress},
    relay::Direction,
};
const TYPE_BITS: usize = 6;
// TIA-136-123-E §6.128 assigns forward 42 only as a notification reference, not a Queue Disconnect message.
const DISCONNECT: u8 = 19;
const DISCONNECT_ACK: u8 = 43;
const UPDATE: u8 = 44;
const POSITION: u8 = 1;
const UPDATE_SUBADDRESS: u8 = 2;
const SUBADDRESS: u8 = 1;
const CURRENT_LIST: u8 = 3;
const OTHER_LIST: u8 = 4;
const DISPLAY: u8 = 8;
const MULTILINGUAL: u8 = 9;
const POSITION_BITS: usize = 4;
const HYPERBAND_BITS: usize = 2;
const ENCODING_BITS: usize = 5;
const RESERVED_BITS: usize = 3;
const MAX_QUEUE_POSITION: u8 = 15;
const RESERVED: u8 = 0;
const EXTENSION: u8 = 7;
const FUTURE: u8 = 15;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueUpdate {
    pub position: Option<u8>,
    pub current_channels: Option<Vec<u16>>,
    pub other_channels: Vec<HyperbandChannels>,
    pub display: Option<String>,
    pub multilingual_display: Option<MultilingualDisplay>,
}
impl QueueUpdate {
    fn validate(&self) -> Result<(), Error> {
        if self
            .position
            .is_some_and(|position| !(1..=MAX_QUEUE_POSITION).contains(&position))
        {
            return Err(Error::Field("queue_position"));
        }
        validate_target_lists(self.current_channels.as_deref(), &self.other_channels)
    }
    // TIA-136-123-E §§6.111–112 exclude the current DCCH and use other lists only for different hyperbands.
    pub fn validate_serving_channel(&self, serving: ControlChannel) -> Result<(), Error> {
        self.validate()?;
        MeasurementTargets::try_new(
            serving,
            self.current_channels.clone().unwrap_or_default(),
            self.other_channels.clone(),
        )?;
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueBody {
    Disconnect,
    DisconnectAck,
    Update(QueueUpdate),
}
impl QueueBody {
    fn header(&self) -> u8 {
        match self {
            Self::Disconnect => DISCONNECT,
            Self::DisconnectAck => DISCONNECT_ACK,
            Self::Update(_) => UPDATE,
        }
    }
    pub fn direction(&self) -> Direction {
        if matches!(self, Self::Disconnect) {
            Direction::Reverse
        } else {
            Direction::Forward
        }
    }
    fn subaddress_code(&self) -> u8 {
        if matches!(self, Self::Update(_)) {
            UPDATE_SUBADDRESS
        } else {
            SUBADDRESS
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueMessage {
    pub body: QueueBody,
    pub subaddress: Option<Subaddress>,
    pub ignored_bits: Vec<bool>,
    pub unparsed_bits: Vec<bool>,
}
impl QueueMessage {
    pub fn validate_version(&self, version: ProtocolVersion) -> Result<(), Error> {
        if version.code() < ProtocolVersion::Is136RevA.code()
            || matches!(&self.body,QueueBody::Update(update) if update.multilingual_display.is_some() && version.code()<ProtocolVersion::Pv1.code())
        {
            return Err(Error::Field("queue_protocol_version"));
        }
        self.encode().map(|_| ())
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bits = Vec::new();
        put(&mut bits, u64::from(self.body.header()), OCTET_BITS)?;
        if let QueueBody::Update(update) = &self.body {
            update.validate()?;
            if let Some(position) = update.position {
                put(&mut bits, u64::from(POSITION), PARAMETER_BITS)?;
                put(&mut bits, u64::from(position), POSITION_BITS)?;
            }
        }
        if let Some(address) = &self.subaddress {
            write_subaddress(&mut bits, self.body.subaddress_code(), address)?;
        }
        if let QueueBody::Update(update) = &self.body {
            if let Some(channels) = &update.current_channels {
                put(&mut bits, u64::from(CURRENT_LIST), PARAMETER_BITS)?;
                write_channels(&mut bits, channels)?;
            }
            for list in &update.other_channels {
                put(&mut bits, u64::from(OTHER_LIST), PARAMETER_BITS)?;
                put(&mut bits, list.hyperband as u64, HYPERBAND_BITS)?;
                write_channels(&mut bits, &list.channels)?;
            }
            if let Some(display) = &update.display {
                if !display.is_ascii() {
                    return Err(Error::Field("display_encoding"));
                }
                put(&mut bits, u64::from(DISPLAY), PARAMETER_BITS)?;
                text(&mut bits, display.as_bytes())?;
            }
            if let Some(display) = &update.multilingual_display {
                validate_multilingual(display)?;
                put(&mut bits, u64::from(MULTILINGUAL), PARAMETER_BITS)?;
                put(
                    &mut bits,
                    (display.bytes.len() + MULTILINGUAL_HEADER_OCTETS) as u64,
                    OCTET_BITS,
                )?;
                put(&mut bits, u64::from(display.encoding), ENCODING_BITS)?;
                put(&mut bits, 0, RESERVED_BITS)?;
                for byte in &display.bytes {
                    put(&mut bits, u64::from(*byte), OCTET_BITS)?;
                }
            }
        }
        bits.extend(&self.ignored_bits);
        bits.extend(&self.unparsed_bits);
        pack(bits)
    }
    pub fn decode(bytes: &[u8], direction: Direction) -> Result<Self, Error> {
        let bits = control_fields::bits(bytes)?;
        let mut reader = Reader {
            bits: &bits,
            offset: 0,
        };
        let header = reader.take(OCTET_BITS)? as u8;
        if header >> TYPE_BITS != 0 {
            return Err(Error::Protocol);
        }
        let body = match (header, direction) {
            (DISCONNECT, Direction::Reverse) => QueueBody::Disconnect,
            (DISCONNECT_ACK, Direction::Forward) => QueueBody::DisconnectAck,
            (UPDATE, Direction::Forward) => QueueBody::Update(Default::default()),
            _ => return Err(Error::MessageType),
        };
        let mut result = Self {
            body,
            subaddress: None,
            ignored_bits: vec![],
            unparsed_bits: vec![],
        };
        let mut previous = 0;
        while reader.offset < bits.len() {
            let remaining = &bits[reader.offset..];
            if remaining.len() < OCTET_BITS && remaining.iter().all(|bit| !*bit) {
                break;
            }
            let start = reader.offset;
            let code = reader.take(PARAMETER_BITS)? as u8;
            if matches!(code, RESERVED | FUTURE) {
                return Err(Error::Field("optional_reserved"));
            }
            if code == EXTENSION {
                let auxiliary = reader.take(PARAMETER_BITS)? as u8;
                if matches!(auxiliary, RESERVED | FUTURE) {
                    return Err(Error::Field("auxiliary_reserved"));
                }
            }
            let update = matches!(result.body, QueueBody::Update(_));
            let known = if update {
                matches!(
                    code,
                    POSITION
                        | UPDATE_SUBADDRESS
                        | CURRENT_LIST
                        | OTHER_LIST
                        | DISPLAY
                        | MULTILINGUAL
                )
            } else {
                code == SUBADDRESS
            };
            if !known {
                if reader.skip_noncritical(code)? {
                    result
                        .ignored_bits
                        .extend_from_slice(&bits[start..reader.offset]);
                    continue;
                }
                result.unparsed_bits = bits[start..].to_vec();
                break;
            }
            if code < previous || code == previous && !(update && code == OTHER_LIST) {
                return Err(Error::Field("parameter_order"));
            }
            previous = code;
            if code == result.body.subaddress_code() {
                result.subaddress = Some(read_subaddress(&mut reader)?);
                continue;
            }
            let QueueBody::Update(update) = &mut result.body else {
                return Err(Error::Field("queue_parameter"));
            };
            match code {
                POSITION => update.position = Some(reader.take(POSITION_BITS)? as u8),
                CURRENT_LIST => update.current_channels = Some(read_channels(&mut reader)?),
                OTHER_LIST => {
                    let hyperband = Hyperband::try_from(reader.take(HYPERBAND_BITS)? as u8)
                        .map_err(|_| Error::Field("hyperband"))?;
                    update.other_channels.push(HyperbandChannels {
                        hyperband,
                        channels: read_channels(&mut reader)?,
                    });
                }
                DISPLAY | MULTILINGUAL => {
                    let count = reader.take(OCTET_BITS)? as usize;
                    let encoding = if code == MULTILINGUAL {
                        let encoding = reader.take(ENCODING_BITS)? as u8;
                        if reader.take(RESERVED_BITS)? != 0 {
                            return Err(Error::Field("display_reserved"));
                        }
                        Some(encoding)
                    } else {
                        None
                    };
                    let count = display_octets(count, encoding)?;
                    let bytes = (0..count)
                        .map(|_| reader.take(OCTET_BITS).map(|value| value as u8))
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Some(encoding) = encoding {
                        let display = MultilingualDisplay { encoding, bytes };
                        validate_multilingual(&display)?;
                        update.multilingual_display = Some(display);
                    } else {
                        if !bytes.is_ascii() {
                            return Err(Error::Field("display_encoding"));
                        }
                        update.display = Some(
                            String::from_utf8(bytes)
                                .map_err(|_| Error::Field("display_encoding"))?,
                        );
                    }
                }
                _ => return Err(Error::Field("queue_parameter")),
            }
        }
        if let QueueBody::Update(update) = &result.body {
            update.validate()?;
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests;
