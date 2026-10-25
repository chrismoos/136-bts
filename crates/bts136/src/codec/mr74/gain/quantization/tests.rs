use super::*;
use crate::codec::mr74::codebook::Codebook;
#[test]
fn gains_energy_coefficients_and_consecutive_prediction_match_fixed_point_reference() {
    let mut hash = 0xcbf29ce484222325u64;
    for pattern in 0..7 {
        let mut quantizer = Quantizer::default();
        for step in 0..256 {
            let mut target = [0; SUBFRAME_SAMPLES];
            let mut adaptive = target;
            let mut filtered = target;
            for i in 0..SUBFRAME_SAMPLES {
                let (x, y, z) = match pattern {
                    0 => (0, 0, 0),
                    1 => (
                        if i == step % 40 { 30000 } else { 0 },
                        if i == (step + 3) % 40 { 30000 } else { 0 },
                        if i == step % 40 { 4096 } else { 0 },
                    ),
                    2 => (
                        if i % 2 == 0 { 32767 } else { -32768 },
                        if i % 3 == 0 { 32767 } else { -32768 },
                        if i % 2 == 0 { -32768 } else { 32767 },
                    ),
                    3 => (32767, 32767, 32767),
                    4 => (-32768, -32768, -32768),
                    5 => (
                        ((i * 347 + step * 997) % 65536) as i16 as i32,
                        ((i * 997 + step * 73) % 65536) as i16 as i32,
                        ((i * 73 + step * 347) % 65536) as i16 as i32,
                    ),
                    _ => (
                        ((i * 73 + step * 17) % 1024) as i32 - 512,
                        ((i * 17 + step * 53) % 512) as i32 - 256,
                        ((i * 53 + step * 31) % 8192) as i32 - 4096,
                    ),
                };
                target[i] = x as i16;
                adaptive[i] = y as i16;
                filtered[i] = z as i16;
            }
            let innovation = *Codebook::try_new(((step * 73) % 8192) as u16, (step % 16) as u16)
                .unwrap()
                .samples_q13();
            let pitch = pitch_gain(&target, &adaptive);
            let (fractions, exponents) =
                energy_coefficients(&target, &adaptive, &filtered, pitch.correlations);
            let prediction = quantizer.decoder.predict(&innovation);
            let limit = if step % 2 == 0 {
                GainLimit::Unrestricted
            } else {
                GainLimit::Resonance
            };
            let result = quantizer.quantize(&target, &adaptive, &filtered, &innovation, limit);
            for value in [pitch.gain_q14]
                .into_iter()
                .chain(pitch.correlations)
                .chain(fractions)
                .chain(exponents)
                .chain([
                    result.index as i16,
                    result.gains.pitch_q14,
                    result.gains.code_q1,
                ])
                .chain(quantizer.decoder.energies_q10)
                .chain([prediction.exponent, prediction.fraction as i16])
            {
                for byte in value.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
            assert!(result.gains.pitch_q14 <= limit.pitch_q14());
        }
        quantizer.reset();
        assert_eq!(quantizer, Quantizer::default());
    }
    assert_eq!(hash, 0x17b5d1f6b8c7318b);
}
