use super::*;
use crate::{
    link::rach::{self, MobileIdentity},
    phy::rach::BurstFormat,
};

fn parameters() -> AccessParameters {
    let config = crate::config::Config::from_yaml(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/two-phones.yaml"
    )))
    .unwrap();
    let crate::l3::broadcast::MandatoryBroadcast::AccessParameters(parameters) =
        crate::runtime::serving::messages(&config)[1].clone()
    else {
        panic!()
    };
    parameters
}
#[test]
fn maximum_admitted_message_sizes_match_the_frame_budgets() {
    const MAXIMUM_MESSAGE_OCTETS: usize = 255;
    let message = [0; MAXIMUM_MESSAGE_OCTETS];
    for (format, expected) in [(BurstFormat::Normal, 43), (BurstFormat::Abbreviated, 56)] {
        let frames =
            rach::encode(format, MobileIdentity::Imsi50(0), &[&message, &message]).unwrap();
        assert_eq!(frames.len(), expected);
    }
}
#[test]
fn serving_parameters_include_access_retries_layer3_resend_and_receive_age() {
    const EXPECTED_ACCESS_MS: u64 = 68_440;
    const EXPECTED_GUARD_MS: u64 = 163_760;
    let lifetime =
        AccessLifetime::from_parameters(&parameters(), super::super::super::timers::SUPERFRAME_MS)
            .unwrap();
    assert_eq!(lifetime.maximum_access_ms, EXPECTED_ACCESS_MS);
    assert_eq!(lifetime.retry_guard_ms, EXPECTED_GUARD_MS);
    let without_age = AccessLifetime::from_parameters(&parameters(), 0).unwrap();
    assert_eq!(
        lifetime.retry_guard_ms - without_age.retry_guard_ms,
        super::super::super::timers::SUPERFRAME_MS
    );
}
#[test]
fn all_supported_access_profiles_fit_the_default_guard() {
    const EXPECTED_MAXIMUM_GUARD_MS: u64 = 504_560;
    let maximum = AccessLifetime::maximum();
    assert_eq!(maximum.retry_guard_ms, EXPECTED_MAXIMUM_GUARD_MS);
    let mut parameters = parameters();
    for retries in 0..=MAX_RETRIES {
        parameters.max_retries_code = retries;
        for repetitions in 0..=MAX_REPETITIONS {
            parameters.max_repetitions = repetitions;
            for busy in [false, true] {
                parameters.max_busy_reserved_code = busy;
                for abbreviated in [false, true] {
                    parameters.abbreviated_burst = abbreviated;
                    let lifetime = AccessLifetime::from_parameters(
                        &parameters,
                        super::super::super::timers::SUPERFRAME_MS,
                    )
                    .unwrap();
                    assert!(lifetime.maximum_access_ms <= maximum.maximum_access_ms);
                    assert!(lifetime.retry_guard_ms <= maximum.retry_guard_ms);
                    assert!(lifetime.retry_guard_ms > lifetime.maximum_access_ms);
                }
            }
        }
    }
}
#[test]
fn invalid_codes_and_receive_age_overflow_reject() {
    let mut parameters = parameters();
    parameters.max_retries_code = MAX_RETRIES + 1;
    assert!(matches!(
        AccessLifetime::from_parameters(&parameters, 0),
        Err(Error::Limits)
    ));
    parameters.max_retries_code = MAX_RETRIES;
    parameters.max_repetitions = MAX_REPETITIONS + 1;
    assert!(matches!(
        AccessLifetime::from_parameters(&parameters, 0),
        Err(Error::Limits)
    ));
    parameters.max_repetitions = MAX_REPETITIONS;
    assert!(matches!(
        AccessLifetime::from_parameters(&parameters, u64::MAX),
        Err(Error::Limits)
    ));
}

#[test]
fn short_contention_profiles_still_cover_full_size_sacch_retries() {
    const EXPECTED_TRAFFIC_GUARD_MS: u64 = 51_600;
    let mut parameters = parameters();
    parameters.max_retries_code = 0;
    parameters.max_repetitions = 0;
    parameters.max_busy_reserved_code = false;
    parameters.abbreviated_burst = false;
    let lifetime =
        AccessLifetime::from_parameters(&parameters, super::super::super::timers::SUPERFRAME_MS)
            .unwrap();
    assert_eq!(lifetime.retry_guard_ms, EXPECTED_TRAFFIC_GUARD_MS);
}
