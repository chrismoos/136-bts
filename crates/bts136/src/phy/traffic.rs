use super::{
    color::{DecodedDvcc, decode_dvcc, encode_dvcc, encode_locator},
    sync::{SYNC_BITS, SyncWord},
};
use std::{error::Error, fmt};

pub const SLOT_BITS: usize = 324;
pub const SACCH_BITS: usize = 12;
pub const DATA_BITS: usize = 260;
const FORWARD_DATA_HALF_BITS: usize = DATA_BITS / 2;
const REVERSE_LEADING_DATA_BITS: usize = 16;
const REVERSE_DATA_BLOCK_BITS: usize = 122;
const REVERSE_SYNC_START: usize = 28;
const REVERSE_DATA_START: usize = REVERSE_SYNC_START + SYNC_BITS;
const REVERSE_SACCH_START: usize = REVERSE_DATA_START + REVERSE_DATA_BLOCK_BITS;
const REVERSE_DVCC_START: usize = REVERSE_SACCH_START + SACCH_BITS;
const REVERSE_FINAL_DATA_START: usize = REVERSE_DVCC_START + SACCH_BITS;
const FORWARD_SACCH_START: usize = SYNC_BITS;
const FORWARD_DATA_START: usize = FORWARD_SACCH_START + SACCH_BITS;
const FORWARD_DVCC_START: usize = FORWARD_DATA_START + FORWARD_DATA_HALF_BITS;
const FORWARD_FINAL_DATA_START: usize = FORWARD_DVCC_START + SACCH_BITS;
const RESERVED_POSITION: usize = FORWARD_FINAL_DATA_START + FORWARD_DATA_HALF_BITS;
const LOCATOR_START: usize = RESERVED_POSITION + 1;
const LOCATOR_BITS: usize = 11;
const REVERSE_LEADING_DATA_START: usize = REVERSE_SYNC_START - REVERSE_LEADING_DATA_BITS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotError {
    Sync,
    Dvcc,
    Locator,
    Reserved,
}

impl fmt::Display for SlotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sync => "unrecognized traffic synchronization word",
            Self::Dvcc => "uncorrectable digital verification color code",
            Self::Locator => "digital control channel locator exceeds seven bits",
            Self::Reserved => "forward traffic reserved bit is not one",
        })
    }
}
impl Error for SlotError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrafficFields {
    pub sync: SyncWord,
    pub sacch: [bool; SACCH_BITS],
    pub data: [bool; DATA_BITS],
    pub color: DecodedDvcc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardTraffic {
    pub fields: TrafficFields,
    pub coded_locator: u16,
}

// IS-136.2-A Figure 1.2-2, forward DTC only. DCCH has different feedback fields.
pub fn encode_forward(
    sync: SyncWord,
    sacch: &[bool; SACCH_BITS],
    data: &[bool; DATA_BITS],
    dvcc: u8,
    locator: u8,
) -> Result<[bool; SLOT_BITS], SlotError> {
    let locator = encode_locator(locator).ok_or(SlotError::Locator)?;
    let mut bits = [false; SLOT_BITS];
    bits[..SYNC_BITS].copy_from_slice(&sync.bits());
    bits[FORWARD_SACCH_START..FORWARD_DATA_START].copy_from_slice(sacch);
    bits[FORWARD_DATA_START..FORWARD_DVCC_START].copy_from_slice(&data[..FORWARD_DATA_HALF_BITS]);
    write_word(
        &mut bits[FORWARD_DVCC_START..FORWARD_FINAL_DATA_START],
        encode_dvcc(dvcc),
    );
    bits[FORWARD_FINAL_DATA_START..RESERVED_POSITION]
        .copy_from_slice(&data[FORWARD_DATA_HALF_BITS..]);
    bits[RESERVED_POSITION] = true;
    write_word(
        &mut bits[LOCATOR_START..LOCATOR_START + LOCATOR_BITS],
        locator,
    );
    Ok(bits)
}

pub fn decode_forward(bits: &[bool; SLOT_BITS]) -> Result<ForwardTraffic, SlotError> {
    if !bits[RESERVED_POSITION] {
        return Err(SlotError::Reserved);
    }
    let sync = sync_from_slice(&bits[..SYNC_BITS])?;
    let color = decode_dvcc(read_word(
        &bits[FORWARD_DVCC_START..FORWARD_FINAL_DATA_START],
    ))
    .ok_or(SlotError::Dvcc)?;
    let mut data = [false; DATA_BITS];
    data[..FORWARD_DATA_HALF_BITS].copy_from_slice(&bits[FORWARD_DATA_START..FORWARD_DVCC_START]);
    data[FORWARD_DATA_HALF_BITS..]
        .copy_from_slice(&bits[FORWARD_FINAL_DATA_START..RESERVED_POSITION]);
    let mut sacch = [false; SACCH_BITS];
    sacch.copy_from_slice(&bits[FORWARD_SACCH_START..FORWARD_DATA_START]);
    Ok(ForwardTraffic {
        fields: TrafficFields {
            sync,
            sacch,
            data,
            color,
        },
        coded_locator: read_word(&bits[LOCATOR_START..LOCATOR_START + LOCATOR_BITS]),
    })
}

// IS-136.2-A Figure 1.2-2, normal reverse DTC burst after hard decisions.
pub fn decode_reverse(bits: &[bool; SLOT_BITS]) -> Result<TrafficFields, SlotError> {
    let sync = sync_from_slice(&bits[REVERSE_SYNC_START..REVERSE_DATA_START])?;
    let color = decode_dvcc(read_word(
        &bits[REVERSE_DVCC_START..REVERSE_FINAL_DATA_START],
    ))
    .ok_or(SlotError::Dvcc)?;
    let mut data = [false; DATA_BITS];
    data[..REVERSE_LEADING_DATA_BITS]
        .copy_from_slice(&bits[REVERSE_LEADING_DATA_START..REVERSE_SYNC_START]);
    let data_boundary = REVERSE_LEADING_DATA_BITS + REVERSE_DATA_BLOCK_BITS;
    data[REVERSE_LEADING_DATA_BITS..data_boundary]
        .copy_from_slice(&bits[REVERSE_DATA_START..REVERSE_SACCH_START]);
    data[data_boundary..].copy_from_slice(&bits[REVERSE_FINAL_DATA_START..]);
    let mut sacch = [false; SACCH_BITS];
    sacch.copy_from_slice(&bits[REVERSE_SACCH_START..REVERSE_DVCC_START]);
    Ok(TrafficFields {
        sync,
        sacch,
        data,
        color,
    })
}

fn sync_from_slice(bits: &[bool]) -> Result<SyncWord, SlotError> {
    let mut sync = [false; SYNC_BITS];
    sync.copy_from_slice(bits);
    SyncWord::detect(&sync).ok_or(SlotError::Sync)
}

fn read_word(bits: &[bool]) -> u16 {
    bits.iter()
        .fold(0, |word, bit| (word << 1) | u16::from(*bit))
}
fn write_word(bits: &mut [bool], word: u16) {
    let length = bits.len();
    for (index, bit) in bits.iter_mut().enumerate() {
        *bit = word & (1 << (length - index - 1)) != 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_wire_field_boundaries() {
        let mut data = [false; DATA_BITS];
        for index in [0, 129, 130, 259] {
            data[index] = true;
        }
        let mut sacch = [false; SACCH_BITS];
        sacch[0] = true;
        sacch[11] = true;
        let bits = encode_forward(SyncWord::One, &sacch, &data, 1, 1).unwrap();
        assert!(bits[28]);
        assert!(bits[39]);
        for position in [40, 169, 182, 311, 312] {
            assert!(bits[position]);
        }
        assert_eq!(read_word(&bits[170..182]), 0x013);
        assert_eq!(read_word(&bits[313..324]), 0x01c);
        let decoded = decode_forward(&bits).unwrap();
        assert_eq!(decoded.fields.data, data);
        assert_eq!(decoded.fields.sacch, sacch);
        assert_eq!(decoded.fields.color.dvcc, 1);
        assert_eq!(
            LOCATOR_BITS,
            bits[LOCATOR_START..LOCATOR_START + LOCATOR_BITS].len()
        );
    }

    #[test]
    fn reverse_wire_field_boundaries() {
        let mut bits = [false; SLOT_BITS];
        bits[28..56].copy_from_slice(&SyncWord::Two.bits());
        for position in [12, 27, 56, 177, 202, 323] {
            bits[position] = true;
        }
        bits[178] = true;
        bits[189] = true;
        write_word(&mut bits[190..202], 0x026);
        let decoded = decode_reverse(&bits).unwrap();
        assert_eq!(decoded.sync, SyncWord::Two);
        assert_eq!(decoded.color.dvcc, 2);
        for index in [0, 15, 16, 137, 138, 259] {
            assert!(decoded.data[index]);
        }
        assert_eq!(decoded.data.iter().filter(|b| **b).count(), 6);
        assert!(decoded.sacch[0]);
        assert!(decoded.sacch[11]);
    }

    #[test]
    fn malformed_forward_slots_are_rejected() {
        let mut bits = encode_forward(
            SyncWord::One,
            &[false; SACCH_BITS],
            &[false; DATA_BITS],
            0,
            0,
        )
        .unwrap();
        bits[RESERVED_POSITION] = false;
        assert_eq!(decode_forward(&bits), Err(SlotError::Reserved));
        bits[RESERVED_POSITION] = true;
        bits[..SYNC_BITS].fill(false);
        assert_eq!(decode_forward(&bits), Err(SlotError::Sync));
    }
}
