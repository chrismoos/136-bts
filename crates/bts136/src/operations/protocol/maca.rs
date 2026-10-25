use crate::l3::maca::MacaReport;
pub(super) fn details(report: &MacaReport) -> String {
    let mut text = if let Some(format) = report.measurements.format() {
        let mut text = format!("Format={format:?}");
        if let Some(fields) = report.measurements.fields() {
            if let Some(value) = fields.long_term {
                text.push_str(&format!(
                    "\nLTM WERCode={} BERCode={} RSSCode={} Full={} RSS={}",
                    value.wer,
                    value.ber,
                    value.rss,
                    value.full,
                    format
                        .rss_description(value.rss)
                        .unwrap_or_else(|error| error.to_string())
                ));
            }
            if let Some(values) = &fields.current {
                text.push_str(&format!("\nSTM serving/current-band codes={values:?}"));
            }
            for (index, group) in fields.other.iter().enumerate() {
                text.push_str(&format!(
                    "\nSTM other-list[{}] codes={:?}",
                    index + 1,
                    group.rss
                ));
            }
            if fields.current.is_some() || !fields.other.is_empty() {
                text.push_str("\nChannel identities require the matching requested-list order");
            }
        }
        text
    } else {
        "No recognized measurement fields".into()
    };
    if !report.ignored_bits.is_empty() {
        text.push_str(&format!("\nIgnoredBits={}", report.ignored_bits.len()));
    }
    if !report.unparsed_bits.is_empty() {
        text.push_str(&super::audit::tail(&report.unparsed_bits));
    }
    text
}
#[cfg(test)]
mod tests {
    #[test]
    fn maca_logs_keep_format_sparse_maps_and_serving_quality_without_guessing_frequencies() {
        let wire = [0x06, 0x14, 0xe3, 0x21, 0x55, 0x0c, 0xe9, 0x3e];
        let (name, text) = super::super::decode::control("Reverse", &wire);
        assert_eq!(name, "MACA Report");
        assert!(text.contains("Format=Basic"));
        assert!(text.contains("RSS=-79 dBm"));
        assert!(text.contains("Some(4), None, Some(31)"));
        assert!(text.contains("matching requested-list order"));
        let wire = [0x06, 0x4f, 0x8a, 0x4a, 0x20, 0x52, 0xc4, 0xd0];
        let (name, text) = super::super::decode::control("Reverse", &wire);
        assert_eq!(name, "MACA Report");
        assert!(text.contains("Format=Extended"));
        assert!(text.contains("RSS=≥-31 dBm"));
        assert_ne!(
            super::super::decode::control("Forward", &wire).0,
            "MACA Report"
        );
    }
}
