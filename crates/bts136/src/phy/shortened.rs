use super::{
    color::{self, DecodedDvcc},
    sync::{SYNC_BITS, SyncWord},
    traffic::SLOT_BITS,
};

// TIA/EIA-136-131-C §1.1 gives six S fields, five D fields and 2/4/6/8-symbol zero spacers.
pub const SYNC_OFFSETS: [usize; 6] = [6, 26, 48, 72, 98, 126];
pub const DVCC_OFFSETS: [usize; 5] = [20, 40, 62, 86, 112];
pub const ACTIVE_END_SYMBOL: usize = 140;
const DVCC_BITS: usize = 12;
const MIN_SYNC_WORDS: u8 = 2;
const MIN_DVCC_REPLICAS: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortenedBurst {
    pub sync: SyncWord,
    pub color: DecodedDvcc,
    pub sync_words: u8,
    pub dvcc_replicas: u8,
}

pub fn encode(sync: SyncWord, dvcc: u8) -> Option<[bool; SLOT_BITS]> {
    if dvcc == color::RESERVED_TRAFFIC_DVCC {
        return None;
    }
    let mut bits = [false; SLOT_BITS];
    for symbol in SYNC_OFFSETS {
        bits[symbol * 2..symbol * 2 + SYNC_BITS].copy_from_slice(&sync.bits());
    }
    let code = color::encode_dvcc(dvcc);
    for symbol in DVCC_OFFSETS {
        for bit in 0..DVCC_BITS {
            bits[symbol * 2 + bit] = code & (1 << (DVCC_BITS - 1 - bit)) != 0;
        }
    }
    Some(bits)
}

pub fn decode(bits: &[bool; SLOT_BITS]) -> Option<ShortenedBurst> {
    let mut result = None;
    for sync in SyncWord::ALL {
        let sync_words = SYNC_OFFSETS
            .into_iter()
            .filter(|symbol| bits[symbol * 2..symbol * 2 + SYNC_BITS] == sync.bits())
            .count() as u8;
        if sync_words < MIN_SYNC_WORDS {
            continue;
        }
        let mut votes = [0u8; 256];
        let mut corrected = [0u32; 256];
        for symbol in DVCC_OFFSETS {
            let code = bits[symbol * 2..symbol * 2 + DVCC_BITS]
                .iter()
                .fold(0, |value, bit| value * 2 + u16::from(*bit));
            let Some(color) = color::decode_dvcc(code) else {
                continue;
            };
            if color.dvcc == color::RESERVED_TRAFFIC_DVCC {
                continue;
            }
            votes[usize::from(color.dvcc)] += 1;
            corrected[usize::from(color.dvcc)] += color.corrected_bits;
        }
        let maximum = votes.iter().copied().max()?;
        if maximum < MIN_DVCC_REPLICAS {
            continue;
        }
        let mut winners = votes
            .iter()
            .enumerate()
            .filter(|(_, count)| **count == maximum);
        let (dvcc, _) = winners.next()?;
        if winners.next().is_some() || result.is_some() {
            return None;
        }
        result = Some(ShortenedBurst {
            sync,
            color: DecodedDvcc {
                dvcc: dvcc as u8,
                corrected_bits: corrected[dvcc],
            },
            sync_words,
            dvcc_replicas: maximum,
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortened_layout_has_unique_pair_spacings_and_fixed_wire_fields() {
        let bits = encode(SyncWord::One, 1).unwrap();
        let mut spacings = std::collections::BTreeSet::new();
        for (index, first) in SYNC_OFFSETS.into_iter().enumerate() {
            for second in &SYNC_OFFSETS[index + 1..] {
                assert!(spacings.insert(second - first));
            }
            let actual: String = bits[first * 2..first * 2 + SYNC_BITS]
                .iter()
                .map(|bit| if *bit { '1' } else { '0' })
                .collect();
            assert_eq!(actual, "1010100100011101111001001010");
        }
        for symbol in DVCC_OFFSETS {
            let actual: String = bits[symbol * 2..symbol * 2 + DVCC_BITS]
                .iter()
                .map(|bit| if *bit { '1' } else { '0' })
                .collect();
            assert_eq!(actual, "000000010011");
        }
        for range in [46..48, 68..72, 92..98, 118..126, 140..162] {
            assert!(bits[range.start * 2..range.end * 2].iter().all(|bit| !bit));
        }
        assert_eq!(decode(&bits).unwrap().sync_words, 6);
        assert_eq!(decode(&bits).unwrap().dvcc_replicas, 5);
    }
    #[test]
    fn any_pair_of_syncs_identifies_the_shortened_slot_and_one_does_not() {
        for sync in SyncWord::ALL {
            for (index, first) in SYNC_OFFSETS.into_iter().enumerate() {
                for second in &SYNC_OFFSETS[index + 1..] {
                    let mut bits = encode(sync, 42).unwrap();
                    for symbol in SYNC_OFFSETS {
                        if symbol != first && symbol != *second {
                            bits[symbol * 2..symbol * 2 + SYNC_BITS].fill(false);
                        }
                    }
                    let decoded = decode(&bits).unwrap();
                    assert_eq!(decoded.sync, sync);
                    assert_eq!(decoded.sync_words, 2);
                    bits[first * 2..first * 2 + SYNC_BITS].fill(false);
                    assert!(decode(&bits).is_none());
                }
            }
        }
    }
    #[test]
    fn shortened_dvcc_replicas_correct_single_errors_and_reject_ambiguous_identity() {
        let mut bits = encode(SyncWord::Two, 1).unwrap();
        for symbol in DVCC_OFFSETS {
            bits[symbol * 2] ^= true;
        }
        assert_eq!(
            decode(&bits).unwrap().color,
            DecodedDvcc {
                dvcc: 1,
                corrected_bits: 5
            }
        );
        let other = encode(SyncWord::Two, 2).unwrap();
        for symbol in &DVCC_OFFSETS[..2] {
            bits[symbol * 2..symbol * 2 + DVCC_BITS]
                .copy_from_slice(&other[symbol * 2..symbol * 2 + DVCC_BITS]);
        }
        bits[DVCC_OFFSETS[4] * 2..DVCC_OFFSETS[4] * 2 + DVCC_BITS].fill(false);
        assert!(decode(&bits).is_none());
        assert!(encode(SyncWord::One, 0).is_none());
    }
}
