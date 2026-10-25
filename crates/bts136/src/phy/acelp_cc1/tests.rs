use super::*;

fn parameters() -> Parameters {
    Parameters::from_codec_bits(&std::array::from_fn(|i| (i * 17 + 5) % 31 < 15))
}
#[test]
fn allocation_and_interleaving_tables_are_permutations() {
    let mut speech = SPEECH_ORDER;
    speech.sort_unstable();
    assert_eq!(speech, std::array::from_fn(|i| i));
    let mut positions: Vec<_> = INTERLEAVER.into_iter().flatten().collect();
    positions.sort_unstable();
    assert_eq!(positions, (0..CHANNEL_BITS).collect::<Vec<_>>());
    let mut used = [false; INPUT_BITS];
    for i in 0..CLASS_ONE_BITS {
        let position = information_position(i);
        assert!(!used[position]);
        used[position] = true;
    }
    for i in CRC_POSITIONS {
        assert!(!used[i]);
        used[i] = true;
    }
    assert!(used[..INPUT_BITS - TAIL_BITS].iter().all(|b| *b));
    assert!(used[INPUT_BITS - TAIL_BITS..].iter().all(|b| !b));
}
#[test]
fn protected_and_unprotected_bits_preserve_all_speech_parameters() {
    for index in 0..FRAME_BITS {
        let mut bits = [false; FRAME_BITS];
        bits[index] = true;
        let parameters = Parameters::from_codec_bits(&bits);
        let decoded = decode(&encode(&parameters)).unwrap();
        assert!(decoded.crc_valid, "speech bit {index}");
        assert_eq!(decoded.parameters, parameters);
        assert_eq!(decoded.corrected_coded_bits, 0);
    }
}
#[test]
fn each_single_protected_bit_error_is_corrected() {
    let parameters = parameters();
    let channel = encode(&parameters);
    for index in 0..PROTECTED_BITS {
        let mut damaged = channel;
        damaged[index] ^= true;
        let decoded = decode(&damaged).unwrap();
        assert!(decoded.crc_valid, "channel bit {index}");
        assert_eq!(decoded.parameters, parameters);
        assert_eq!(decoded.corrected_coded_bits, 1);
    }
}
#[test]
fn class_two_errors_are_not_claimed_to_be_crc_protected() {
    let parameters = parameters();
    let channel = encode(&parameters);
    for index in PROTECTED_BITS..CHANNEL_BITS {
        let mut damaged = channel;
        damaged[index] ^= true;
        let decoded = decode(&damaged).unwrap();
        assert!(decoded.crc_valid);
        assert_ne!(decoded.parameters, parameters);
        assert_eq!(decoded.corrected_coded_bits, 0);
    }
}
#[test]
fn all_interleaving_depths_recover_the_same_frame_without_neighbor_leakage() {
    for mode in [Interleaving::One, Interleaving::Two, Interleaving::Three] {
        for index in 0..CHANNEL_BITS {
            let mut frames = [[false; CHANNEL_BITS]; 5];
            frames[2][index] = true;
            let slots = std::array::from_fn(|i| {
                interleave(&[frames[i], frames[i + 1], frames[i + 2]], mode)
            });
            assert_eq!(
                deinterleave(&slots, mode),
                frames[2],
                "{mode:?} bit {index}"
            );
        }
    }
}

fn channel_from_hex(value: &str) -> [bool; CHANNEL_BITS] {
    assert_eq!(value.len() * 4, CHANNEL_BITS);
    std::array::from_fn(|i| {
        let nibble = value.as_bytes()[i / 4];
        let nibble = (nibble as char).to_digit(16).unwrap();
        nibble & (1 << (3 - i % 4)) != 0
    })
}
#[test]
fn channel_matches_polynomial_reference_vector() {
    // TIA/EIA-136-410 §§4.1–4.3, polynomial-division CRC and direct delay-tap reference.
    let channel =
        channel_from_hex("d346ce409c9c4bb526f2d972a19ca879141c0e71e724533e61b05aaab56b5556a");
    assert_eq!(encode(&parameters()), channel);
    let decoded = decode(&channel).unwrap();
    assert_eq!(decoded.parameters, parameters());
    assert!(decoded.crc_valid);
    assert_eq!(decoded.corrected_coded_bits, 0);
}
#[test]
fn valid_trellis_with_wrong_parity_is_rejected_by_crc() {
    let channel =
        channel_from_hex("d346ce409c9ca67526f2d972a19ca879141c0e71e724533e61b05aaab56b5556a");
    let decoded = decode(&channel).unwrap();
    assert_eq!(decoded.parameters, parameters());
    assert!(!decoded.crc_valid);
    assert_eq!(decoded.corrected_coded_bits, 0);
}
