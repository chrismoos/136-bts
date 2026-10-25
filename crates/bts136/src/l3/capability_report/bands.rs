use super::CapabilityReport;
use crate::l3::{broadcast::ProtocolVersion, legacy_traffic::Hyperband};

const CELLULAR_800: u8 = 0x01;
const PCS_A: u8 = 0x02;
const PCS_B: u8 = 0x04;
const PCS_C: u8 = 0x08;
const PCS_D: u8 = 0x10;
const PCS_E: u8 = 0x20;
const PCS_F: u8 = 0x40;
const PCS_BANDS: u8 = PCS_A | PCS_B | PCS_C | PCS_D | PCS_E | PCS_F;
const CELLULAR_700: u8 = 0x80;
const IMT_2000: u8 = 0x01;
// TIA-136-110-B Tables 1, 5 and 7 exclude unused channel numbers from 30 kHz operation.
const CELLULAR_FIRST: u16 = 1;
const CELLULAR_LAST: u16 = 799;
const CELLULAR_WRAP_FIRST: u16 = 991;
const CELLULAR_WRAP_LAST: u16 = 1023;
const CELLULAR_700_FIRST: u16 = 1;
const CELLULAR_700_LAST: u16 = 499;
const IMT_FIRST: u16 = 2;
const IMT_LAST: u16 = 1999;
const BAND_LABELS: [(u8, &str, &str); 8] = [
    (CELLULAR_800, "800 MHz A/B", "800AB"),
    (PCS_A, "1900 MHz A", "A"),
    (PCS_B, "1900 MHz B", "B"),
    (PCS_C, "1900 MHz C", "C"),
    (PCS_D, "1900 MHz D", "D"),
    (PCS_E, "1900 MHz E", "E"),
    (PCS_F, "1900 MHz F", "F"),
    (CELLULAR_700, "700 MHz C/D", "700CD"),
];

struct PcsRange {
    first: u16,
    last: u16,
    bands: u8,
}
// TIA-136-110-B Table 3 Note 1 requires border channels for either adjacent PCS block.
const PCS_RANGES: [PcsRange; 11] = [
    PcsRange {
        first: 2,
        last: 498,
        bands: PCS_A,
    },
    PcsRange {
        first: 499,
        last: 501,
        bands: PCS_A | PCS_D,
    },
    PcsRange {
        first: 502,
        last: 665,
        bands: PCS_D,
    },
    PcsRange {
        first: 666,
        last: 667,
        bands: PCS_D | PCS_B,
    },
    PcsRange {
        first: 668,
        last: 1165,
        bands: PCS_B,
    },
    PcsRange {
        first: 1166,
        last: 1167,
        bands: PCS_B | PCS_E,
    },
    PcsRange {
        first: 1168,
        last: 1332,
        bands: PCS_E,
    },
    PcsRange {
        first: 1333,
        last: 1334,
        bands: PCS_E | PCS_F,
    },
    PcsRange {
        first: 1335,
        last: 1498,
        bands: PCS_F,
    },
    PcsRange {
        first: 1499,
        last: 1501,
        bands: PCS_F | PCS_C,
    },
    PcsRange {
        first: 1502,
        last: 1998,
        bands: PCS_C,
    },
];

impl CapabilityReport {
    pub(crate) fn dtc_band_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = BAND_LABELS
            .into_iter()
            .filter_map(|(mask, name, _)| (self.supported_bands & mask != 0).then_some(name))
            .collect();
        if self.supports_dtc_channel(Hyperband::Imt2000, IMT_FIRST) {
            names.push("IMT-2000 2 GHz");
        }
        names
    }
    pub(crate) fn dtc_band_codes(&self) -> String {
        let mut codes = Vec::new();
        if self.supported_bands & CELLULAR_800 != 0 {
            codes.push("800AB".to_owned());
        }
        let pcs: String = BAND_LABELS
            .into_iter()
            .filter_map(|(mask, _, code)| {
                (self.supported_bands & mask & PCS_BANDS != 0).then_some(code)
            })
            .collect();
        if !pcs.is_empty() {
            codes.push(format!("PCS:{pcs}"));
        }
        if self.supported_bands & CELLULAR_700 != 0 {
            codes.push("700CD".to_owned());
        }
        if self.supports_dtc_channel(Hyperband::Imt2000, IMT_FIRST) {
            codes.push("IMT-2000".to_owned());
        }
        codes.join(",")
    }
    /// TIA-136-123-E §§6.239–240 report DTC support. This does not describe analog-only measurement capability.
    pub fn supports_dtc_channel(&self, hyperband: Hyperband, channel: u16) -> bool {
        match hyperband {
            Hyperband::Cellular800 => {
                self.supported_bands & CELLULAR_800 != 0
                    && ((CELLULAR_FIRST..=CELLULAR_LAST).contains(&channel)
                        || (CELLULAR_WRAP_FIRST..=CELLULAR_WRAP_LAST).contains(&channel))
            }
            Hyperband::Pcs1900 => PCS_RANGES.iter().any(|range| {
                (range.first..=range.last).contains(&channel)
                    && self.supported_bands & range.bands != 0
            }),
            Hyperband::Cellular700 => {
                self.supported_bands & CELLULAR_700 != 0
                    && (CELLULAR_700_FIRST..=CELLULAR_700_LAST).contains(&channel)
            }
            Hyperband::Imt2000 => {
                self.protocol_version.code() >= ProtocolVersion::Pv5.code()
                    && self
                        .options
                        .extensions
                        .frequency_bands
                        .is_some_and(|bands| bands & IMT_2000 != 0)
                    && (IMT_FIRST..=IMT_LAST).contains(&channel)
            }
        }
    }
}

#[cfg(test)]
mod tests;
