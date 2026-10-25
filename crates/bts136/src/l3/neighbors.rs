use super::broadcast::WireError;
use serde::Deserialize;
pub const MAX_NEIGHBORS: usize = 24;
const MESSAGE_TYPE: u32 = 13;
const TDMA_LIST_PARAMETER: u32 = 2;
const PUBLIC_NETWORK: u32 = 4;
const NEIGHBOR_BITS: usize = 57;
const HEADER_BITS: usize = 21;
const OCTET_BITS: usize = 8;
const FLAG_BITS: usize = 1;
const PARAMETER_BITS: usize = 4;
const COUNT_BITS: usize = 5;
const CHANNEL_BITS: usize = 11;
const VERSION_BITS: usize = 4;
const COLOR_BITS: usize = 8;
const OFFSET_BITS: usize = 7;
const SIGNAL_BITS: usize = 5;
const DELAY_BITS: usize = 4;
const CELL_TYPE_BITS: usize = 2;
const NETWORK_BITS: usize = 3;
const POWER_BITS: usize = 4;
const OFFSET_BIAS_DB: i16 = 128;
const OFFSET_STEP_DB: i16 = 2;
const MAX_OFFSET_DB: i16 = 126;
const MAX_CELL_TYPE: u8 = 2;
const DEFAULT_ACCESS_POWER: u8 = 7;
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Neighbor {
    pub channel: u16,
    pub dvcc: u8,
    #[serde(default = "default_protocol")]
    pub protocol_version_code: u8,
    #[serde(default)]
    pub reselect_offset_db: i16,
    #[serde(default)]
    pub sufficient_signal_code: u8,
    #[serde(default)]
    pub delay_code: u8,
    #[serde(default)]
    pub frequent_measurements: bool,
    #[serde(default)]
    pub synchronized: bool,
    #[serde(default)]
    pub cell_type_code: u8,
    #[serde(default = "default_access_power")]
    pub access_power_level: u8,
    #[serde(default)]
    pub minimum_signal_code: u8,
}
fn default_protocol() -> u8 {
    super::broadcast::ProtocolVersion::Pv2.code()
}
fn default_access_power() -> u8 {
    DEFAULT_ACCESS_POWER
}
fn put(bits: &mut Vec<bool>, value: u32, width: usize) -> Result<(), WireError> {
    if value >= 1 << width {
        return Err(WireError::Field("neighbor"));
    }
    bits.extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
    Ok(())
}
// TIA-136-123-E §§5.1.2.1.1, 6.147 and 6.150. Public same-hyperband neighbors omit the PSID/RSID map.
pub fn encode(neighbors: &[Neighbor]) -> Result<Vec<u8>, WireError> {
    if neighbors.is_empty() || neighbors.len() > MAX_NEIGHBORS {
        return Err(WireError::Field("neighbors"));
    }
    let mut bits = Vec::new();
    put(&mut bits, MESSAGE_TYPE, OCTET_BITS)?;
    put(&mut bits, 0, PARAMETER_BITS)?;
    put(&mut bits, TDMA_LIST_PARAMETER, PARAMETER_BITS)?;
    put(&mut bits, neighbors.len() as u32, COUNT_BITS)?;
    for neighbor in neighbors {
        super::broadcast::ProtocolVersion::decode(neighbor.protocol_version_code)?;
        if !(-OFFSET_BIAS_DB..=MAX_OFFSET_DB).contains(&neighbor.reselect_offset_db)
            || neighbor.reselect_offset_db % OFFSET_STEP_DB != 0
            || neighbor.cell_type_code > MAX_CELL_TYPE
            || neighbor.access_power_level > crate::l3::legacy_traffic::MAX_POWER_LEVEL
        {
            return Err(WireError::Field("neighbor"));
        }
        for (value, width) in [
            (u32::from(neighbor.channel), CHANNEL_BITS),
            (u32::from(neighbor.protocol_version_code), VERSION_BITS),
            (u32::from(neighbor.dvcc), COLOR_BITS),
            (
                ((i32::from(neighbor.reselect_offset_db) + i32::from(OFFSET_BIAS_DB))
                    / i32::from(OFFSET_STEP_DB)) as u32,
                OFFSET_BITS,
            ),
            (u32::from(neighbor.sufficient_signal_code), SIGNAL_BITS),
            (u32::from(neighbor.delay_code), DELAY_BITS),
            (u32::from(neighbor.frequent_measurements), FLAG_BITS),
            (u32::from(neighbor.synchronized), FLAG_BITS),
            (u32::from(neighbor.cell_type_code), CELL_TYPE_BITS),
            (PUBLIC_NETWORK, NETWORK_BITS),
            (0, FLAG_BITS),
            (u32::from(neighbor.access_power_level), POWER_BITS),
            (u32::from(neighbor.minimum_signal_code), SIGNAL_BITS),
            (0, FLAG_BITS),
        ] {
            put(&mut bits, value, width)?;
        }
    }
    Ok(bits
        .chunks(8)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .fold(0, |value, (bit, set)| value | (u8::from(*set) << (7 - bit)))
        })
        .collect())
}
pub fn decode(bytes: &[u8]) -> Result<Vec<Neighbor>, WireError> {
    let mut position = 0;
    let mut read = |width: usize| -> Result<u32, WireError> {
        if position + width > bytes.len() * 8 {
            return Err(WireError::Truncated);
        }
        let mut value = 0;
        for bit in position..position + width {
            value = (value << 1) | u32::from(bytes[bit / 8] & (1 << (7 - bit % 8)) != 0);
        }
        position += width;
        Ok(value)
    };
    if read(OCTET_BITS)? != MESSAGE_TYPE
        || read(PARAMETER_BITS)? != 0
        || read(PARAMETER_BITS)? != TDMA_LIST_PARAMETER
    {
        return Err(WireError::Field("neighbor_header"));
    }
    let count = read(COUNT_BITS)? as usize;
    if count == 0
        || count > MAX_NEIGHBORS
        || bytes.len() != (HEADER_BITS + count * NEIGHBOR_BITS).div_ceil(8)
    {
        return Err(WireError::Field("neighbor_count"));
    }
    let mut neighbors = Vec::new();
    for _ in 0..count {
        let channel = read(CHANNEL_BITS)? as u16;
        let version = read(VERSION_BITS)?;
        let dvcc = read(COLOR_BITS)? as u8;
        let reselect_offset_db = (read(OFFSET_BITS)? as i16) * OFFSET_STEP_DB - OFFSET_BIAS_DB;
        let sufficient_signal_code = read(SIGNAL_BITS)? as u8;
        let delay_code = read(DELAY_BITS)? as u8;
        let frequent_measurements = read(FLAG_BITS)? != 0;
        let synchronized = read(FLAG_BITS)? != 0;
        let cell_type_code = read(CELL_TYPE_BITS)? as u8;
        if read(NETWORK_BITS)? != PUBLIC_NETWORK
            || read(FLAG_BITS)? != 0
            || super::broadcast::ProtocolVersion::decode(version as u8).is_err()
        {
            return Err(WireError::Field("neighbor_service"));
        }
        let access_power_level = read(POWER_BITS)? as u8;
        let minimum_signal_code = read(SIGNAL_BITS)? as u8;
        if read(FLAG_BITS)? != 0 {
            return Err(WireError::Field("neighbor_psid"));
        }
        neighbors.push(Neighbor {
            protocol_version_code: version as u8,
            channel,
            dvcc,
            reselect_offset_db,
            sufficient_signal_code,
            delay_code,
            frequent_measurements,
            synchronized,
            cell_type_code,
            access_power_level,
            minimum_signal_code,
        });
    }
    let padding = bytes.len() * 8 - (HEADER_BITS + count * NEIGHBOR_BITS);
    if padding > 0 && read(padding)? != 0 {
        return Err(WireError::Field("neighbor_padding"));
    }
    if encode(&neighbors)?.as_slice() != bytes {
        return Err(WireError::Field("neighbor_canonical"));
    }
    Ok(neighbors)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn neighbor(channel: u16) -> Neighbor {
        serde_yaml_ng::from_str(&format!("channel: {channel}\ndvcc: 1")).unwrap()
    }
    #[test]
    fn complete_public_neighbor_list_obeys_wire_count_padding_and_bounds() {
        let single = neighbor(991);
        let bytes = encode(std::slice::from_ref(&single)).unwrap();
        assert_eq!(
            bytes,
            [0x0d, 0x02, 0x0b, 0xdf, 0x60, 0x18, 0x00, 0x00, 0x87, 0x00]
        );
        assert_eq!(decode(&bytes).unwrap(), [single]);
        let neighbors: Vec<_> = (1..=24).map(neighbor).collect();
        let bytes = encode(&neighbors).unwrap();
        assert_eq!(bytes.len(), 174);
        assert_eq!(decode(&bytes).unwrap(), neighbors);
        for length in 0..bytes.len() {
            assert!(decode(&bytes[..length]).is_err());
        }
        assert!(encode(&[]).is_err());
        assert!(encode(&vec![neighbor(1); 25]).is_err());
        let mut invalid = neighbor(1);
        invalid.reselect_offset_db = 128;
        assert!(encode(&[invalid]).is_err());
    }
}
