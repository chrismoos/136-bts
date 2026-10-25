use super::broadcast::WireError;
const NEIGHBOR_CELL: u8 = 13;
const REGULATORY_CONFIGURATION: u8 = 15;
const SERV_SS_BITS: u32 = 4;
const SERV_SS_PADDING: u8 = 15;
const RCI_STANDARD: u8 = 1;
const RCI_SHIFT: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtendedBroadcast {
    NeighborCell { service_offering_offset_code: u8 },
    StandardRegulatoryConfiguration,
}
impl ExtendedBroadcast {
    // TIA-136-123-E §§5.1.2.1.1–2. A single cell has no optional neighbor lists.
    pub fn encode(self) -> Result<Vec<u8>, WireError> {
        match self {
            Self::NeighborCell {
                service_offering_offset_code,
            } if service_offering_offset_code < 1 << SERV_SS_BITS => Ok(vec![
                NEIGHBOR_CELL,
                service_offering_offset_code << SERV_SS_BITS,
            ]),
            Self::NeighborCell { .. } => Err(WireError::Field("serv_ss")),
            Self::StandardRegulatoryConfiguration => {
                Ok(vec![REGULATORY_CONFIGURATION, RCI_STANDARD << RCI_SHIFT])
            }
        }
    }
}
pub fn decode(bytes: &[u8]) -> Result<ExtendedBroadcast, WireError> {
    let [kind, field] = bytes else {
        return Err(WireError::Truncated);
    };
    match *kind {
        NEIGHBOR_CELL if field & SERV_SS_PADDING == 0 => Ok(ExtendedBroadcast::NeighborCell {
            service_offering_offset_code: field >> SERV_SS_BITS,
        }),
        REGULATORY_CONFIGURATION if *field == RCI_STANDARD << RCI_SHIFT => {
            Ok(ExtendedBroadcast::StandardRegulatoryConfiguration)
        }
        NEIGHBOR_CELL | REGULATORY_CONFIGURATION => Err(WireError::Field("extended_broadcast")),
        code => Err(WireError::MessageType(code)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standalone_neighbor_and_regulatory_messages_match_fixed_vectors() {
        for (message, wire) in [
            (
                ExtendedBroadcast::NeighborCell {
                    service_offering_offset_code: 0,
                },
                [0x0d, 0x00],
            ),
            (
                ExtendedBroadcast::StandardRegulatoryConfiguration,
                [0x0f, 0x40],
            ),
        ] {
            assert_eq!(message.encode().unwrap(), wire);
            assert_eq!(decode(&wire).unwrap(), message);
        }
        assert!(decode(&[0x0d, 0x01]).is_err());
        assert!(decode(&[0x0f, 0x80]).is_err());
        assert!(
            ExtendedBroadcast::NeighborCell {
                service_offering_offset_code: 16
            }
            .encode()
            .is_err()
        );
    }
}
