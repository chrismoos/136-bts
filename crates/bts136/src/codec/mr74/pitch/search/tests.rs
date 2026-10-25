use super::*;
#[test]
fn normalized_and_fractional_pitch_search_matches_fixed_point_reference() {
    let mut hash = 0xcbf29ce484222325u64;
    for pattern in 0..7 {
        let mut search = Search::default();
        for step in 0..256 {
            let excitation = std::array::from_fn(|i| match pattern {
                0 => 0,
                1 => {
                    if i == step % HISTORY_SAMPLES {
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
            let target = excitation[HISTORY_SAMPLES..].try_into().unwrap();
            let impulse = std::array::from_fn(|i| {
                if i == 0 {
                    4096
                } else {
                    ((i * 17 + step * 7) % 256) as i16 - 128
                }
            });
            let open = [
                OpenLoopLag::try_new((20 + ((step / 4) * 7) % 124) as u16).unwrap(),
                OpenLoopLag::try_new((20 + ((step / 4) * 19 + 37) % 124) as u16).unwrap(),
            ];
            let subframe = [
                Subframe::One,
                Subframe::Two,
                Subframe::Three,
                Subframe::Four,
            ][step % 4];
            let result = search
                .search(&excitation, target, &impulse, open, subframe)
                .unwrap();
            for value in [
                result.lag.samples() as i16,
                result.lag.fraction().thirds(),
                i16::from(result.index),
                search.previous.samples() as i16,
            ] {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        search.reset();
        assert_eq!(search, Search::default());
    }
    assert_eq!(hash, 0x9a964e3230db2bbf);
}
#[test]
fn invalid_open_loop_lags_and_out_of_order_subframes_do_not_advance_search() {
    for value in [0, 19, 144, u16::MAX] {
        assert_eq!(
            OpenLoopLag::try_new(value),
            Err(SearchError::OpenLoopLag(value))
        );
    }
    let mut search = Search::default();
    let previous = search.clone();
    let lag = OpenLoopLag::try_new(40).unwrap();
    let result = search.search(
        &[0; EXCITATION_SAMPLES],
        &[0; SUBFRAME_SAMPLES],
        &[0; SUBFRAME_SAMPLES],
        [lag; 2],
        Subframe::Two,
    );
    assert_eq!(
        result,
        Err(SearchError::SubframeOrder {
            expected: Subframe::One,
            received: Subframe::Two
        })
    );
    assert_eq!(search, previous);
}
