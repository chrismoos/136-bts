use super::*;
fn delivery(vendor: Vendor) -> Delivery {
    Delivery {
        vendor,
        custom_control: vec![true, false, true],
        subaddress: None,
        ignored_bits: vec![],
        unparsed_bits: vec![],
    }
}

#[test]
fn independent_vendor_envelopes_use_direction_specific_headers_and_non_octet_payloads() {
    for (vendor, direction, wire) in [
        (
            Vendor::Soc(0xabc),
            Direction::Forward,
            vec![0x09, 0xab, 0xca],
        ),
        (
            Vendor::Soc(0xabc),
            Direction::Reverse,
            vec![0x0e, 0xab, 0xca],
        ),
        (
            Vendor::Bsmc(0x42),
            Direction::Forward,
            vec![0x02, 0x42, 0xa0],
        ),
        (
            Vendor::Bsmc(0x42),
            Direction::Reverse,
            vec![0x04, 0x42, 0xa0],
        ),
    ] {
        let message = delivery(vendor);
        assert_eq!(message.encode(direction).unwrap(), wire);
        assert_eq!(Delivery::decode(&wire, direction, 3).unwrap(), message);
        let prefix = Prefix::decode(&wire, direction).unwrap();
        assert_eq!(prefix.vendor, vendor);
        assert!(prefix.trailing_bits.starts_with(&message.custom_control));
        let opposite = if direction == Direction::Forward {
            Direction::Reverse
        } else {
            Direction::Forward
        };
        assert!(Prefix::decode(&wire, opposite).is_err());
    }
}

#[test]
fn vendor_payload_is_not_scanned_for_fake_subaddresses_or_stripped_padding() {
    let message = Delivery {
        custom_control: vec![
            false, false, false, true, false, false, false, false, false, false, false, true,
            false, false, false, false, false, false, false, false,
        ],
        ..delivery(Vendor::Soc(1))
    };
    let wire = message.encode(Direction::Reverse).unwrap();
    assert_eq!(
        Delivery::decode(&wire, Direction::Reverse, 20).unwrap(),
        message
    );
    let prefix = Prefix::decode(&[0x04, 0x42], Direction::Reverse).unwrap();
    assert!(prefix.trailing_bits.is_empty());
    assert!(Delivery::decode(&[0x04, 0x42], Direction::Reverse, 1).is_err());
    let prefix = Prefix::decode(&[0x0e, 0xab, 0xc0], Direction::Reverse).unwrap();
    assert_eq!(prefix.trailing_bits, vec![false; 4]);
}

#[test]
fn explicit_vendor_framing_locates_subaddress_and_keeps_unknown_optional_bits() {
    for bits in 1..=16 {
        let mut message = delivery(Vendor::Bsmc(255));
        message.custom_control = vec![true; bits];
        message.subaddress = Some(Subaddress {
            kind: 2,
            odd: true,
            address: vec![0x12, 0x34],
        });
        let wire = message.encode(Direction::Reverse).unwrap();
        assert_eq!(
            Delivery::decode(&wire, Direction::Reverse, bits).unwrap(),
            message
        );
        message.ignored_bits = vec![
            true, false, false, false, false, false, false, false, false, false, false, false,
        ];
        message.unparsed_bits = vec![false, true, false, false, true, false, true, false];
        let wire = message.encode(Direction::Forward).unwrap();
        let decoded = Delivery::decode(&wire, Direction::Forward, bits).unwrap();
        assert_eq!(decoded.encode(Direction::Forward).unwrap(), wire);
        assert_eq!(decoded.subaddress, message.subaddress);
    }
}

#[test]
fn vendor_width_payload_message_and_optional_bounds_are_enforced() {
    assert!(
        delivery(Vendor::Soc(4096))
            .encode(Direction::Forward)
            .is_err()
    );
    for (vendor, limit) in [(Vendor::Soc(4095), 2020), (Vendor::Bsmc(255), 2024)] {
        let mut message = delivery(vendor);
        message.custom_control = vec![true; limit];
        let wire = message.encode(Direction::Reverse).unwrap();
        assert_eq!(wire.len(), 255);
        assert_eq!(
            Delivery::decode(&wire, Direction::Reverse, limit).unwrap(),
            message
        );
        message.custom_control.push(true);
        assert!(message.encode(Direction::Reverse).is_err());
    }
    let mut message = delivery(Vendor::Soc(0));
    message.custom_control.clear();
    assert!(message.encode(Direction::Forward).is_err());
    assert!(Delivery::decode(&[9, 0, 0], Direction::Forward, 0).is_err());
    assert!(Delivery::decode(&[9, 0, 0], Direction::Forward, usize::MAX).is_err());
    for wire in [
        vec![],
        vec![9],
        vec![9, 0],
        vec![2],
        vec![0x49, 0, 0],
        vec![9; 256],
    ] {
        assert!(Prefix::decode(&wire, Direction::Forward).is_err());
    }
    message = delivery(Vendor::Bsmc(0));
    message.subaddress = Some(Subaddress {
        kind: 2,
        odd: false,
        address: vec![0; 21],
    });
    assert!(message.encode(Direction::Forward).is_err());
}

#[test]
fn vendor_envelopes_cannot_enter_registration_or_sms_procedures() {
    use crate::link::rach::{MessageSet, MobileIdentity};
    for vendor in [Vendor::Soc(1), Vendor::Bsmc(1)] {
        let bytes = delivery(vendor).encode(Direction::Reverse).unwrap();
        assert!(
            crate::runtime::access::classify(&MessageSet {
                identity: MobileIdentity::Min34(1),
                messages: vec![bytes]
            })
            .is_err()
        );
    }
}

#[test]
fn supplied_framing_rejects_duplicate_subaddresses_reserved_codes_and_truncated_options() {
    let mut message = delivery(Vendor::Bsmc(0x42));
    message.custom_control = vec![true];
    for ignored in [
        vec![true; 4],
        vec![false; 8],
        vec![false, true, true, true, false, false, false, false],
        vec![false, false, false, true],
    ] {
        message.ignored_bits = ignored;
        let wire = message.encode(Direction::Reverse).unwrap();
        assert!(
            Delivery::decode(&wire, Direction::Reverse, 1).is_err(),
            "{wire:02x?}"
        );
    }
    let address = Subaddress {
        kind: 0,
        odd: false,
        address: vec![],
    };
    message.subaddress = Some(address.clone());
    message.ignored_bits.clear();
    write_subaddress(&mut message.ignored_bits, 1, &address).unwrap();
    let wire = message.encode(Direction::Reverse).unwrap();
    assert!(Delivery::decode(&wire, Direction::Reverse, 1).is_err());
}
