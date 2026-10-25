use super::broadcast::WireError;
const MESSAGE_TYPE: u8 = 10;
const MAX_SOC: u16 = 0xfff;
const PACKET_OCTETS: usize = 4;
const PADDING_MASK: u8 = 0xf;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperatorIdentity {
    pub soc: u16,
    pub bsmc: u8,
}
impl OperatorIdentity {
    // TIA-136-123-E §§5.1.1.2.7, 6.18 and 6.225. Zero codes explicitly mean unknown.
    pub fn encode(self) -> Result<[u8; PACKET_OCTETS], WireError> {
        if self.soc > MAX_SOC {
            return Err(WireError::Field("soc"));
        }
        Ok([
            MESSAGE_TYPE,
            (self.soc >> 4) as u8,
            ((self.soc << 4) as u8) | (self.bsmc >> 4),
            self.bsmc << 4,
        ])
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let [kind, high, mid, low] = *bytes else {
            return Err(WireError::Truncated);
        };
        if kind != MESSAGE_TYPE {
            return Err(WireError::MessageType(kind));
        }
        if low & PADDING_MASK != 0 {
            return Err(WireError::Field("operator_padding"));
        }
        Ok(Self {
            soc: (u16::from(high) << 4) | u16::from(mid >> 4),
            bsmc: (mid << 4) | (low >> 4),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operator_vector_preserves_unaligned_codes_and_rejects_bad_wire() {
        let message = OperatorIdentity {
            soc: 0xabc,
            bsmc: 0x12,
        };
        assert_eq!(message.encode().unwrap(), [10, 0xab, 0xc1, 0x20]);
        assert_eq!(
            OperatorIdentity::decode(&[10, 0xab, 0xc1, 0x20]).unwrap(),
            message
        );
        assert!(OperatorIdentity::decode(&[10, 0xab, 0xc1, 0x21]).is_err());
        assert!(OperatorIdentity { soc: 4096, bsmc: 0 }.encode().is_err());
        assert_eq!(
            OperatorIdentity { soc: 0, bsmc: 0 }.encode().unwrap(),
            [10, 0, 0, 0]
        );
    }
}
