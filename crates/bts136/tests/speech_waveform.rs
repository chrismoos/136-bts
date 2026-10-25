use bts136::codec::vselp::{VselpParameters, VselpSubframe};
use bts136::phy::{
    acquisition::decode_forward_traffic,
    forward::ForwardModulator,
    speech::{self},
    sync::SyncWord,
    traffic::{self, SACCH_BITS},
};

#[test]
fn adjacent_vselp_parameters_recover_exact_values_through_forward_iq() {
    let expected: Vec<_> = (0..3u8)
        .map(|frame| {
            let subframes = std::array::from_fn(|index| {
                let index = index as u8;
                VselpSubframe::try_new(
                    17 + frame * 3 + index * 13,
                    index * 7 + frame * 9,
                    2 + index * 5 + frame * 11,
                    0xa5 ^ (frame * 7) ^ (index * 0x11),
                )
                .unwrap()
            });
            VselpParameters::try_new(
                5 + frame,
                [42, 19, 13, 9, 6, 5, 3, 1, 7, 2].map(|coefficient| coefficient ^ frame),
                subframes,
            )
            .unwrap()
        })
        .collect();
    let padding = speech::encode(&VselpParameters::default().speech_bits());
    let mut previous = padding;
    let mut slots = Vec::new();
    for frame in &expected {
        let coded = speech::encode(&frame.speech_bits());
        slots.push(speech::interleave(&previous, &coded));
        previous = coded;
    }
    slots.push(speech::interleave(&previous, &padding));
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for slot in slots {
        let burst =
            traffic::encode_forward(SyncWord::One, &[false; SACCH_BITS], &slot, 10, 0).unwrap();
        samples.extend(transmitter.encode_slot(&burst));
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_traffic(&samples, transmitter.sample_rate_hz()).unwrap();
    assert_eq!(bursts.len(), expected.len() + 1);
    let recovered: Vec<_> = bursts
        .windows(2)
        .map(|pair| {
            let channel =
                speech::deinterleave(&pair[0].fields.fields.data, &pair[1].fields.fields.data);
            let decoded = speech::decode(&channel).unwrap();
            assert_eq!(decoded.corrected_coded_bits, 0);
            VselpParameters::from_speech_bits(&decoded.bits)
        })
        .collect();
    assert_eq!(recovered, expected);
}
