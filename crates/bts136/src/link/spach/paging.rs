use super::*;

const NULL: u64 = 0;
const PENTA: u64 = 1;
const TRIPLE: u64 = 5;
const QUADRUPLE: u64 = 6;
const TRIPLE_COUNT: usize = 3;
const QUADRUPLE_COUNT: usize = 4;
const PENTA_COUNT: usize = 5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PagingFlags {
    pub pch_continuation: bool,
    pub bcch_change: bool,
    pub paging_frame_modifier: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageBody {
    Null { go_away: bool },
    TripleMin34([Option<u64>; TRIPLE_COUNT]),
    QuadrupleTmsi20([Option<u32>; QUADRUPLE_COUNT]),
    QuadrupleTmsi24([Option<u32>; QUADRUPLE_COUNT]),
    PentaTmsi20([Option<u32>; PENTA_COUNT]),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFrame {
    pub flags: PagingFlags,
    pub body: PageBody,
}
impl PageFrame {
    // IS-136.1-A Figures 5-26/27/28/29. These formats have no Header B.
    pub fn encode(&self) -> Result<[bool; INFORMATION_BITS], SpachError> {
        let usage = match self.body {
            PageBody::Null { .. } => NULL,
            PageBody::TripleMin34(_) => TRIPLE,
            PageBody::QuadrupleTmsi20(_) | PageBody::QuadrupleTmsi24(_) => QUADRUPLE,
            PageBody::PentaTmsi20(_) => PENTA,
        };
        let mut bits = Vec::new();
        put(&mut bits, usage, USAGE_BITS);
        for flag in [
            self.flags.pch_continuation,
            self.flags.bcch_change,
            self.flags.paging_frame_modifier,
            false,
        ] {
            put(&mut bits, u64::from(flag), FLAG_BITS);
        }
        match self.body {
            PageBody::Null { go_away } => put(&mut bits, u64::from(go_away), FLAG_BITS),
            PageBody::TripleMin34(slots) => identities(&mut bits, &slots, MIN_BITS)?,
            PageBody::QuadrupleTmsi20(slots) => {
                put(&mut bits, TMSI20_TYPE, IDENTITY_TYPE_BITS);
                identities(
                    &mut bits,
                    &slots.map(|slot| slot.map(u64::from)),
                    TMSI20_BITS,
                )?;
            }
            PageBody::QuadrupleTmsi24(slots) => {
                put(&mut bits, TMSI24_TYPE, IDENTITY_TYPE_BITS);
                identities(
                    &mut bits,
                    &slots.map(|slot| slot.map(u64::from)),
                    TMSI24_BITS,
                )?;
            }
            PageBody::PentaTmsi20(slots) => identities(
                &mut bits,
                &slots.map(|slot| slot.map(u64::from)),
                TMSI20_BITS,
            )?,
        }
        bits.resize(INFORMATION_BITS, false);
        Ok(bits
            .try_into()
            .expect("page frame padded to information length"))
    }
    pub fn addresses(&self, identity: MobileIdentity) -> bool {
        match (&self.body, identity) {
            (PageBody::TripleMin34(slots), MobileIdentity::Min34(value)) => {
                slots.contains(&Some(value))
            }
            (PageBody::QuadrupleTmsi20(slots), MobileIdentity::Tmsi20(value)) => {
                slots.contains(&Some(value))
            }
            (PageBody::PentaTmsi20(slots), MobileIdentity::Tmsi20(value)) => {
                slots.contains(&Some(value))
            }
            (PageBody::QuadrupleTmsi24(slots), MobileIdentity::Tmsi24(value)) => {
                slots.contains(&Some(value))
            }
            _ => false,
        }
    }
}
fn identities(bits: &mut Vec<bool>, slots: &[Option<u64>], width: usize) -> Result<(), SpachError> {
    for slot in slots {
        if slot.is_some_and(|value| value == 0 || value >= 1 << width) {
            return Err(SpachError::Identity);
        }
        // IS-136.1-A §5.2.5.1. Zero marks an unused identity position.
        put(bits, slot.unwrap_or(0), width);
    }
    Ok(())
}

/// Input must be one CRC-validated SPACH information block.
pub fn decode(bits: &[bool]) -> Result<PageFrame, SpachError> {
    if bits.len() != INFORMATION_BITS {
        return Err(SpachError::Length);
    }
    let mut reader = Reader { bits, offset: 0 };
    let usage = reader.take(USAGE_BITS)?;
    let flags = PagingFlags {
        pch_continuation: reader.take(FLAG_BITS)? != 0,
        bcch_change: reader.take(FLAG_BITS)? != 0,
        paging_frame_modifier: reader.take(FLAG_BITS)? != 0,
    };
    if reader.take(FLAG_BITS)? != 0 {
        return Err(SpachError::Reserved);
    }
    let body = match usage {
        NULL => PageBody::Null {
            go_away: reader.take(FLAG_BITS)? != 0,
        },
        TRIPLE => PageBody::TripleMin34(read_identities(&mut reader, MIN_BITS)?),
        QUADRUPLE => match reader.take(IDENTITY_TYPE_BITS)? {
            TMSI20_TYPE => PageBody::QuadrupleTmsi20(
                read_identities::<QUADRUPLE_COUNT>(&mut reader, TMSI20_BITS)?
                    .map(|slot| slot.map(|value| value as u32)),
            ),
            TMSI24_TYPE => PageBody::QuadrupleTmsi24(
                read_identities::<QUADRUPLE_COUNT>(&mut reader, TMSI24_BITS)?
                    .map(|slot| slot.map(|value| value as u32)),
            ),
            _ => return Err(SpachError::Identity),
        },
        PENTA => PageBody::PentaTmsi20(
            read_identities::<PENTA_COUNT>(&mut reader, TMSI20_BITS)?
                .map(|slot| slot.map(|value| value as u32)),
        ),
        _ => return Err(SpachError::UnsupportedUsage),
    };
    if bits[reader.offset..].iter().any(|bit| *bit) {
        return Err(SpachError::Filler);
    }
    Ok(PageFrame { flags, body })
}
fn read_identities<const N: usize>(
    reader: &mut Reader<'_>,
    width: usize,
) -> Result<[Option<u64>; N], SpachError> {
    let mut slots = [None; N];
    for slot in &mut slots {
        let value = reader.take(width)?;
        *slot = if value == 0 { None } else { Some(value) };
    }
    Ok(slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn header_identity_fields_and_filler_match_fixed_vectors() {
        let frame = PageFrame {
            flags: PagingFlags::default(),
            body: PageBody::TripleMin34([Some(1), None, Some(0x3ffffffff)]),
        };
        let bits = frame.encode().unwrap();
        assert_eq!(bits[..7], [true, false, true, false, false, false, false]);
        assert!(bits[7..40].iter().all(|bit| !bit));
        assert!(bits[40]);
        assert!(bits[41..75].iter().all(|bit| !bit));
        assert!(bits[75..].iter().all(|bit| *bit));
        assert_eq!(decode(&bits).unwrap(), frame);
        assert!(frame.addresses(MobileIdentity::Min34(1)));
        assert!(!frame.addresses(MobileIdentity::Tmsi20(1)));
        assert!(!frame.addresses(MobileIdentity::Min34(0)));
        let null = PageFrame {
            flags: PagingFlags::default(),
            body: PageBody::Null { go_away: true },
        }
        .encode()
        .unwrap();
        assert!(null[..7].iter().all(|bit| !bit));
        assert!(null[7]);
        assert!(null[8..].iter().all(|bit| !bit));
    }
    #[test]
    fn all_formats_round_trip_and_reject_reserved_bits_filler_and_overflow() {
        for body in [
            PageBody::Null { go_away: false },
            PageBody::QuadrupleTmsi20([Some(1), None, Some(0xfffff), None]),
            PageBody::QuadrupleTmsi24([Some(1), Some(0xffffff), None, None]),
            PageBody::PentaTmsi20([None, Some(1), Some(0xfffff), None, None]),
        ] {
            let frame = PageFrame {
                flags: PagingFlags {
                    pch_continuation: true,
                    bcch_change: true,
                    paging_frame_modifier: true,
                },
                body,
            };
            let bits = frame.encode().unwrap();
            assert_eq!(decode(&bits).unwrap(), frame);
            let mut malformed = bits;
            malformed[6] = true;
            assert_eq!(decode(&malformed), Err(SpachError::Reserved));
            malformed = bits;
            malformed[INFORMATION_BITS - 1] = true;
            assert_eq!(decode(&malformed), Err(SpachError::Filler));
        }
        for value in [0, 1 << TMSI20_BITS] {
            assert_eq!(
                PageFrame {
                    flags: PagingFlags::default(),
                    body: PageBody::PentaTmsi20([Some(value), None, None, None, None])
                }
                .encode(),
                Err(SpachError::Identity)
            );
        }
    }
}
