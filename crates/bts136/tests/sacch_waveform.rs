use bts136::phy::{
    acquisition::decode_forward_traffic,
    forward::ForwardModulator,
    sacch::{MESSAGE_BITS, SacchEncoder, SacchReceiver},
    sync::SyncWord,
    traffic::{self, DATA_BITS},
};

#[test]
fn continuous_sacch_words_recover_from_iq_that_starts_mid_stream() {
    let messages: [[bool; MESSAGE_BITS]; 4] = std::array::from_fn(|word| {
        std::array::from_fn(|bit| (bit + word).is_multiple_of(3 + word))
    });
    let mut encoder = SacchEncoder::default();
    let mut fields = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        fields.extend(encoder.encode_word(index != 0, message, 10));
    }
    fields.extend(encoder.encode_word(false, &[false; MESSAGE_BITS], 10));
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for sacch in fields.into_iter().skip(3) {
        let bits =
            traffic::encode_forward(SyncWord::One, &sacch, &[false; DATA_BITS], 10, 0).unwrap();
        samples.extend(transmitter.encode_slot(&bits));
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_traffic(&samples, transmitter.sample_rate_hz()).unwrap();
    let mut receiver = SacchReceiver::default();
    let mut words = Vec::new();
    for burst in bursts {
        if let Some(word) = receiver.push(burst.fields.fields.sacch, 10) {
            words.push(word);
        }
    }
    assert_eq!(words.len(), 3);
    for (word, message) in words.iter().zip(&messages[1..]) {
        assert_eq!(&word.message, message);
        assert!(word.continuation);
        assert_eq!(word.corrected_coded_bits, 0);
    }
    receiver.discontinuity();
    assert!(receiver.push([false; 12], 10).is_none());
}
