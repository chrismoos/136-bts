use super::*;

fn report(bands: u8) -> CapabilityReport {
    let mut report = super::super::tests::report();
    report.supported_bands = bands;
    report
}

#[test]
fn every_pcs_block_includes_its_border_channels_without_enabling_other_blocks() {
    let blocks = [
        (0x02, 2..=501),
        (0x04, 666..=1167),
        (0x08, 1499..=1998),
        (0x10, 499..=667),
        (0x20, 1166..=1334),
        (0x40, 1333..=1501),
    ];
    for (mask, channels) in blocks {
        let report = report(mask);
        for channel in 0..=2048 {
            assert_eq!(
                report.supports_dtc_channel(Hyperband::Pcs1900, channel),
                channels.contains(&channel),
                "mask={mask:#04x} channel={channel}"
            );
        }
        assert!(!report.supports_dtc_channel(Hyperband::Cellular800, 1));
        assert!(!report.supports_dtc_channel(Hyperband::Cellular700, 1));
        assert!(!report.supports_dtc_channel(Hyperband::Imt2000, 2));
    }
}

#[test]
fn cellular_flags_keep_unused_channels_and_other_hyperbands_excluded() {
    for (mask, band, ranges) in [
        (
            0x01,
            Hyperband::Cellular800,
            [Some(1..=799), Some(991..=1023)],
        ),
        (0x80, Hyperband::Cellular700, [Some(1..=499), None]),
    ] {
        let report = report(mask);
        for channel in 0..=2048 {
            assert_eq!(
                report.supports_dtc_channel(band, channel),
                ranges
                    .iter()
                    .flatten()
                    .any(|range| range.contains(&channel)),
                "mask={mask:#04x} channel={channel}"
            );
        }
        assert!(!report.supports_dtc_channel(Hyperband::Pcs1900, 2));
        assert!(!report.supports_dtc_channel(Hyperband::Imt2000, 2));
    }
}

#[test]
fn imt_support_uses_the_versioned_extension_and_ignores_reserved_bits() {
    let mut report = report(0);
    report.protocol_version = ProtocolVersion::Pv5;
    for bands in 0..=u8::MAX {
        report.options.extensions.frequency_bands = Some(bands);
        for channel in [0, 1, 2, 498, 499, 1000, 1998, 1999, 2000, u16::MAX] {
            assert_eq!(
                report.supports_dtc_channel(Hyperband::Imt2000, channel),
                bands & 1 != 0 && (2..=1999).contains(&channel)
            );
        }
    }
    report.options.extensions.frequency_bands = None;
    assert!(!report.supports_dtc_channel(Hyperband::Imt2000, 2));
    report.options.extensions.frequency_bands = Some(1);
    report.protocol_version = ProtocolVersion::Pv4;
    assert!(!report.supports_dtc_channel(Hyperband::Imt2000, 2));
}

#[test]
fn analog_service_does_not_claim_digital_traffic_support() {
    let mut report = report(0);
    report.services.analog_800_speech = true;
    for band in [
        Hyperband::Cellular800,
        Hyperband::Pcs1900,
        Hyperband::Cellular700,
        Hyperband::Imt2000,
    ] {
        assert!(!report.supports_dtc_channel(band, 2));
    }
}
