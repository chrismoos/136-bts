use crate::phy::{color::encode_locator, control::SharedFeedback};
use std::{error::Error, fmt};

const BRI_BITS: usize = 6;
const RECEIVED_BITS: usize = 5;
const CPE_BITS: usize = 11;
const BRI_BUSY: u8 = 0b111100;
const BRI_RESERVED: u8 = 0b001111;
const BRI_IDLE: u8 = 0;
const RECEIVED_CODE: u8 = 0b11111;
const NOT_RECEIVED_CODE: u8 = 0;
const IDLE_WINDOW_MASK: u8 = 0b1111;
const IDLE_SECOND_WINDOW_SHIFT: u32 = 2;
const IDLE_MAX_ONES: u32 = 1;
const RESERVED_MAX_ERRORS: u32 = 2;
const BUSY_MAX_ERRORS: u32 = 3;
const RECEIVED_MIN_ONES: u32 = 4;
const PARTIAL_ECHO_MAX_ERRORS: u32 = 2;
const BRI_POSITIONS: [usize; BRI_BITS] = [0, 4, 8, 12, 16, 20];
const RECEIVED_POSITIONS: [usize; RECEIVED_BITS] = [2, 6, 10, 14, 18];
const CPE_POSITIONS: [usize; CPE_BITS] = [1, 3, 5, 7, 9, 11, 13, 15, 17, 19, 21];
const LEADING_BITS: usize = 12;
const TOTAL_BITS: usize = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Busy,
    Reserved,
    Idle,
}
impl Availability {
    fn code(self) -> u8 {
        match self {
            Self::Busy => BRI_BUSY,
            Self::Reserved => BRI_RESERVED,
            Self::Idle => BRI_IDLE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessState {
    StartRandomAccess,
    StartReservedAccess,
    MoreBursts,
    AfterLastBurst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reception {
    Received,
    NotReceived,
}
impl Reception {
    fn code(self) -> u8 {
        match self {
            Self::Received => RECEIVED_CODE,
            Self::NotReceived => NOT_RECEIVED_CODE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPartialEcho;
impl fmt::Display for InvalidPartialEcho {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("partial echo exceeds seven bits")
    }
}
impl Error for InvalidPartialEcho {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessFeedback {
    pub availability: Availability,
    pub reception: Reception,
    pub partial_echo: u8,
}
impl AccessFeedback {
    // IS-136.1-A Figures 4-5–4-7 and Tables 5-8–5-10.
    pub fn encode(self) -> Result<SharedFeedback, InvalidPartialEcho> {
        let cpe = encode_locator(self.partial_echo).ok_or(InvalidPartialEcho)?;
        let mut bits = [false; TOTAL_BITS];
        write_field(
            &mut bits,
            &BRI_POSITIONS,
            u16::from(self.availability.code()),
        );
        write_field(
            &mut bits,
            &RECEIVED_POSITIONS,
            u16::from(self.reception.code()),
        );
        write_field(&mut bits, &CPE_POSITIONS, cpe);
        Ok(SharedFeedback {
            leading: bits[..LEADING_BITS]
                .try_into()
                .expect("leading feedback length"),
            trailing: bits[LEADING_BITS..]
                .try_into()
                .expect("trailing feedback length"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedFeedback {
    bri: u8,
    received: u8,
    coded_partial_echo: u16,
}
impl From<&SharedFeedback> for ObservedFeedback {
    fn from(value: &SharedFeedback) -> Self {
        let mut bits = [false; TOTAL_BITS];
        bits[..LEADING_BITS].copy_from_slice(&value.leading);
        bits[LEADING_BITS..].copy_from_slice(&value.trailing);
        Self {
            bri: read_field(&bits, &BRI_POSITIONS) as u8,
            received: read_field(&bits, &RECEIVED_POSITIONS) as u8,
            coded_partial_echo: read_field(&bits, &CPE_POSITIONS),
        }
    }
}
impl ObservedFeedback {
    pub fn bri_code(self) -> u8 {
        self.bri
    }
    pub fn reception_code(self) -> u8 {
        self.received
    }
    pub fn coded_partial_echo(self) -> u16 {
        self.coded_partial_echo
    }

    // IS-136.1-A Table 5-11. Idle uses overlapping four-bit windows.
    pub fn availability(self, state: AccessState) -> Option<Availability> {
        match state {
            AccessState::StartRandomAccess => {
                let first = (self.bri & IDLE_WINDOW_MASK).count_ones();
                let second =
                    ((self.bri >> IDLE_SECOND_WINDOW_SHIFT) & IDLE_WINDOW_MASK).count_ones();
                (first <= IDLE_MAX_ONES && second <= IDLE_MAX_ONES).then_some(Availability::Idle)
            }
            AccessState::StartReservedAccess => ((self.bri ^ BRI_RESERVED).count_ones()
                <= RESERVED_MAX_ERRORS)
                .then_some(Availability::Reserved),
            AccessState::MoreBursts | AccessState::AfterLastBurst => {
                ((self.bri ^ BRI_BUSY).count_ones() <= BUSY_MAX_ERRORS)
                    .then_some(Availability::Busy)
            }
        }
    }

    // IS-136.1-A Table 5-12 requires four of five received bits, not a majority.
    pub fn reception(self) -> Reception {
        if self.received.count_ones() >= RECEIVED_MIN_ONES {
            Reception::Received
        } else {
            Reception::NotReceived
        }
    }

    // IS-136.1-A §5.3.2.2.3 compares with the expected echo instead of decoding an ID.
    pub fn matches_partial_echo(self, expected: u8) -> Result<bool, InvalidPartialEcho> {
        let code = encode_locator(expected).ok_or(InvalidPartialEcho)?;
        Ok((self.coded_partial_echo ^ code).count_ones() <= PARTIAL_ECHO_MAX_ERRORS)
    }
}

fn write_field(bits: &mut [bool; TOTAL_BITS], positions: &[usize], value: u16) {
    for (index, position) in positions.iter().enumerate() {
        bits[*position] = value & (1 << (positions.len() - 1 - index)) != 0;
    }
}
fn read_field(bits: &[bool; TOTAL_BITS], positions: &[usize]) -> u16 {
    positions
        .iter()
        .fold(0, |value, position| value * 2 + u16::from(bits[*position]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_bit_positions_match_normative_wire_vector() {
        let fields = AccessFeedback {
            availability: Availability::Busy,
            reception: Reception::Received,
            partial_echo: 0,
        }
        .encode()
        .unwrap();
        let wire: String = fields
            .leading
            .into_iter()
            .chain(fields.trailing)
            .map(|bit| if bit { '1' } else { '0' })
            .collect();
        assert_eq!(wire, "1010101010101011011101");
        let observed = ObservedFeedback::from(&fields);
        assert_eq!(observed.bri_code(), BRI_BUSY);
        assert_eq!(observed.reception_code(), RECEIVED_CODE);
        assert_eq!(observed.coded_partial_echo(), 0xf);
        assert!(observed.matches_partial_echo(0).unwrap());
        assert!(
            AccessFeedback {
                availability: Availability::Idle,
                reception: Reception::NotReceived,
                partial_echo: 128
            }
            .encode()
            .is_err()
        );
    }

    #[test]
    fn state_specific_thresholds_match_all_received_patterns() {
        for bri in 0..1 << BRI_BITS {
            let observed = ObservedFeedback {
                bri,
                received: 0,
                coded_partial_echo: 0,
            };
            let bits: Vec<_> = (0..BRI_BITS)
                .map(|bit| u8::from(bri & (1 << bit) != 0))
                .collect();
            let idle = bits[..4].iter().sum::<u8>() < 2 && bits[2..].iter().sum::<u8>() < 2;
            assert_eq!(
                observed
                    .availability(AccessState::StartRandomAccess)
                    .is_some(),
                idle
            );
            for state in [AccessState::MoreBursts, AccessState::AfterLastBurst] {
                assert_eq!(
                    observed.availability(state).is_some(),
                    (bri ^ 0b111100).count_ones() < 4
                );
            }
            assert_eq!(
                observed
                    .availability(AccessState::StartReservedAccess)
                    .is_some(),
                (bri ^ 0b001111).count_ones() < 3
            );
        }
        for received in 0..1 << RECEIVED_BITS {
            let observed = ObservedFeedback {
                bri: 0,
                received,
                coded_partial_echo: 0,
            };
            assert_eq!(
                observed.reception() == Reception::Received,
                received.count_ones() >= 4
            );
        }
    }

    #[test]
    fn partial_echo_accepts_two_bit_errors_and_rejects_three() {
        for value in 0..128 {
            let code = encode_locator(value).unwrap();
            for mask in 0..1 << CPE_BITS {
                let observed = ObservedFeedback {
                    bri: 0,
                    received: 0,
                    coded_partial_echo: code ^ mask,
                };
                assert_eq!(
                    observed.matches_partial_echo(value).unwrap(),
                    mask.count_ones() < 3
                );
            }
        }
    }
}
