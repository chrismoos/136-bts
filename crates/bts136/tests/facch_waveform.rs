use bts136::phy::{
    acquisition::decode_forward_traffic,
    facch::{self, MESSAGE_BITS},
    forward::ForwardModulator,
    sync::SyncWord,
    traffic::{self, DATA_BITS, SACCH_BITS},
};

#[test]
fn facch_words_survive_forward_slot_coding_modulation_and_acquisition() {
    let a: [bool; MESSAGE_BITS] = std::array::from_fn(|index| index.is_multiple_of(3));
    let b: [bool; MESSAGE_BITS] = std::array::from_fn(|index| index.is_multiple_of(7));
    let coded_a = facch::encode(false, &a, 10);
    let coded_b = facch::encode(true, &b, 10);
    let padding = facch::encode(false, &[false; MESSAGE_BITS], 10);
    let data = [
        facch::interleave(&padding, &coded_a),
        facch::interleave(&coded_a, &coded_b),
        facch::interleave(&coded_b, &padding),
    ];
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for data in data {
        let bits =
            traffic::encode_forward(SyncWord::One, &[false; SACCH_BITS], &data, 10, 0).unwrap();
        samples.extend(transmitter.encode_slot(&bits));
        for word in [SyncWord::Two, SyncWord::Three] {
            let filler =
                traffic::encode_forward(word, &[false; SACCH_BITS], &[false; DATA_BITS], 10, 0)
                    .unwrap();
            samples.extend(transmitter.encode_slot(&filler));
        }
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_traffic(&samples, transmitter.sample_rate_hz()).unwrap();
    let assigned: Vec<_> = bursts
        .iter()
        .filter(|burst| burst.fields.fields.sync == SyncWord::One)
        .collect();
    assert_eq!(assigned.len(), 3);
    let mut words = Vec::new();
    for pair in assigned.windows(2) {
        let coded = facch::deinterleave(&pair[0].fields.fields.data, &pair[1].fields.fields.data);
        words.push(facch::decode(&coded, 10).unwrap());
    }
    assert_eq!(words[0].message, a);
    assert!(!words[0].continuation);
    assert_eq!(words[1].message, b);
    assert!(words[1].continuation);
    assert!(words.iter().all(|word| word.corrected_coded_bits == 0));
}
