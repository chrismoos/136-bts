use super::*;
#[test]
fn codebook_search_and_pitch_sharpening_match_fixed_point_reference() {
    let mut hash = 0xcbf29ce484222325u64;
    for pattern in 0..8 {
        for step in 0..128 {
            let mut target = [0; SUBFRAME_SAMPLES];
            let mut impulse = [0; SUBFRAME_SAMPLES];
            for i in 0..SUBFRAME_SAMPLES {
                let (x, h) = match pattern {
                    0 => (0, if i == 0 { 4096 } else { 0 }),
                    1 => (
                        if i == step % 40 { 1000 } else { 0 },
                        if i == 0 { 4096 } else { 0 },
                    ),
                    2 => (
                        ((i * 997 + step * 347) % 8192) as i32 - 4096,
                        ((i * 73 + step * 17) % 1024) as i32 - 512,
                    ),
                    3 => {
                        if i % 2 == 0 {
                            (-32768, -32768)
                        } else {
                            (32767, 32767)
                        }
                    }
                    4 => (32767, 32767),
                    5 => (-32768, -32768),
                    6 => (
                        ((i * 347 + step * 997) % 65536) as i16 as i32,
                        if i == 0 {
                            4096
                        } else {
                            ((i * 73 + step * 31) % 256) as i32 - 128
                        },
                    ),
                    _ => (
                        ((i * 17 + step * 73) % 1024) as i32 - 512,
                        ((40 - i) * 90) as i32,
                    ),
                };
                target[i] = x as i16;
                impulse[i] = h as i16;
            }
            let delay = (20 + step % 124) as u16;
            let lag = (u8::MIN..=u8::MAX)
                .map(PitchLag::from_absolute)
                .find(|lag| lag.samples() == delay)
                .unwrap();
            let result = search(&target, &impulse, lag, ((step * 103) % 13018) as i16);
            for value in [result.pulse_positions as i16, result.pulse_signs as i16]
                .into_iter()
                .chain(result.innovation_q13)
                .chain(result.filtered_q12)
                .chain(result.sharpened_impulse_q12)
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
    }
    assert_eq!(hash, 0x9a0da718e3788e83);
}
#[test]
fn unsharpened_search_emits_the_transmitted_pulse_indices() {
    let target = std::array::from_fn(|i| ((i * 73) % 257) as i16 - 128);
    let mut impulse = [0; SUBFRAME_SAMPLES];
    impulse[0] = 4096;
    let result = search(&target, &impulse, PitchLag::from_absolute(u8::MAX), 0);
    let codebook = Codebook::try_new(result.pulse_positions, result.pulse_signs).unwrap();
    assert_eq!(result.innovation_q13, *codebook.samples_q13());
    assert_eq!(result.sharpened_impulse_q12, impulse);
}
