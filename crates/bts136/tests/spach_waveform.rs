use bts136::{
    l3::registration::{RegistrationResponse, RejectCause},
    link::{
        rach::MobileIdentity,
        spach::{Channel, HeaderFlags, SingleMessage, decode_single},
    },
    phy::{
        acquisition::decode_forward_control,
        control::{self, LogicalChannel, SharedFeedback},
        forward::ForwardModulator,
        sync::SyncWord,
        traffic::{self, DATA_BITS, SACCH_BITS},
    },
};

#[test]
fn addressed_registration_responses_recover_from_forward_iq_for_all_identity_formats() {
    let identities = [
        MobileIdentity::Tmsi20(0xfffff),
        MobileIdentity::Tmsi24(0xffffff),
        MobileIdentity::Min34(0x123456789),
        MobileIdentity::Imsi50(0x123456789abcd),
    ];
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    let mut expected = Vec::new();
    for (phase, identity) in identities.into_iter().enumerate() {
        let response = if phase == 0 {
            RegistrationResponse::Accept
        } else {
            RegistrationResponse::Reject {
                cause: RejectCause::Congestion,
            }
        };
        let frame = SingleMessage {
            channel: Channel::Arch,
            flags: HeaderFlags {
                bcch_change: true,
                reservation_response: true,
                ..HeaderFlags::default()
            },
            identity,
            message: response.encode(),
        };
        let information = frame.encode().unwrap();
        let bits = control::encode_forward(
            SyncWord::One,
            phase as u8,
            &SharedFeedback {
                leading: [false; 12],
                trailing: [false; 10],
            },
            &information,
            LogicalChannel::Spach,
            42,
        )
        .unwrap();
        samples.extend(transmitter.encode_slot(&bits));
        for word in [SyncWord::Two, SyncWord::Three] {
            samples.extend(
                transmitter.encode_slot(
                    &traffic::encode_forward(
                        word,
                        &[false; SACCH_BITS],
                        &[false; DATA_BITS],
                        42,
                        0,
                    )
                    .unwrap(),
                ),
            );
        }
        expected.push((frame, response));
    }
    samples.extend(transmitter.finish());
    let decoded = decode_forward_control(
        &samples,
        transmitter.sample_rate_hz(),
        LogicalChannel::Spach,
        42,
    )
    .unwrap();
    assert_eq!(decoded.len(), expected.len());
    for (burst, (frame, response)) in decoded.iter().zip(expected) {
        let recovered = decode_single(&burst.fields.block.information).unwrap();
        assert_eq!(recovered, frame);
        assert_eq!(
            RegistrationResponse::decode_prefix(&recovered.message)
                .unwrap()
                .message,
            response
        );
    }
    assert!(
        decode_forward_control(
            &samples,
            transmitter.sample_rate_hz(),
            LogicalChannel::Spach,
            43
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn maximum_length_spach_message_reassembles_from_forward_iq() {
    use bts136::link::spach::SpachReceiver;
    let expected = SingleMessage {
        channel: Channel::Smsch,
        flags: HeaderFlags::default(),
        identity: MobileIdentity::Imsi50(0x123456789abcd),
        message: (0..255).map(|octet| octet as u8).collect(),
    };
    let frames = expected.encode_fragmented().unwrap();
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
            LogicalChannel::Spach,
            42,
        )
        .unwrap();
        samples.extend(transmitter.encode_slot(&bits));
        for word in [SyncWord::Two, SyncWord::Three] {
            samples.extend(
                transmitter.encode_slot(
                    &traffic::encode_forward(
                        word,
                        &[false; SACCH_BITS],
                        &[false; DATA_BITS],
                        42,
                        0,
                    )
                    .unwrap(),
                ),
            );
        }
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_control(
        &samples,
        transmitter.sample_rate_hz(),
        LogicalChannel::Spach,
        42,
    )
    .unwrap();
    assert_eq!(bursts.len(), frames.len());
    let mut receiver = SpachReceiver::default();
    let mut messages = Vec::new();
    for burst in bursts {
        if let Some(message) = receiver.push(&burst.fields.block.information).unwrap() {
            messages.push(message);
        }
    }
    assert_eq!(messages, [expected]);
}

#[test]
fn arq_iq_reassembly_reports_missing_frame_then_recovers_retransmission() {
    use bts136::{
        link::{
            rach::{self, Outcome, RachReceiver},
            spach::arq::{self, ArqHeader, ArqReassembler, Reception, ResponseMode},
        },
        phy::rach::{self as phy_rach, BurstFormat},
    };
    let identity = MobileIdentity::Imsi50(1);
    let expected: Vec<u8> = (0..40).collect();
    let frames = arq::fragment_message(
        ArqHeader {
            channel: Channel::Smsch,
            flags: HeaderFlags::default(),
            partial_echo: 42,
            polling: true,
            response_mode: ResponseMode::Reservation,
        },
        identity,
        &expected,
    )
    .unwrap();
    assert_eq!(frames.len(), 5);
    let order = [0, 4, 1, 3, 4, 2];
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for (phase, index) in order.iter().enumerate() {
        let information = frames[*index].encode().unwrap();
        let bits = control::encode_forward(
            SyncWord::One,
            phase as u8,
            &SharedFeedback {
                leading: [false; 12],
                trailing: [false; 10],
            },
            &information,
            LogicalChannel::Spach,
            42,
        )
        .unwrap();
        samples.extend(transmitter.encode_slot(&bits));
        for word in [SyncWord::Two, SyncWord::Three] {
            samples.extend(
                transmitter.encode_slot(
                    &traffic::encode_forward(
                        word,
                        &[false; SACCH_BITS],
                        &[false; DATA_BITS],
                        42,
                        0,
                    )
                    .unwrap(),
                ),
            );
        }
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_control(
        &samples,
        transmitter.sample_rate_hz(),
        LogicalChannel::Spach,
        42,
    )
    .unwrap();
    assert_eq!(bursts.len(), order.len());
    let mut receiver = ArqReassembler::try_new(identity).unwrap();
    let mut messages = Vec::new();
    let mut polls = Vec::new();
    for burst in bursts {
        let Reception::Accepted {
            message,
            status_request: Some(request),
        } = receiver.push(&burst.fields.block.information).unwrap()
        else {
            panic!("expected accepted polled frame");
        };
        assert_eq!(request.response_mode, ResponseMode::Reservation);
        if let Some(message) = message {
            messages.push(message.message);
        }
        polls.push(request.status);
    }
    assert_eq!(messages, [expected]);
    assert_eq!(polls[4].received_frames[..4], [true, false, true, true]);
    assert_eq!(polls[5].received_frames[..4], [true; 4]);
    assert!(
        polls[5].received_frames[4..]
            .iter()
            .all(|received| !received)
    );
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let information = rach::encode_arq_status(format, &polls[4]).unwrap();
        let bits = phy_rach::encode(format, SyncWord::Two, &information, 42).unwrap();
        let decoded = phy_rach::decode(&bits, format, 42).unwrap();
        assert_eq!(
            RachReceiver::default()
                .push(format, &decoded.block.information)
                .unwrap(),
            Outcome::ArqStatus(polls[4].clone())
        );
    }
}

#[test]
fn null_and_all_hard_page_formats_recover_from_forward_iq() {
    use bts136::link::spach::paging::{self, PageBody, PageFrame, PagingFlags};
    let bodies = [
        PageBody::Null { go_away: true },
        PageBody::TripleMin34([Some(1), None, Some(0x3ffffffff)]),
        PageBody::QuadrupleTmsi20([Some(1), None, Some(0xfffff), None]),
        PageBody::QuadrupleTmsi24([Some(1), Some(0xffffff), None, None]),
        PageBody::PentaTmsi20([Some(1), None, Some(0xfffff), None, None]),
    ];
    let expected: Vec<_> = bodies
        .into_iter()
        .map(|body| PageFrame {
            flags: PagingFlags {
                bcch_change: true,
                ..PagingFlags::default()
            },
            body,
        })
        .collect();
    let mut transmitter = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    for (phase, frame) in expected.iter().enumerate() {
        let information = frame.encode().unwrap();
        let bits = control::encode_forward(
            SyncWord::One,
            phase as u8,
            &SharedFeedback {
                leading: [false; 12],
                trailing: [false; 10],
            },
            &information,
            LogicalChannel::Spach,
            42,
        )
        .unwrap();
        samples.extend(transmitter.encode_slot(&bits));
        for word in [SyncWord::Two, SyncWord::Three] {
            samples.extend(
                transmitter.encode_slot(
                    &traffic::encode_forward(
                        word,
                        &[false; SACCH_BITS],
                        &[false; DATA_BITS],
                        42,
                        0,
                    )
                    .unwrap(),
                ),
            );
        }
    }
    samples.extend(transmitter.finish());
    let bursts = decode_forward_control(
        &samples,
        transmitter.sample_rate_hz(),
        LogicalChannel::Spach,
        42,
    )
    .unwrap();
    assert_eq!(bursts.len(), expected.len());
    let decoded: Vec<_> = bursts
        .iter()
        .map(|burst| paging::decode(&burst.fields.block.information).unwrap())
        .collect();
    assert_eq!(decoded, expected);
}

#[test]
fn traffic_designation_survives_fragmented_arch_coding() {
    use bts136::l3::{
        broadcast::ProtocolVersion,
        designation::{self, AssignedSlots, Designation, TimeAlignment},
    };
    use bts136::link::spach::SpachReceiver;
    let expected = Designation {
        dvcc: 42,
        dmac: 3,
        channel: 1,
        assigned_slots: AssignedSlots::Full14,
        shortened_burst: true,
        protocol_version: ProtocolVersion::Is136RevA,
        time_alignment: TimeAlignment::OffsetUnits(0),
        delay_interval_compensation: false,
    };
    let message = SingleMessage {
        channel: Channel::Arch,
        flags: HeaderFlags::default(),
        identity: MobileIdentity::Imsi50(1),
        message: expected.encode().unwrap(),
    };
    let frames = message.encode_fragmented().unwrap();
    assert_eq!(frames.len(), 2);
    let mut receiver = SpachReceiver::default();
    let mut recovered = None;
    for (phase, information) in frames.iter().enumerate() {
        let bits = control::encode_forward(
            SyncWord::One,
            phase as u8,
            &SharedFeedback {
                leading: [false; 12],
                trailing: [false; 10],
            },
            information,
            LogicalChannel::Spach,
            42,
        )
        .unwrap();
        let decoded = control::decode_forward(&bits, LogicalChannel::Spach, 42).unwrap();
        if let Some(message) = receiver.push(&decoded.block.information).unwrap() {
            recovered = Some(message);
        }
    }
    let recovered = recovered.unwrap();
    assert_eq!(recovered.identity, message.identity);
    assert_eq!(
        designation::decode_prefix(&recovered.message)
            .unwrap()
            .message,
        expected
    );
}
