use bts136::{
    link::broadcast::{BroadcastChanges, FbcchReceiver, encode_fbcch},
    phy::{
        acquisition::decode_forward_control,
        control::{self, LogicalChannel, SharedFeedback},
        forward::ForwardModulator,
        sync::SyncWord,
        traffic::{self, DATA_BITS, SACCH_BITS},
    },
};

#[test]
fn fragmented_broadcast_octets_survive_complete_forward_waveform_chain() {
    let long: Vec<u8> = (0..30).collect();
    let short = [0xaa, 0x55, 0x0b, 0x01, 0x7f];
    let frames = encode_fbcch(
        &[&long, &short],
        BroadcastChanges {
            fbcch: false,
            ebcch: false,
        },
        3,
    )
    .unwrap();
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for (phase, information) in frames.iter().enumerate() {
        let bits = control::encode_forward(
            SyncWord::One,
            phase as u8,
            &SharedFeedback {
                leading: [false; 12],
                trailing: [false; 10],
            },
            information,
            LogicalChannel::Fbcch,
            0,
        )
        .unwrap();
        samples.extend(transmitter.encode_slot(&bits));
        for word in [SyncWord::Two, SyncWord::Three] {
            let filler =
                traffic::encode_forward(word, &[false; SACCH_BITS], &[false; DATA_BITS], 1, 0)
                    .unwrap();
            samples.extend(transmitter.encode_slot(&filler));
        }
    }
    samples.extend(transmitter.finish());
    let decoded = decode_forward_control(
        &samples,
        transmitter.sample_rate_hz(),
        LogicalChannel::Fbcch,
        0,
    )
    .unwrap();
    assert_eq!(decoded.len(), 3);
    let mut receiver = FbcchReceiver::default();
    let mut messages = Vec::new();
    for (phase, burst) in decoded.iter().enumerate() {
        assert_eq!(burst.fields.superframe_phase, phase as u8);
        let information: [bool; 109] = burst.fields.block.information.clone().try_into().unwrap();
        messages.extend(receiver.push(&information).unwrap());
    }
    assert_eq!(messages, [long, short.to_vec()]);
}

#[test]
fn required_broadcast_wire_vectors_reassemble_and_decode_from_iq() {
    use bts136::l3::broadcast::{MandatoryBroadcast, ProtocolVersion, decode_prefix};
    let structure = [0x04, 0, 0, 0x10, 0x10, 0];
    let access = [0x01, 0x40, 0, 0, 0, 0x22, 0xfd, 0x17];
    let selection = [0x03, 0x88, 0xe6, 0xb0];
    let registration = [0x07, 0xaa, 0x80];
    let identity = [0x0b, 0, 0x55, 0x10];
    let messages: [&[u8]; 5] = [&structure, &access, &selection, &registration, &identity];
    let frames = encode_fbcch(
        &messages,
        BroadcastChanges {
            fbcch: false,
            ebcch: false,
        },
        3,
    )
    .unwrap();
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for (phase, information) in frames.iter().enumerate() {
        let bits = control::encode_forward(
            SyncWord::One,
            phase as u8,
            &SharedFeedback {
                leading: [false; 12],
                trailing: [false; 10],
            },
            information,
            LogicalChannel::Fbcch,
            0,
        )
        .unwrap();
        samples.extend(transmitter.encode_slot(&bits));
        for word in [SyncWord::Two, SyncWord::Three] {
            let filler =
                traffic::encode_forward(word, &[false; SACCH_BITS], &[false; DATA_BITS], 1, 0)
                    .unwrap();
            samples.extend(transmitter.encode_slot(&filler));
        }
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_control(
        &samples,
        transmitter.sample_rate_hz(),
        LogicalChannel::Fbcch,
        0,
    )
    .unwrap();
    assert_eq!(bursts.len(), 3);
    let mut receiver = FbcchReceiver::default();
    let mut recovered = Vec::new();
    for burst in bursts {
        let bits = burst.fields.block.information.try_into().unwrap();
        recovered.extend(receiver.push(&bits).unwrap());
    }
    assert_eq!(recovered, messages);
    let parsed: Vec<_> = recovered
        .iter()
        .map(|bytes| decode_prefix(bytes).unwrap())
        .collect();
    assert!(
        parsed
            .iter()
            .all(|message| message.optional_bits.is_empty())
    );
    let MandatoryBroadcast::DcchStructure(structure) = &parsed[0].message else {
        panic!("first broadcast message is DCCH structure");
    };
    assert_eq!(structure.fbcch_slots, 3);
    assert_eq!(structure.ebcch_slots, 1);
    assert_eq!(structure.dvcc, 1);
    let MandatoryBroadcast::SystemIdentity(identity) = &parsed[4].message else {
        panic!("last broadcast message is system identity");
    };
    assert_eq!(identity.sid, 42);
    assert_eq!(identity.network_types, 4);
    assert_eq!(identity.protocol_version, ProtocolVersion::Is136RevA);
}
