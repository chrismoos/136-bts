use super::*;
#[test]
fn complete_encoder_parameters_and_local_synthesis_match_fixed_point_reference() {
    const EXPECTED: [u64; 8] = [
        0x7687d1405f195c84,
        0xc09bd80d5771a202,
        0x3f67e33146152048,
        0xb4110570129474c3,
        0x0fe4d029e9b62c06,
        0x6fed7ec06be4a440,
        0x1900677827814770,
        0x9c4410cb9e765229,
    ];
    for (pattern, &expected) in EXPECTED.iter().enumerate() {
        let mut encoder = Encoder::default();
        let mut hash = 0xcbf29ce484222325u64;
        for frame in 0..64 {
            let input = std::array::from_fn(|i| {
                let j = frame * FRAME_SAMPLES + i;
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
            let output = encoder.encode(&input).unwrap();
            for value in output
                .parameters
                .values()
                .iter()
                .map(|v| *v as i16)
                .chain(output.synthesis_q0)
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        assert_eq!(hash, expected, "pattern {pattern}");
        encoder.reset();
        assert_eq!(encoder, Encoder::default());
    }
}
#[test]
fn entire_sample_wav_encodes_to_reference_parameters_and_local_synthesis() {
    let wav = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/sample-sound.wav"
    ));
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(&wav[36..40], b"data");
    let mut encoder = Encoder::default();
    let mut hash = 0xcbf29ce484222325u64;
    let mut frames = 0;
    for chunk in wav[44..]
        .chunks(FRAME_SAMPLES * 2)
        .chain(std::iter::once(&[][..]))
    {
        let mut samples = [0i16; FRAME_SAMPLES];
        for (sample, bytes) in samples.iter_mut().zip(chunk.as_chunks::<2>().0.iter()) {
            *sample = i16::from_le_bytes(*bytes);
        }
        let output = encoder.encode(&samples).unwrap();
        frames += 1;
        for value in output
            .parameters
            .values()
            .iter()
            .map(|v| *v as i16)
            .chain(output.synthesis_q0)
        {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        }
    }
    assert_eq!(frames, 345);
    assert_eq!(hash, 0xcffb879fd7ffcf48);
}
