use super::{
    color::{decode_dvcc, encode_dvcc},
    dcch::{self, BlockError, BlockFormat, CrcMode, DecodedBlock},
    sync::{SYNC_BITS, SyncWord},
    traffic::SLOT_BITS,
};
use std::{error::Error, fmt};

pub const INFORMATION_BITS: usize = 109;
pub const LEADING_FEEDBACK_BITS: usize = 12;
pub const TRAILING_FEEDBACK_BITS: usize = 10;
const DATA_HALF_BITS: usize = 130;
const FEEDBACK_START: usize = SYNC_BITS;
const DATA_START: usize = FEEDBACK_START + LEADING_FEEDBACK_BITS;
const PHASE_START: usize = DATA_START + DATA_HALF_BITS;
const PHASE_BITS: usize = 12;
const FINAL_DATA_START: usize = PHASE_START + PHASE_BITS;
const FINAL_FEEDBACK_START: usize = FINAL_DATA_START + DATA_HALF_BITS;
const RESERVED_START: usize = FINAL_FEEDBACK_START + TRAILING_FEEDBACK_BITS;
const MAX_PHASE: u8 = 31;
const PARITY_COMPLEMENT: u16 = 0xf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalChannel {
    Fbcch,
    Ebcch,
    Sbcch,
    Reserved,
    Spach,
}
impl LogicalChannel {
    fn crc_mode(self) -> CrcMode {
        match self {
            Self::Spach => CrcMode::Addressed,
            _ => CrcMode::Broadcast,
        }
    }
    fn crc_dvcc(self, dvcc: u8) -> u8 {
        match self {
            Self::Fbcch => 0,
            _ => dvcc,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFeedback {
    pub leading: [bool; LEADING_FEEDBACK_BITS],
    pub trailing: [bool; TRAILING_FEEDBACK_BITS],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardControlFields {
    pub sync: SyncWord,
    pub superframe_phase: u8,
    pub corrected_phase_bits: u32,
    pub feedback: SharedFeedback,
    pub block: DecodedBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlError {
    Phase,
    Sync,
    Reserved,
    Block(BlockError),
}
impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Phase => f.write_str("invalid coded DCCH superframe phase"),
            Self::Sync => f.write_str("unrecognized DCCH synchronization word"),
            Self::Reserved => f.write_str("DCCH reserved bits are not both one"),
            Self::Block(e) => e.fmt(f),
        }
    }
}
impl Error for ControlError {}
impl From<BlockError> for ControlError {
    fn from(value: BlockError) -> Self {
        Self::Block(value)
    }
}

// IS-136.1-A Figure 4-4 and §§4.4.3, 4.4.7, 4.4.8.
pub fn encode_forward(
    sync: SyncWord,
    superframe_phase: u8,
    feedback: &SharedFeedback,
    information: &[bool; INFORMATION_BITS],
    channel: LogicalChannel,
    dvcc: u8,
) -> Result<[bool; SLOT_BITS], ControlError> {
    if superframe_phase > MAX_PHASE {
        return Err(ControlError::Phase);
    }
    let data = dcch::encode(
        BlockFormat::Forward,
        information,
        channel.crc_dvcc(dvcc),
        channel.crc_mode(),
    )?;
    let mut bits = [false; SLOT_BITS];
    bits[..SYNC_BITS].copy_from_slice(&sync.bits());
    bits[FEEDBACK_START..DATA_START].copy_from_slice(&feedback.leading);
    bits[DATA_START..PHASE_START].copy_from_slice(&data[..DATA_HALF_BITS]);
    let coded_phase = encode_dvcc(superframe_phase) ^ PARITY_COMPLEMENT;
    for (index, bit) in bits[PHASE_START..FINAL_DATA_START].iter_mut().enumerate() {
        *bit = coded_phase & (1 << (PHASE_BITS - index - 1)) != 0;
    }
    bits[FINAL_DATA_START..FINAL_FEEDBACK_START].copy_from_slice(&data[DATA_HALF_BITS..]);
    bits[FINAL_FEEDBACK_START..RESERVED_START].copy_from_slice(&feedback.trailing);
    bits[RESERVED_START..].fill(true);
    Ok(bits)
}

pub fn decode_forward(
    bits: &[bool; SLOT_BITS],
    channel: LogicalChannel,
    dvcc: u8,
) -> Result<ForwardControlFields, ControlError> {
    if bits[RESERVED_START..].iter().any(|bit| !bit) {
        return Err(ControlError::Reserved);
    }
    let mut sync = [false; SYNC_BITS];
    sync.copy_from_slice(&bits[..SYNC_BITS]);
    let sync = SyncWord::detect(&sync).ok_or(ControlError::Sync)?;
    let coded_phase = bits[PHASE_START..FINAL_DATA_START]
        .iter()
        .fold(0, |v, bit| (v << 1) | u16::from(*bit));
    let phase = decode_dvcc(coded_phase ^ PARITY_COMPLEMENT).ok_or(ControlError::Phase)?;
    if phase.dvcc > MAX_PHASE {
        return Err(ControlError::Phase);
    }
    let mut leading = [false; LEADING_FEEDBACK_BITS];
    leading.copy_from_slice(&bits[FEEDBACK_START..DATA_START]);
    let mut trailing = [false; TRAILING_FEEDBACK_BITS];
    trailing.copy_from_slice(&bits[FINAL_FEEDBACK_START..RESERVED_START]);
    let mut data = Vec::with_capacity(DATA_HALF_BITS * 2);
    data.extend_from_slice(&bits[DATA_START..PHASE_START]);
    data.extend_from_slice(&bits[FINAL_DATA_START..FINAL_FEEDBACK_START]);
    let block = dcch::decode(
        BlockFormat::Forward,
        &data,
        channel.crc_dvcc(dvcc),
        channel.crc_mode(),
    )?;
    Ok(ForwardControlFields {
        sync,
        superframe_phase: phase.dvcc,
        corrected_phase_bits: phase.corrected_bits,
        feedback: SharedFeedback { leading, trailing },
        block,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_control_field_positions_and_fbcch_color_rule() {
        let feedback = SharedFeedback {
            leading: [true; 12],
            trailing: [false; 10],
        };
        let information = [false; INFORMATION_BITS];
        let bits = encode_forward(
            SyncWord::One,
            0,
            &feedback,
            &information,
            LogicalChannel::Fbcch,
            27,
        )
        .unwrap();
        assert_eq!(&bits[28..40], &[true; 12]);
        let coded_phase = bits[170..182]
            .iter()
            .fold(0, |v, b| (v << 1) | u16::from(*b));
        assert_eq!(coded_phase, 0xf);
        assert_eq!(&bits[312..322], &[false; 10]);
        assert_eq!(&bits[322..324], &[true; 2]);
        let fields = decode_forward(&bits, LogicalChannel::Fbcch, 99).unwrap();
        assert_eq!(fields.superframe_phase, 0);
        assert_eq!(fields.block.information, information);
        assert_eq!(fields.feedback, feedback);
        assert_eq!(
            decode_forward(&bits, LogicalChannel::Spach, 0),
            Err(ControlError::Block(BlockError::Crc))
        );
    }

    #[test]
    fn superframe_phase_is_bounded_and_correctable() {
        let feedback = SharedFeedback {
            leading: [false; 12],
            trailing: [false; 10],
        };
        for phase in 0..=MAX_PHASE {
            let mut bits = encode_forward(
                SyncWord::Two,
                phase,
                &feedback,
                &[false; INFORMATION_BITS],
                LogicalChannel::Spach,
                1,
            )
            .unwrap();
            bits[171] = !bits[171];
            let fields = decode_forward(&bits, LogicalChannel::Spach, 1).unwrap();
            assert_eq!(fields.superframe_phase, phase);
            assert_eq!(fields.corrected_phase_bits, 1);
        }
        assert_eq!(
            encode_forward(
                SyncWord::One,
                32,
                &feedback,
                &[false; INFORMATION_BITS],
                LogicalChannel::Fbcch,
                0
            ),
            Err(ControlError::Phase)
        );
    }
}
