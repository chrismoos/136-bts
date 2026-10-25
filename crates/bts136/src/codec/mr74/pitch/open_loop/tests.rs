use super::*;
#[test]
fn coarse_pitch_and_continuous_history_match_fixed_point_reference() {
    let mut hash = 0xcbf29ce484222325u64;
    for pattern in 0..8 {
        let mut detector = Detector::default();
        for step in 0..256 {
            let samples = std::array::from_fn(|i| {
                let j = step * HALF_FRAME_SAMPLES + i;
                match pattern {
                    0 => 0,
                    1 => {
                        if j.is_multiple_of(97) {
                            30000
                        } else {
                            0
                        }
                    }
                    2 => {
                        if j.is_multiple_of(2) {
                            i16::MAX
                        } else {
                            i16::MIN
                        }
                    }
                    3 => i16::MAX,
                    4 => i16::MIN,
                    5 => ((j * 347 + 997) % 65536) as i16,
                    6 => ((j * 73) % 1024) as i16 - 512,
                    _ => {
                        if j % 40 < 20 {
                            1000
                        } else {
                            -1000
                        }
                    }
                }
            });
            let lag = detector.push(&samples);
            for value in [lag.samples() as i16]
                .into_iter()
                .chain(detector.history_q0)
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        detector.reset();
        assert_eq!(detector, Detector::default());
    }
    assert_eq!(hash, 0x59f8fd3815c3a4ec);
}
