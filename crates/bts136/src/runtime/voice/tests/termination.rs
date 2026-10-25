use super::*;
use crate::l3::{
    legacy_traffic::{ControlChannel, Hyperband, ReleaseFields, ReleaseInformation, ReleaseReason},
    relay::Direction,
};
#[test]
fn return_channel_ack_requires_both_facch_words_and_their_final_interleaved_half_before_stop() {
    let (mut voice, registry) = setup();
    let target = ControlChannel {
        hyperband: Hyperband::Cellular800,
        channel: 1,
        dvcc: 1,
    };
    voice.set_return_channel(target).unwrap();
    assigned(&mut voice, &registry);
    assert!(voice.set_return_channel(target).is_err());
    let silent = TransmitFrame::speech(&VselpParameters::default());
    let data = TransmitFrame::interleave(&silent, &silent);
    for party in [Party::Caller, Party::Callee] {
        receive(&mut voice, party, 1, &data, 50);
    }
    let active = voice.active_mut().unwrap();
    let caller = active.legs[index(Party::Caller)].assignment.handle.clone();
    active
        .session
        .receive(
            &caller,
            &TrafficMessage::Release {
                information: ReleaseInformation {
                    fields: ReleaseFields::Reason(ReleaseReason::PowerDown),
                    ..Default::default()
                },
            },
            60,
        )
        .unwrap();
    let expected = active
        .session
        .response(Party::Caller)
        .unwrap()
        .message()
        .clone();
    let word_count = crate::link::traffic::encode(&expected).unwrap().len();
    assert_eq!(word_count, 2);
    voice.advance(61).unwrap();
    let first_interval = voice.active().unwrap().legs[index(Party::Caller)].next_tx;
    let mut bursts = Vec::new();
    let mut decoder = crate::link::traffic::TrafficMessageReceiver::new(Direction::Forward);
    let mut messages = Vec::new();
    for offset in 0..=word_count {
        assert!(
            voice
                .active()
                .unwrap()
                .session
                .stop_request(Party::Caller)
                .is_none()
        );
        let slot = physical(&voice, Party::Caller, first_interval + offset as u64);
        let (_, party, prepared) = voice.traffic(slot).unwrap().unwrap();
        assert_eq!(party, Party::Caller);
        bursts.push(
            crate::phy::traffic::decode_forward(prepared.bits())
                .unwrap()
                .fields
                .data,
        );
        let command = voice
            .traffic_submitted(voice.call_id().unwrap(), party, &prepared)
            .unwrap();
        if offset < word_count {
            assert!(command.facch.is_none());
        } else {
            assert!(command.facch.is_some());
        }
        voice
            .transmitted(
                Completion::Traffic {
                    party,
                    call_id: voice.call_id().unwrap(),
                    command,
                },
                70 + offset as u64,
            )
            .unwrap();
        if bursts.len() >= 2 {
            let fields =
                crate::phy::facch::deinterleave(&bursts[bursts.len() - 2], bursts.last().unwrap());
            let word = crate::phy::facch::decode(&fields, 1).unwrap();
            if let Some(message) = decoder
                .push(&crate::link::traffic::SignalingWord::from(&word))
                .unwrap()
            {
                messages.push(message);
            }
        }
    }
    assert_eq!(messages, vec![expected]);
    assert!(
        voice
            .active()
            .unwrap()
            .session
            .stop_request(Party::Caller)
            .is_some()
    );
    assert_eq!(
        voice
            .active()
            .unwrap()
            .session
            .release_reason(Party::Caller),
        Some(ReleaseReason::PowerDown)
    );
    assert!(voice.active().unwrap().session.stopped_handle().is_none());
}
