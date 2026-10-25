use crate::l3::vendor_delivery::{Prefix, Vendor};
pub(super) fn name(prefix: &Prefix) -> &'static str {
    match prefix.vendor {
        Vendor::Soc(_) => "SOC Message Delivery",
        Vendor::Bsmc(_) => "BSMC Message Delivery",
    }
}
pub(super) fn details(prefix: &Prefix) -> String {
    let owner = match prefix.vendor {
        Vendor::Soc(code) => format!("SOC=0x{code:03X}"),
        Vendor::Bsmc(code) => format!("BSMC=0x{code:02X}"),
    };
    format!(
        "{owner}\nVendor framing required. Tail includes optional fields and octet padding{}",
        super::audit::tail(&prefix.trailing_bits).replacen("UnknownBits=", "VendorTailBits=", 1)
    )
}
#[cfg(test)]
mod tests {
    use crate::l3::{
        relay::Direction,
        vendor_delivery::{Delivery, Vendor},
    };
    #[test]
    fn vendor_logs_preserve_payload_ambiguity_and_direction_without_interpreting_commands() {
        for (direction, wire, name, owner) in [
            (
                "Forward",
                vec![0x09, 0xab, 0xca],
                "SOC Message Delivery",
                "SOC=0xABC",
            ),
            (
                "Reverse",
                vec![0x0e, 0xab, 0xca],
                "SOC Message Delivery",
                "SOC=0xABC",
            ),
            (
                "Forward",
                vec![0x02, 0x42, 0xa0],
                "BSMC Message Delivery",
                "BSMC=0x42",
            ),
            (
                "Reverse",
                vec![0x04, 0x42, 0xa0],
                "BSMC Message Delivery",
                "BSMC=0x42",
            ),
        ] {
            let (actual, text) = super::super::decode::control(direction, &wire);
            assert_eq!(actual, name);
            assert!(text.contains(owner));
            assert!(text.contains("Vendor framing required"));
            assert!(text.contains("octet padding"));
        }
        for wire in [vec![0x09, 0xab, 0xca], vec![0x02, 0x42, 0xa0]] {
            let (name, text) = super::super::decode::broadcast(&wire);
            assert!(name.ends_with("Message Delivery"));
            assert!(text.contains("VendorTailBits="));
        }
        let wire = Delivery {
            vendor: Vendor::Soc(4095),
            custom_control: vec![true; 2020],
            subaddress: None,
            ignored_bits: vec![],
            unparsed_bits: vec![],
        }
        .encode(Direction::Reverse)
        .unwrap();
        let (_, text) = super::super::decode::control("Reverse", &wire);
        assert!(text.contains("VendorTailBits=2020:"));
        assert!(text.len() < super::super::MAX_RECORD_BYTES);
    }
}
