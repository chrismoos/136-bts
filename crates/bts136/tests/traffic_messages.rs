use bts136::{
    l3::legacy_traffic::TrafficMessage,
    link::traffic::{self as link, SignalingWord, TrafficMessageReceiver},
    phy::{
        acquisition::decode_forward_traffic,
        facch,
        forward::ForwardModulator,
        sacch::{SacchEncoder, SacchReceiver},
        sync::SyncWord,
        traffic::{self, DATA_BITS, SACCH_BITS},
    },
};

fn message() -> TrafficMessage {
    TrafficMessage::Ack {
        ack_message_type: 0xc1,
        information: bts136::l3::legacy_traffic::AcknowledgmentInformation {
            unparsed_bits: ([0x0e, 0xa5, 0xff, 0, 0x55, 1, 3, 7])
                .iter()
                .flat_map(|byte| {
                    (0..u8::BITS)
                        .rev()
                        .map(move |shift| byte & (1 << shift) != 0)
                })
                .collect(),
            ..Default::default()
        },
    }
}

#[test]
fn fragmented_legacy_ack_recovers_from_forward_facch_iq() {
    let expected = message();
    let words = link::encode(&expected).unwrap();
    let filler = SignalingWord::filler();
    let padding = facch::encode(filler.continuation, &filler.contents, 10);
    let coded: Vec<_> = words
        .iter()
        .map(|word| facch::encode(word.continuation, &word.contents, 10))
        .collect();
    let mut previous = padding;
    let mut data = Vec::new();
    for word in &coded {
        data.push(facch::interleave(&previous, word));
        previous = *word;
    }
    data.push(facch::interleave(&previous, &padding));
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for data in data {
        let bits =
            traffic::encode_forward(SyncWord::One, &[false; SACCH_BITS], &data, 10, 0).unwrap();
        samples.extend(transmitter.encode_slot(&bits));
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_traffic(&samples, transmitter.sample_rate_hz()).unwrap();
    assert_eq!(bursts.len(), words.len() + 1);
    let mut receiver = TrafficMessageReceiver::default();
    let mut messages = Vec::new();
    for pair in bursts.windows(2) {
        let coded = facch::deinterleave(&pair[0].fields.fields.data, &pair[1].fields.fields.data);
        let word = facch::decode(&coded, 10).unwrap();
        if let Some(message) = receiver.push(&SignalingWord::from(&word)).unwrap() {
            messages.push(message);
        }
    }
    assert_eq!(messages, [expected]);
}

#[test]
fn fragmented_legacy_ack_recovers_from_continuous_sacch_iq() {
    let expected = message();
    let words = link::encode(&expected).unwrap();
    let mut encoder = SacchEncoder::default();
    let mut fields = Vec::new();
    for word in &words {
        fields.extend(encoder.encode_word(word.continuation, &word.contents, 10));
    }
    let filler = SignalingWord::filler();
    fields.extend(encoder.encode_word(false, &filler.contents, 10));
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for sacch in fields {
        let bits =
            traffic::encode_forward(SyncWord::One, &sacch, &[false; DATA_BITS], 10, 0).unwrap();
        samples.extend(transmitter.encode_slot(&bits));
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_traffic(&samples, transmitter.sample_rate_hz()).unwrap();
    let mut decoder = SacchReceiver::default();
    let mut receiver = TrafficMessageReceiver::default();
    let mut messages = Vec::new();
    for burst in bursts {
        if let Some(word) = decoder.push(burst.fields.fields.sacch, 10)
            && let Some(message) = receiver.push(&SignalingWord::from(&word)).unwrap()
        {
            messages.push(message);
        }
    }
    assert_eq!(messages, [expected]);
}
