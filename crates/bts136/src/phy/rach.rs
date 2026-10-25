use super::{
    dcch::{self, BlockError, BlockFormat, CrcMode, DecodedBlock},
    sync::{SYNC_BITS, SyncWord},
    traffic::SLOT_BITS,
};
use std::{error::Error, fmt, ops::Range};

const GUARD_BITS: usize = 6;
const RAMP_BITS: usize = 6;
const PREAMBLE_START: usize = GUARD_BITS + RAMP_BITS;
const PREAMBLE_BITS: usize = 16;
const SYNC_START: usize = PREAMBLE_START + PREAMBLE_BITS;
const DATA_START: usize = SYNC_START + SYNC_BITS;
const FIRST_DATA_BITS: usize = 122;
const ADDITIONAL_SYNC_START: usize = DATA_START + FIRST_DATA_BITS;
const ADDITIONAL_SYNC_BITS: usize = 24;
const FINAL_DATA_START: usize = ADDITIONAL_SYNC_START + ADDITIONAL_SYNC_BITS;
const ABBREVIATED_GUARD_BITS: usize = 44;
const ADDITIONAL_SYNC: [bool; ADDITIONAL_SYNC_BITS] = [
    false, false, true, false, false, true, true, true, true, false, true, false, true, true,
    false, true, false, true, false, false, false, false, true, false,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstFormat {
    Normal,
    Abbreviated,
}
impl BurstFormat {
    fn block(self) -> BlockFormat {
        match self {
            Self::Normal => BlockFormat::ReverseNormal,
            Self::Abbreviated => BlockFormat::ReverseAbbreviated,
        }
    }
    fn data_end(self) -> usize {
        match self {
            Self::Normal => SLOT_BITS,
            Self::Abbreviated => SLOT_BITS - ABBREVIATED_GUARD_BITS,
        }
    }
    pub(crate) fn decision_symbols(self) -> Range<usize> {
        PREAMBLE_START / 2..self.data_end() / 2
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseControlFields {
    pub format: BurstFormat,
    pub sync: SyncWord,
    pub preamble_errors: u32,
    pub additional_sync_errors: u32,
    pub block: DecodedBlock,
}

impl ReverseControlFields {
    pub(crate) fn bit_errors(&self) -> super::bit_errors::BitErrors {
        super::bit_errors::BitErrors {
            corrected: u32::from(self.block.corrected_coded_bits),
            checked: self.format.block().encoded_bits() as u32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RachError {
    Sync,
    Block(BlockError),
}
impl fmt::Display for RachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sync => f.write_str("unrecognized RACH synchronization word"),
            Self::Block(error) => error.fmt(f),
        }
    }
}
impl Error for RachError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Block(error) => Some(error),
            Self::Sync => None,
        }
    }
}
impl From<BlockError> for RachError {
    fn from(error: BlockError) -> Self {
        Self::Block(error)
    }
}

// IS-136.1-A Figures 4-2/4-3 and §§4.4.3–4.4.10. Inactive positions contain no bits.
pub fn encode(
    format: BurstFormat,
    sync: SyncWord,
    information: &[bool],
    dvcc: u8,
) -> Result<[bool; SLOT_BITS], RachError> {
    let coded = dcch::encode(format.block(), information, dvcc, CrcMode::Addressed)?;
    let mut bits = [false; SLOT_BITS];
    for dibit in bits[PREAMBLE_START..SYNC_START].as_chunks_mut::<2>().0 {
        *dibit = [true, false];
    }
    bits[SYNC_START..DATA_START].copy_from_slice(&sync.bits());
    bits[DATA_START..ADDITIONAL_SYNC_START].copy_from_slice(&coded[..FIRST_DATA_BITS]);
    bits[ADDITIONAL_SYNC_START..FINAL_DATA_START].copy_from_slice(&ADDITIONAL_SYNC);
    bits[FINAL_DATA_START..format.data_end()].copy_from_slice(&coded[FIRST_DATA_BITS..]);
    Ok(bits)
}

pub(crate) fn decode_soft(
    bits: &[bool; SLOT_BITS],
    soft: &[f64; SLOT_BITS],
    format: BurstFormat,
    dvcc: u8,
) -> Result<ReverseControlFields, RachError> {
    decode_with(bits, format, || {
        let mut coded = Vec::with_capacity(format.block().encoded_bits());
        coded.extend_from_slice(&soft[DATA_START..ADDITIONAL_SYNC_START]);
        coded.extend_from_slice(&soft[FINAL_DATA_START..format.data_end()]);
        dcch::decode_soft(format.block(), &coded, dvcc, CrcMode::Addressed)
    })
}
pub fn decode(
    bits: &[bool; SLOT_BITS],
    format: BurstFormat,
    dvcc: u8,
) -> Result<ReverseControlFields, RachError> {
    decode_with(bits, format, || {
        let mut coded = Vec::with_capacity(format.block().encoded_bits());
        coded.extend_from_slice(&bits[DATA_START..ADDITIONAL_SYNC_START]);
        coded.extend_from_slice(&bits[FINAL_DATA_START..format.data_end()]);
        dcch::decode(format.block(), &coded, dvcc, CrcMode::Addressed)
    })
}
fn decode_with(
    bits: &[bool; SLOT_BITS],
    format: BurstFormat,
    decode_block: impl FnOnce() -> Result<DecodedBlock, BlockError>,
) -> Result<ReverseControlFields, RachError> {
    let sync_bits: [bool; SYNC_BITS] = bits[SYNC_START..DATA_START]
        .try_into()
        .expect("SYNC field has fixed length");
    let sync = SyncWord::detect(&sync_bits).ok_or(RachError::Sync)?;
    let block = decode_block()?;
    let preamble_errors = bits[PREAMBLE_START..SYNC_START]
        .iter()
        .enumerate()
        .filter(|(index, bit)| **bit != index.is_multiple_of(2))
        .count() as u32;
    let additional_sync_errors = bits[ADDITIONAL_SYNC_START..FINAL_DATA_START]
        .iter()
        .zip(ADDITIONAL_SYNC)
        .filter(|(a, b)| **a != *b)
        .count() as u32;
    Ok(ReverseControlFields {
        format,
        sync,
        preamble_errors,
        additional_sync_errors,
        block,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_and_abbreviated_layouts_match_standard_field_boundaries() {
        for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
            let information: Vec<_> = (0..format.block().information_bits())
                .map(|index| index.is_multiple_of(3))
                .collect();
            let bits = encode(format, SyncWord::Four, &information, 42).unwrap();
            assert_eq!(&bits[..12], &[false; 12]);
            assert_eq!(&bits[12..28], &[true, false].repeat(8));
            assert_eq!(&bits[28..56], &SyncWord::Four.bits());
            let additional: String = bits[178..202]
                .iter()
                .map(|b| if *b { '1' } else { '0' })
                .collect();
            assert_eq!(additional, "001001111010110101000010");
            let coded = dcch::encode(format.block(), &information, 42, CrcMode::Addressed).unwrap();
            assert_eq!(&bits[56..178], &coded[..122]);
            assert_eq!(&bits[202..format.data_end()], &coded[122..]);
            assert!(bits[format.data_end()..].iter().all(|b| !*b));
            let decoded = decode(&bits, format, 42).unwrap();
            assert_eq!(decoded.block.information, information);
            assert_eq!(decoded.preamble_errors, 0);
            assert_eq!(decoded.additional_sync_errors, 0);
            assert_eq!(
                decode(&bits, format, 43),
                Err(RachError::Block(BlockError::Crc))
            );
            let mut corrupt = bits;
            corrupt[12] ^= true;
            corrupt[178] ^= true;
            corrupt[65] ^= true;
            let decoded = decode(&corrupt, format, 42).unwrap();
            assert_eq!(decoded.block.information, information);
            assert_eq!(decoded.preamble_errors, 1);
            assert_eq!(decoded.additional_sync_errors, 1);
            assert_eq!(decoded.block.corrected_coded_bits, 1);
        }
    }
}
