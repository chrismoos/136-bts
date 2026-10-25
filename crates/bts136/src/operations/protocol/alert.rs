use crate::l3::legacy_traffic::AlertInformation;
pub(super) fn details(information: &AlertInformation) -> String {
    let mut text = information.signals.as_ref().map_or_else(
        || "Signal=standard(1)".into(),
        |values| format!("Signal={values:?}"),
    );
    for (index, number) in information.calling_numbers.iter().enumerate() {
        text.push_str(&format!(
            "\nCaller[{index}] Type={} Plan={} Presentation={} Screening={} Number={}",
            number.number_type,
            number.numbering_plan,
            number.presentation,
            number.screening,
            number.characters.escape_default()
        ));
    }
    if let Some(name) = &information.calling_name {
        text.push_str(&format!(
            "\nCallerName Presentation={} Screening={} Name={}",
            name.presentation,
            name.screening,
            name.characters.escape_default()
        ));
    }
    if !information.unparsed_bits.is_empty() {
        text.push_str(&super::audit::tail(&information.unparsed_bits));
    }
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::legacy_traffic::{CallingName, CallingNumber};
    #[test]
    fn maximum_alert_record_retains_presentation_names_and_extensions_without_truncation() {
        let information = AlertInformation {
            signals: Some(vec![140]),
            calling_numbers: vec![CallingNumber::network(&"1".repeat(15)).unwrap(); 2],
            calling_name: Some(CallingName::network(&"\x1b".repeat(20)).unwrap()),
            unparsed_bits: (0..16).map(|bit| bit % 2 == 0).collect(),
        };
        information.validate().unwrap();
        let text = details(&information);
        assert!(text.len() <= super::super::MAX_RECORD_BYTES);
        assert!(text.contains("Caller[1]"));
        assert!(text.contains("CallerName Presentation=0 Screening=3"));
        assert!(text.ends_with("UnknownBits=16:AAAA"));
    }
}
