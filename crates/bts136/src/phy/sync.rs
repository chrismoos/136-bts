use super::modulation::Dibit;

pub const SYNC_SYMBOLS: usize = 14;
pub const SYNC_BITS: usize = SYNC_SYMBOLS * 2;

// IS-136.2-A Table 1.2.4-2, phase differences in units of pi/4.
const PHASES: [[i8; SYNC_SYMBOLS]; 6] = [
    [-1, -1, -1, 3, 1, 3, -3, 3, -3, -1, 3, 1, -1, -1],
    [-1, -1, -1, 3, -3, 3, 1, 3, 1, -1, 3, -3, -1, -1],
    [-3, 1, 3, -3, -3, -1, 1, -3, -3, 1, 1, 1, -3, 1],
    [1, -3, 3, 1, 1, -1, -3, 1, 1, -3, -3, -3, 1, -3],
    [1, 3, 1, -3, -3, -1, 1, -1, 1, -3, -3, 3, 1, 3],
    [-3, 3, -3, 1, 1, -1, -3, -1, -3, 1, 1, 3, -3, 3],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncWord {
    One,
    Two,
    Three,
    Four,
    Five,
    Six,
}

impl SyncWord {
    pub const ALL: [Self; 6] = [
        Self::One,
        Self::Two,
        Self::Three,
        Self::Four,
        Self::Five,
        Self::Six,
    ];

    pub fn bits(self) -> [bool; SYNC_BITS] {
        let mut bits = [false; SYNC_BITS];
        for (index, phase) in PHASES[self as usize].iter().enumerate() {
            let dibit = match phase {
                1 => Dibit::ZeroZero,
                3 => Dibit::ZeroOne,
                -1 => Dibit::OneZero,
                -3 => Dibit::OneOne,
                _ => unreachable!("sync phases contain only the four defined differences"),
            };
            bits[index * 2..index * 2 + 2].copy_from_slice(&dibit.bits());
        }
        bits
    }

    pub fn detect(bits: &[bool; SYNC_BITS]) -> Option<Self> {
        Self::ALL.into_iter().find(|word| word.bits() == *bits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_one_matches_transcribed_wire_bits() {
        let expected = "1010100100011101111001001010";
        let actual: String = SyncWord::One
            .bits()
            .map(|b| if b { '1' } else { '0' })
            .into_iter()
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn sync_words_are_distinct() {
        for word in SyncWord::ALL {
            assert_eq!(SyncWord::detect(&word.bits()), Some(word));
        }
    }
}
