use super::*;
fn reference(seed: u32) -> [Complex64; SLOT_SYMBOLS] {
    let mut state = seed;
    let mut phase = 0.0;
    std::array::from_fn(|_| {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        phase += match state >> 30 {
            0 => std::f64::consts::FRAC_PI_4,
            1 => 3.0 * std::f64::consts::FRAC_PI_4,
            2 => -std::f64::consts::FRAC_PI_4,
            _ => -3.0 * std::f64::consts::FRAC_PI_4,
        };
        Complex64::from_polar(1.0, phase)
    })
}
#[test]
fn channel_fit_recovers_precursors_and_postcursors_independently_of_gain_and_phase() {
    let reference = reference(17);
    for lag in [-12, -5, 5, 12] {
        for amplitude in [0.1, 1.0, 10.0] {
            let gain = Complex64::from_polar(amplitude, 1.2);
            let received = std::array::from_fn(|index| {
                let n = FIRST_TRAINING_SYMBOL + index;
                gain * (reference[n]
                    + Complex64::from_polar(0.3, -0.8) * reference[(n as i32 - lag) as usize])
            });
            let profile = fit(&reference, &received).unwrap();
            assert_eq!(
                (profile.earliest_symbol, profile.latest_symbol),
                (lag.min(0), lag.max(0))
            );
            assert!(profile.residual_ratio < 1e-20);
            assert!((profile.main_path_power - amplitude * amplitude).abs() < 1e-10);
        }
    }
}
#[test]
fn single_path_low_noise_and_subthreshold_echo_do_not_create_significant_spread() {
    let reference = reference(65);
    for echo in [0.0, 0.1] {
        let received = std::array::from_fn(|index| {
            let n = FIRST_TRAINING_SYMBOL + index;
            reference[n]
                + echo * reference[n - 7]
                + Complex64::new(
                    (n as f64 * 2.17).sin() * 0.01,
                    (n as f64 * 1.33).cos() * 0.01,
                )
        });
        let profile = fit(&reference, &received).unwrap();
        assert_eq!((profile.earliest_symbol, profile.latest_symbol), (0, 0));
    }
}
#[test]
fn unexplained_interference_and_outside_search_echo_are_not_reported_as_clean_channels() {
    let reference = reference(91);
    let received = std::array::from_fn(|index| {
        let n = FIRST_TRAINING_SYMBOL + index;
        reference[n] + Complex64::from_polar(0.5, n as f64 * 0.073)
    });
    assert!(fit(&reference, &received).is_none());
    assert!(fit_with_refinement(&reference, &received).is_none());
    let received = std::array::from_fn(|index| {
        let n = FIRST_TRAINING_SYMBOL + index;
        reference[n] + reference[(n + 21) % SLOT_SYMBOLS] * 0.4
    });
    assert!(fit(&reference, &received).is_none());
    assert!(fit_with_refinement(&reference, &received).is_none());
    assert!(fit(&reference, &[Complex64::new(0.0, 0.0); TRAINING_SYMBOLS]).is_none());
}
fn raised_cosine(t: f64) -> f64 {
    const ROLL_OFF: f64 = 0.35;
    if t.abs() < 1e-10 {
        return 1.0;
    }
    let pi = std::f64::consts::PI;
    (pi * t).sin() / (pi * t) * (pi * ROLL_OFF * t).cos() / (1.0 - (2.0 * ROLL_OFF * t).powi(2))
}
#[test]
fn fractional_delay_echo_is_resolved_without_confusing_nyquist_pulse_tails() {
    let reference = reference(75);
    for amplitude in [0.3, 0.35] {
        let received = std::array::from_fn(|index| {
            let n = (FIRST_TRAINING_SYMBOL + index) as f64;
            reference
                .iter()
                .enumerate()
                .map(|(m, symbol)| {
                    symbol
                        * (raised_cosine(n - m as f64)
                            + amplitude * raised_cosine(n - m as f64 - 4.5))
                })
                .sum()
        });
        let profile = fit(&reference, &received).unwrap();
        assert_eq!(profile.earliest_symbol, 0);
        assert_eq!(profile.latest_symbol, 5);
        assert!(profile.residual_ratio < MAX_RESIDUAL_RATIO);
    }
}

#[test]
fn periodic_reference_cannot_identify_path_delays_and_is_not_reported_as_clean() {
    let reference =
        std::array::from_fn(|n| Complex64::from_polar(1.0, n as f64 * std::f64::consts::FRAC_PI_4));
    let received = column(&reference, 0);
    assert!(fit(&reference, &received).is_none());
    assert!(fit_with_refinement(&reference, &received).is_none());
}

#[test]
fn pulse_filtered_waveform_profiles_use_the_same_sample_reference_as_acquisition() {
    use crate::phy::modulation::Symbol;
    let sps = 16;
    let filter =
        crate::phy::pulse::RootRaisedCosine::try_new(SYMBOL_RATE_HZ * sps as f64, 8).unwrap();
    let mut bits = [false; SLOT_SYMBOLS * 2];
    let mut state = 17u32;
    let mut phase = 0.0;
    let mut impulses = vec![Symbol { i: 0.0, q: 0.0 }; (SLOT_SYMBOLS + 32) * sps];
    for symbol in REVERSE_DATA_SYMBOL..SLOT_SYMBOLS {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let code = state >> 30;
        bits[symbol * 2] = code & 2 != 0;
        bits[symbol * 2 + 1] = code & 1 != 0;
        phase += match code {
            0 => std::f64::consts::FRAC_PI_4,
            1 => 3.0 * std::f64::consts::FRAC_PI_4,
            2 => -std::f64::consts::FRAC_PI_4,
            _ => -3.0 * std::f64::consts::FRAC_PI_4,
        };
        let (q, i) = phase.sin_cos();
        impulses[symbol * sps] = Symbol { i, q };
    }
    let raw = filter.filter(&impulses);
    for echo in [0.0, 0.3] {
        let mut samples = raw.clone();
        for n in 5 * sps..samples.len() {
            samples[n].i += echo * raw[n - 5 * sps].i;
            samples[n].q += echo * raw[n - 5 * sps].q;
        }
        let mut workspace =
            AcquisitionWorkspace::try_new(SYMBOL_RATE_HZ * sps as f64, samples.len()).unwrap();
        workspace
            .filter
            .filter_into(&samples, &mut workspace.filtered);
        let burst = AcquiredSlot {
            slot_start_sample: filter.delay_samples() as f64,
            sync_correlation: 1.0,
            frequency_offset_hz: 0.0,
            hard_bits: bits,
            fields: (),
            quality: None,
        };
        let profile = estimate(&workspace, &burst).unwrap();
        assert_eq!(
            (profile.earliest_symbol, profile.latest_symbol),
            (0, if echo == 0.0 { 0 } else { 5 }),
            "{profile:?}"
        );
    }
}

#[test]
fn decoded_body_refines_sync_frequency_bias_without_relaxing_fit_quality() {
    let reference = reference(92);
    for hz in [-40.0, 40.0] {
        let received = std::array::from_fn(|index| {
            let n = FIRST_TRAINING_SYMBOL + index;
            (reference[n] + reference[n - 5] * Complex64::from_polar(0.3, 0.8))
                * Complex64::from_polar(1.0, TAU * hz * index as f64 / SYMBOL_RATE_HZ)
        });
        assert!(fit(&reference, &received).is_none());
        let profile = fit_with_refinement(&reference, &received).unwrap();
        assert_eq!((profile.earliest_symbol, profile.latest_symbol), (0, 5));
        assert!(profile.residual_ratio < MAX_RESIDUAL_RATIO);
    }
}
