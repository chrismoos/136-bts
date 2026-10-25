use super::*;
#[test]
fn targets_impulses_residuals_and_weighted_speech_match_fixed_point_reference() {
    let mut hash = 0xcbf29ce484222325u64;
    for pattern in 0..7 {
        let mut filter = WeightedSpeechFilter::default();
        for step in 0..64 {
            let a = std::array::from_fn(|k| {
                if k == 0 {
                    4096
                } else {
                    ((k * 31 + step * 73) % 1024) as i16 - 512
                }
            });
            let aq = std::array::from_fn(|k| {
                if k == 0 {
                    4096
                } else {
                    ((k * 53 + step * 97) % 1024) as i16 - 512
                }
            });
            let speech = std::array::from_fn(|i| match pattern {
                0 => 0,
                1 => {
                    if i == step % 50 {
                        30000
                    } else {
                        0
                    }
                }
                2 => {
                    if i.is_multiple_of(2) {
                        i16::MAX
                    } else {
                        i16::MIN
                    }
                }
                3 => i16::MAX,
                4 => i16::MIN,
                5 => ((i * 347 + step * 997) % 65536) as i16,
                _ => ((i * 73 + step * 17) % 1024) as i16 - 512,
            });
            let error = std::array::from_fn(|i| ((i * 37 + step * 53) % 512) as i16 - 256);
            let weighting = std::array::from_fn(|i| ((i * 23 + step * 13) % 256) as i16 - 128);
            let prepared = prepare(&speech, &a, &aq, &error, &weighting);
            let weighted = filter.filter(&speech, &a);
            for value in prepared
                .residual_q0
                .into_iter()
                .chain(prepared.impulse_q12)
                .chain(prepared.target_q0)
                .chain(prepared.error_q0)
                .chain(weighted)
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        filter.reset();
        assert_eq!(filter, WeightedSpeechFilter::default());
    }
    assert_eq!(hash, 0x0457f1b555a09026);
}
