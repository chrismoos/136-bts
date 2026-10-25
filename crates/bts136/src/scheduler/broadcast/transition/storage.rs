use super::{AssignedSlots, BroadcastContent, Error, MandatoryBroadcast};
use std::{error::Error as StdError, fmt};

const STORAGE_VERSION: u8 = 1;
const STORAGE_WITH_TIME: u8 = 2;
const STORAGE_WITH_NEIGHBORS: u8 = 3;
const MANDATORY_PACKETS: usize = 5;

#[derive(Debug)]
pub enum BroadcastContentStorageError {
    Version(u8),
    Truncated,
    Format,
    NonCanonical,
    Content(Error),
}
impl fmt::Display for BroadcastContentStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "broadcast content recovery failed: {self:?}")
    }
}
impl StdError for BroadcastContentStorageError {}

impl BroadcastContent {
    // Version 2 adds a two-octet timezone policy after the three-octet header. Version 3 adds a length-prefixed neighbor packet after the timezone.
    pub fn storage_bytes(&self) -> Result<Vec<u8>, BroadcastContentStorageError> {
        let count =
            u8::try_from(self.optional.len()).map_err(|_| BroadcastContentStorageError::Format)?;
        let mut bytes = vec![
            if self.neighbor_packet.is_some() {
                STORAGE_WITH_NEIGHBORS
            } else if self.time_zone.is_some() {
                STORAGE_WITH_TIME
            } else {
                STORAGE_VERSION
            },
            self.pair as u8,
            count,
        ];
        if let Some(offset) = self.time_zone {
            bytes.extend_from_slice(&offset.bits().to_be_bytes());
        }
        if let Some(packet) = &self.neighbor_packet {
            if self.time_zone.is_none() {
                return Err(BroadcastContentStorageError::Format);
            }
            append(&mut bytes, packet)?;
        }
        for message in &self.messages {
            let packet = message
                .encode()
                .map_err(|error| BroadcastContentStorageError::Content(Error::Wire(error)))?;
            append(&mut bytes, &packet)?;
        }
        for packet in &self.optional {
            append(&mut bytes, packet)?;
        }
        Ok(bytes)
    }
    pub fn from_storage_bytes(bytes: &[u8]) -> Result<Self, BroadcastContentStorageError> {
        let mut reader = Reader { remaining: bytes };
        let version = reader.octet()?;
        if version != STORAGE_VERSION
            && version != STORAGE_WITH_TIME
            && version != STORAGE_WITH_NEIGHBORS
        {
            return Err(BroadcastContentStorageError::Version(version));
        }
        if reader.octet()? != AssignedSlots::Full14 as u8 {
            return Err(BroadcastContentStorageError::Format);
        }
        let count = usize::from(reader.octet()?);
        let time_zone = if version >= STORAGE_WITH_TIME {
            Some(
                crate::l3::time_date::TimeZoneOffset::from_bits(u16::from_be_bytes([
                    reader.octet()?,
                    reader.octet()?,
                ]))
                .map_err(|_| BroadcastContentStorageError::Format)?,
            )
        } else {
            None
        };
        let neighbors = if version == STORAGE_WITH_NEIGHBORS {
            Some(
                crate::l3::neighbors::decode(reader.packet()?)
                    .map_err(|_| BroadcastContentStorageError::Format)?,
            )
        } else {
            None
        };
        let mut mandatory = Vec::with_capacity(MANDATORY_PACKETS);
        for _ in 0..MANDATORY_PACKETS {
            let packet = reader.packet()?;
            mandatory.push(
                crate::l3::broadcast::decode_prefix(packet)
                    .map_err(|error| BroadcastContentStorageError::Content(Error::Wire(error)))?
                    .message,
            );
        }
        let optional = (0..count)
            .map(|_| reader.packet().map(<[u8]>::to_vec))
            .collect::<Result<Vec<_>, _>>()?;
        if !reader.remaining.is_empty() {
            return Err(BroadcastContentStorageError::Format);
        }
        let mandatory: [MandatoryBroadcast; MANDATORY_PACKETS] = mandatory
            .try_into()
            .map_err(|_| BroadcastContentStorageError::Format)?;
        let mut content = Self::try_new(AssignedSlots::Full14, mandatory, optional)
            .map_err(BroadcastContentStorageError::Content)?;
        if let Some(time_zone) = time_zone {
            content = content
                .with_time_zone(time_zone)
                .map_err(BroadcastContentStorageError::Content)?;
        }
        if let Some(neighbors) = neighbors {
            content = content
                .with_neighbors(&neighbors)
                .map_err(BroadcastContentStorageError::Content)?;
        }
        if content.storage_bytes()? != bytes {
            return Err(BroadcastContentStorageError::NonCanonical);
        }
        Ok(content)
    }
}
fn append(bytes: &mut Vec<u8>, packet: &[u8]) -> Result<(), BroadcastContentStorageError> {
    let length = u8::try_from(packet.len()).map_err(|_| BroadcastContentStorageError::Format)?;
    if length == 0 {
        return Err(BroadcastContentStorageError::Format);
    }
    bytes.push(length);
    bytes.extend_from_slice(packet);
    Ok(())
}
struct Reader<'a> {
    remaining: &'a [u8],
}
impl<'a> Reader<'a> {
    fn octet(&mut self) -> Result<u8, BroadcastContentStorageError> {
        let (octet, rest) = self
            .remaining
            .split_first()
            .ok_or(BroadcastContentStorageError::Truncated)?;
        self.remaining = rest;
        Ok(*octet)
    }
    fn packet(&mut self) -> Result<&'a [u8], BroadcastContentStorageError> {
        let length = usize::from(self.octet()?);
        if length == 0 {
            return Err(BroadcastContentStorageError::Format);
        }
        let packet = self
            .remaining
            .get(..length)
            .ok_or(BroadcastContentStorageError::Truncated)?;
        self.remaining = &self.remaining[length..];
        Ok(packet)
    }
}

#[cfg(test)]
mod tests;
