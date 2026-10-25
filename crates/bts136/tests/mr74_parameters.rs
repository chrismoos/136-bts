use bts136::codec::mr74::{FRAME_BITS, PARAMETER_COUNT, Parameters};
use serde::Deserialize;
#[derive(Deserialize)]
struct Vector {
    frame: usize,
    parameters: [u16; PARAMETER_COUNT],
    codec_hex: String,
}
#[test]
fn enhanced_codec_frame_vectors_preserve_all_quantized_parameters_and_bits() {
    let vectors: Vec<Vector> =
        serde_yaml_ng::from_str(include_str!("fixtures/mr74_parameters.json")).unwrap();
    assert_eq!(vectors.len(), 35);
    for vector in vectors {
        let mut all_bits = Vec::new();
        let (hex_pairs, remainder) = vector.codec_hex.as_bytes().as_chunks::<2>();
        assert!(remainder.is_empty());
        for hex in hex_pairs {
            let byte = u8::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap();
            all_bits.extend((0..8).rev().map(|bit| byte & (1 << bit) != 0));
        }
        assert!(all_bits[FRAME_BITS..].iter().all(|bit| !bit));
        let bits: [bool; FRAME_BITS] = all_bits[..FRAME_BITS].try_into().unwrap();
        let parameters = Parameters::from_codec_bits(&bits);
        assert_eq!(
            parameters.values(),
            &vector.parameters,
            "frame={}",
            vector.frame
        );
        assert_eq!(parameters.codec_bits(), bits, "frame={}", vector.frame);
        assert_eq!(Parameters::try_new(vector.parameters).unwrap(), parameters);
        assert_eq!(parameters.lsf_indices(), vector.parameters[..3]);
        for (index, subframe) in parameters.subframes().iter().enumerate() {
            let base = 3 + index * 4;
            assert_eq!(
                [
                    subframe.pitch_index,
                    subframe.pulse_positions,
                    subframe.pulse_signs,
                    subframe.gain_index
                ],
                vector.parameters[base..base + 4]
            );
        }
    }
}
