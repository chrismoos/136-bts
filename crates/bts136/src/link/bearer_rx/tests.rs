use super::*;
use crate::{
    link::{traffic, voice::TransmitFrame},
    phy::{sacch::SacchEncoder, sync::SyncWord, traffic as physical},
};
const DVCC: u8 = 10;
fn fields(
    previous: &TransmitFrame,
    current: &TransmitFrame,
    sacch: [bool; physical::SACCH_BITS],
) -> TrafficFields {
    let data = TransmitFrame::interleave(previous, current);
    let bits = physical::encode_forward(SyncWord::One, &sacch, &data, DVCC, 0).unwrap();
    physical::decode_forward(&bits).unwrap().fields
}
#[test]
fn mixed_bearer_recovers_facch_and_continuous_sacch_separately() {
    let mut receiver = BearerRx::with_direction(
        DVCC,
        VselpParameters::default(),
        crate::l3::relay::Direction::Forward,
    );
    let speech = TransmitFrame::speech(&VselpParameters::default());
    let mut previous = speech.clone();
    let alert = traffic::encode(&TrafficMessage::AlertWithInfo {
        information: Default::default(),
    })
    .unwrap();
    let audit = traffic::encode(&TrafficMessage::Audit).unwrap();
    let mut encoder = SacchEncoder::default();
    let mut sacch = Vec::new();
    sacch.extend(encoder.encode_word(false, &audit[0].contents, DVCC));
    for _ in 0..3 {
        sacch.extend(encoder.encode_word(false, &SignalingWord::filler().contents, DVCC));
    }
    let mut facch_messages = Vec::new();
    let mut sacch_messages = Vec::new();
    for (index, sacch) in sacch.into_iter().enumerate() {
        let current = if index == 3 {
            TransmitFrame::facch(&alert[0], DVCC)
        } else {
            speech.clone()
        };
        let received = receiver
            .push(index as u64, 0, Some(&fields(&previous, &current, sacch)))
            .unwrap();
        assert!(!received.discontinuity);
        if let Some(message) = received.facch_message.unwrap() {
            facch_messages.push(message);
        }
        if let Some(message) = received.sacch_message.unwrap() {
            sacch_messages.push(message);
        }
        previous = current;
    }
    assert_eq!(
        facch_messages,
        [TrafficMessage::AlertWithInfo {
            information: Default::default()
        }]
    );
    assert_eq!(sacch_messages, [TrafficMessage::Audit]);
}
#[test]
fn segment_changes_and_skipped_intervals_drop_fragmented_facch_then_recover() {
    for segment_change in [true, false] {
        let message = TrafficMessage::Release {
            information: crate::l3::legacy_traffic::ReleaseInformation {
                unparsed_bits: ([1; 24])
                    .iter()
                    .flat_map(|byte| {
                        (0..u8::BITS)
                            .rev()
                            .map(move |shift| byte & (1 << shift) != 0)
                    })
                    .collect(),
                ..Default::default()
            },
        };
        let words = traffic::encode(&message).unwrap();
        let mut receiver = BearerRx::with_direction(
            DVCC,
            VselpParameters::default(),
            crate::l3::relay::Direction::Forward,
        );
        let mut previous = TransmitFrame::facch(&SignalingWord::filler(), DVCC);
        let mut index = 0;
        let mut segment = 0;
        for (word_index, word) in words.iter().enumerate() {
            let current = TransmitFrame::facch(word, DVCC);
            let received = receiver
                .push(
                    index,
                    segment,
                    Some(&fields(&previous, &current, [false; physical::SACCH_BITS])),
                )
                .unwrap();
            assert_eq!(received.facch_message.unwrap(), None);
            previous = current;
            index += 1;
            if word_index == 1 {
                if segment_change {
                    segment += 1;
                } else {
                    index += 1;
                }
            }
        }
        let connect =
            TransmitFrame::facch(&traffic::encode(&TrafficMessage::Audit).unwrap()[0], DVCC);
        let filler = TransmitFrame::facch(&SignalingWord::filler(), DVCC);
        assert_eq!(
            receiver
                .push(
                    index,
                    segment,
                    Some(&fields(&previous, &connect, [false; physical::SACCH_BITS]))
                )
                .unwrap()
                .facch_message
                .unwrap(),
            None
        );
        assert_eq!(
            receiver
                .push(
                    index + 1,
                    segment,
                    Some(&fields(&connect, &filler, [false; physical::SACCH_BITS]))
                )
                .unwrap()
                .facch_message
                .unwrap(),
            Some(TrafficMessage::Audit)
        );
    }
}
#[test]
fn rejected_interval_and_segment_preserve_partial_speech() {
    let seed = VselpParameters::default();
    let speech = TransmitFrame::speech(&seed);
    let slot = fields(&speech, &speech, [false; physical::SACCH_BITS]);
    let mut receiver =
        BearerRx::with_direction(DVCC, seed.clone(), crate::l3::relay::Direction::Forward);
    assert!(receiver.push(0, 1, Some(&slot)).unwrap().frame.is_none());
    assert!(matches!(
        receiver.push(0, 1, Some(&slot)),
        Err(Error::Interval)
    ));
    assert!(matches!(
        receiver.push(1, 0, Some(&slot)),
        Err(Error::Segment)
    ));
    let frame = receiver.push(1, 1, Some(&slot)).unwrap().frame.unwrap();
    assert_eq!(frame.parameters, Some(seed));
    assert!(matches!(
        frame.kind,
        FrameKind::Speech {
            crc: CrcStatus::Valid,
            ..
        }
    ));
    assert!(receiver.push(2, 2, Some(&slot)).unwrap().frame.is_none());
}

#[test]
fn rejected_sacch_words_drop_partial_messages_and_recover_new_audit() {
    let mut receiver = BearerRx::with_direction(
        DVCC,
        VselpParameters::default(),
        crate::l3::relay::Direction::Forward,
    );
    let message = TrafficMessage::Release {
        information: crate::l3::legacy_traffic::ReleaseInformation {
            unparsed_bits: ([1; 24])
                .iter()
                .flat_map(|byte| {
                    (0..u8::BITS)
                        .rev()
                        .map(move |shift| byte & (1 << shift) != 0)
                })
                .collect(),
            ..Default::default()
        },
    };
    let words = traffic::encode(&message).unwrap();
    let audit = traffic::encode(&TrafficMessage::Audit).unwrap();
    let mut encoder = SacchEncoder::default();
    let mut sacch = Vec::new();
    for word in &words {
        sacch.extend(encoder.encode_word(word.continuation, &word.contents, DVCC));
    }
    sacch.extend(encoder.encode_word(false, &audit[0].contents, DVCC));
    for _ in 0..2 {
        sacch.extend(encoder.encode_word(false, &SignalingWord::filler().contents, DVCC));
    }
    let speech = TransmitFrame::speech(&VselpParameters::default());
    let mut messages = Vec::new();
    let mut rejected = 0;
    for (index, mut sacch) in sacch.into_iter().enumerate() {
        if (23..33).contains(&index) {
            for bit in &mut sacch {
                *bit = !*bit;
            }
        }
        let received = receiver
            .push(index as u64, 0, Some(&fields(&speech, &speech, sacch)))
            .unwrap();
        rejected += usize::from(received.sacch_error.is_some());
        if let Some(message) = received.sacch_message.unwrap() {
            messages.push(message);
        }
    }
    assert!(rejected > 0);
    assert_eq!(messages, [TrafficMessage::Audit]);
}

#[test]
fn channel_quality_reports_survive_simultaneous_facch_and_sacch() {
    use crate::l3::legacy_traffic::{ChannelQualityReport, DownlinkQuality, NeighborGroup};
    let facch_report = TrafficMessage::ChannelQuality(ChannelQualityReport::First {
        current: DownlinkQuality {
            ber_code: 1,
            rssi_code: 31,
        },
        neighbor_rssi_codes: [0, 1, 2, 3, 4, 5],
    });
    let sacch_report = TrafficMessage::ChannelQuality(ChannelQualityReport::Later {
        group: NeighborGroup::Fourth,
        neighbor_rssi_codes: [6, 7, 8, 9, 10, 11],
    });
    simultaneous_reports(
        facch_report,
        sacch_report,
        BearerRx::with_direction(DVCC, VselpParameters::default(), Direction::Reverse),
    );
}
#[test]
fn extended_reports_survive_simultaneous_facch_and_sacch_with_short_word_filler() {
    use crate::l3::legacy_traffic::{
        ExtendedChannelQualityReport, ExtendedCurrentQuality, NeighborGroup,
    };
    for group in [
        NeighborGroup::Second,
        NeighborGroup::Third,
        NeighborGroup::Fourth,
    ] {
        let first = TrafficMessage::ExtendedChannelQuality(ExtendedChannelQualityReport::First {
            current: ExtendedCurrentQuality {
                ber_code: 15,
                measurement_code: 15,
                rssi_code: 63,
            },
            neighbor_rssi_codes: [0, 1, 32, 63],
        });
        let later = TrafficMessage::ExtendedChannelQuality(ExtendedChannelQualityReport::Later {
            group,
            neighbor_rssi_codes: [0, 1, 15, 31, 32, 63],
        });
        simultaneous_reports(
            first.clone(),
            later.clone(),
            BearerRx::with_direction(DVCC, VselpParameters::default(), Direction::Reverse),
        );
        simultaneous_reports(
            later,
            first,
            BearerRx::with_direction(DVCC, VselpParameters::default(), Direction::Reverse),
        );
    }
}
#[test]
fn versioned_bearer_decodes_extended_messages_on_both_facch_and_sacch() {
    use crate::l3::{
        broadcast::ProtocolVersion,
        legacy_traffic::{ExtendedMeasurementTargets, ExtendedMeasurementType},
    };
    let ack = TrafficMessage::ExtendedMeasurementAck(
        ExtendedMeasurementTargets::try_new(ExtendedMeasurementType::WordErrorRate, vec![])
            .unwrap(),
    );
    for version in [ProtocolVersion::Pv2, ProtocolVersion::Pv6] {
        simultaneous_reports(
            ack.clone(),
            ack.clone(),
            BearerRx::with_version(
                DVCC,
                VselpParameters::default(),
                Direction::Reverse,
                version,
            ),
        );
    }
}
fn simultaneous_reports(
    facch_report: TrafficMessage,
    sacch_report: TrafficMessage,
    mut receiver: BearerRx,
) {
    let facch_word = traffic::encode(&facch_report).unwrap().remove(0);
    let sacch_word = traffic::encode(&sacch_report).unwrap().remove(0);
    let mut encoder = SacchEncoder::default();
    let mut fields_out = encoder
        .encode_word(false, &sacch_word.contents, DVCC)
        .to_vec();
    for _ in 0..3 {
        fields_out.extend(encoder.encode_word(false, &SignalingWord::filler().contents, DVCC));
    }
    let speech = TransmitFrame::speech(&VselpParameters::default());
    let mut previous = speech.clone();
    let mut facch = Vec::new();
    let mut sacch = Vec::new();
    for (index, field) in fields_out.into_iter().enumerate() {
        let current = if index == 3 {
            TransmitFrame::facch(&facch_word, DVCC)
        } else {
            speech.clone()
        };
        let result = receiver
            .push(index as u64, 0, Some(&fields(&previous, &current, field)))
            .unwrap();
        facch.extend(result.facch_message.unwrap());
        sacch.extend(result.sacch_message.unwrap());
        previous = current;
    }
    assert_eq!(facch, [facch_report]);
    assert_eq!(sacch, [sacch_report]);
}
#[test]
fn sacch_rdata_completion_can_share_a_valid_facch_frame_while_speech_continues() {
    use crate::l3::{
        legacy_traffic::TrafficRelay,
        relay::{Direction, HigherLayerProtocol, RelayMessage},
    };
    let message = TrafficMessage::Relay(TrafficRelay {
        message: RelayMessage::Data {
            transaction_id: 1,
            protocol: HigherLayerProtocol::Sms,
            payload: vec![1, 2, 3, 4, 5, 6],
        },
        optional_octets: vec![],
    });
    let words = traffic::encode(&message).unwrap();
    let completion = words.len() * crate::phy::sacch::WORD_SLOTS + physical::SACCH_BITS - 2;
    let mut encoder = SacchEncoder::default();
    let mut stream = Vec::new();
    for word in &words {
        stream.extend(encoder.encode_word(word.continuation, &word.contents, DVCC));
    }
    for _ in 0..3 {
        stream.extend(encoder.encode_word(false, &SignalingWord::filler().contents, DVCC));
    }
    let mut receiver =
        BearerRx::with_direction(DVCC, VselpParameters::default(), Direction::Forward);
    let speech = TransmitFrame::speech(&VselpParameters::default());
    let facch = TransmitFrame::facch(&traffic::encode(&TrafficMessage::Audit).unwrap()[0], DVCC);
    let mut previous = speech.clone();
    let mut received_message = None;
    let mut valid_speech = 0;
    for (interval, sacch) in stream.into_iter().enumerate() {
        let current = if interval + 1 == completion {
            facch.clone()
        } else {
            speech.clone()
        };
        let received = receiver
            .push(
                interval as u64,
                0,
                Some(&fields(&previous, &current, sacch)),
            )
            .unwrap();
        if matches!(
            received.frame.as_ref().map(|frame| &frame.kind),
            Some(FrameKind::Speech {
                crc: CrcStatus::Valid,
                ..
            })
        ) {
            valid_speech += 1;
        }
        if let Some(value) = received.sacch_message.unwrap() {
            assert_eq!(interval, completion);
            assert!(matches!(
                received.frame.as_ref().map(|frame| &frame.kind),
                Some(FrameKind::Facch(_))
            ));
            assert_eq!(received.facch_message.unwrap(), Some(TrafficMessage::Audit));
            assert!(valid_speech > 0);
            received_message = Some(value);
        }
        previous = current;
    }
    assert_eq!(received_message, Some(message));
}

#[test]
fn comfort_noise_facch_decodes_separately_from_simultaneous_sacch_quality() {
    let noise = TrafficMessage::ComfortNoise(crate::l3::legacy_traffic::ComfortNoiseParameters {
        lsf_residual_vector: 0x123456,
        energy_quantization_index: 0x9a,
        resc_parameters: 0xb,
    });
    let quality =
        TrafficMessage::ChannelQuality(crate::l3::legacy_traffic::ChannelQualityReport::First {
            current: crate::l3::legacy_traffic::DownlinkQuality {
                ber_code: 1,
                rssi_code: 2,
            },
            neighbor_rssi_codes: [0; 6],
        });
    simultaneous_reports(
        noise,
        quality,
        BearerRx::with_version(
            DVCC,
            VselpParameters::default(),
            Direction::Reverse,
            crate::l3::broadcast::ProtocolVersion::Pv2,
        ),
    );
}

#[test]
fn amr_noise_formats_decode_on_coded_facch_with_directional_context() {
    use crate::l3::legacy_traffic::{ActiveAmrMode, AmrNoiseMessage, AmrNoiseParameters};
    for command in [
        ActiveAmrMode::Mode1,
        ActiveAmrMode::Mode2,
        ActiveAmrMode::Mode3,
        ActiveAmrMode::Mode4,
    ] {
        let message = TrafficMessage::AmrNoise(AmrNoiseMessage::Parameters {
            command,
            indication: ActiveAmrMode::Mode2,
            parameters: AmrNoiseParameters {
                lsf_reference_vector: 5,
                lsf_subvectors: [2, 3, 4],
                frame_energy: 6,
            },
        });
        simultaneous_reports(
            message,
            TrafficMessage::Audit,
            BearerRx::with_version(
                DVCC,
                VselpParameters::default(),
                Direction::Forward,
                crate::l3::broadcast::ProtocolVersion::Pv5,
            ),
        );
    }
    let implicit = TrafficMessage::AmrNoise(AmrNoiseMessage::Implicit {
        data: ActiveAmrMode::Mode3,
    });
    let quality =
        TrafficMessage::ChannelQuality(crate::l3::legacy_traffic::ChannelQualityReport::First {
            current: crate::l3::legacy_traffic::DownlinkQuality {
                ber_code: 1,
                rssi_code: 2,
            },
            neighbor_rssi_codes: [0; 6],
        });
    simultaneous_reports(
        implicit,
        quality,
        BearerRx::with_version(
            DVCC,
            VselpParameters::default(),
            Direction::Reverse,
            crate::l3::broadcast::ProtocolVersion::Pv5,
        ),
    );
}

#[test]
fn all_amr_control_orders_decode_on_coded_facch_without_consuming_sacch() {
    use crate::l3::legacy_traffic::{
        ActiveAmrMode, AmrActiveSet, AmrConfiguration, AmrControlMessage, AmrFreeze, AmrLink,
        AmrPhase, AmrThresholds,
    };
    for message in [
        AmrControlMessage::Configuration(AmrConfiguration {
            countdown: true,
            link: AmrLink::Both,
            initial_mode: ActiveAmrMode::Mode2,
            active_set: AmrActiveSet::try_new(0x33).unwrap(),
            thresholds: [12, 23, 34],
            common_hysteresis: 5,
        }),
        AmrControlMessage::Thresholds(AmrThresholds {
            lower: [4, 17, 50],
            hysteresis: [3, 9, 12],
        }),
        AmrControlMessage::PhaseFreeze {
            phase: AmrPhase::Maintain,
            freeze: AmrFreeze::Unfreeze,
            mode: ActiveAmrMode::Mode4,
        },
    ] {
        simultaneous_reports(
            TrafficMessage::AmrControl(message),
            TrafficMessage::Audit,
            BearerRx::with_version(
                DVCC,
                VselpParameters::default(),
                Direction::Forward,
                crate::l3::broadcast::ProtocolVersion::Pv5,
            ),
        );
    }
}

#[test]
fn partial_facch_and_sacch_messages_hold_receive_assembly_until_reset() {
    let message = TrafficMessage::Release {
        information: crate::l3::legacy_traffic::ReleaseInformation {
            unparsed_bits: vec![true; 160],
            ..Default::default()
        },
    };
    let words = traffic::encode(&message).unwrap();
    assert!(words.len() > 1);
    let speech = TransmitFrame::speech(&VselpParameters::default());
    let mut receiver =
        BearerRx::with_direction(DVCC, VselpParameters::default(), Direction::Forward);
    let first = TransmitFrame::facch(&words[0], DVCC);
    receiver
        .push(
            0,
            0,
            Some(&fields(&speech, &first, [false; physical::SACCH_BITS])),
        )
        .unwrap();
    assert!(!receiver.has_pending_signaling());
    receiver
        .push(
            1,
            0,
            Some(&fields(&first, &speech, [false; physical::SACCH_BITS])),
        )
        .unwrap();
    assert!(receiver.has_pending_signaling());
    receiver.discontinuity();
    assert!(!receiver.has_pending_signaling());
    let mut receiver =
        BearerRx::with_direction(DVCC, VselpParameters::default(), Direction::Forward);
    let mut encoder = SacchEncoder::default();
    let mut samples = Vec::new();
    samples.extend(encoder.encode_word(false, &words[0].contents, DVCC));
    for _ in 0..3 {
        samples.extend(encoder.encode_word(false, &SignalingWord::filler().contents, DVCC));
    }
    let mut retained = false;
    for (index, sacch) in samples.into_iter().enumerate() {
        receiver
            .push(index as u64, 0, Some(&fields(&speech, &speech, sacch)))
            .unwrap();
        if receiver.has_pending_signaling() {
            retained = true;
            break;
        }
    }
    assert!(retained);
    receiver.discontinuity();
    assert!(!receiver.has_pending_signaling());
}
