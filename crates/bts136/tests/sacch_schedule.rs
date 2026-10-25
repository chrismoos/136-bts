use bts136::{
    codec::vselp::{SUBFRAMES, VselpParameters, VselpSubframe},
    l3::{designation::AssignedSlots, legacy_traffic::TrafficMessage},
    link::{
        bearer_tx::{BearerTx, QueueLimits},
        traffic::{SignalingWord, TrafficMessageReceiver},
        voice::{FrameKind, TrafficFrameReceiver, TransmitFrame},
    },
    phy::{
        acquisition::decode_forward_traffic, forward::ForwardModulator, sacch::SacchReceiver,
        speech::CrcStatus, traffic as traffic_phy,
    },
    scheduler::frame::{CarrierLayout, FrameClock, SlotRole},
};
const DVCC: u8 = 42;
const ASSIGNED_INTERVALS: u64 = 44;
#[test]
fn audit_sacch_continues_during_facch_and_speech_on_a_full_rate_generated_carrier() {
    let clock = FrameClock::try_new(0, 1000000, 194400).unwrap();
    let layout = CarrierLayout::try_new(&[], &[AssignedSlots::Full14]).unwrap();
    let mut tx = BearerTx::try_new(
        DVCC,
        0,
        QueueLimits {
            facch_words: 4,
            sacch_words: 4,
        },
        0,
    )
    .unwrap();
    let audit = tx.enqueue_sacch(&TrafficMessage::Audit).unwrap();
    let alert = TrafficMessage::AlertWithInfo {
        information: Default::default(),
    };
    let mut alert_command = None;
    let speech = VselpParameters::try_new(
        20,
        [32, 16, 16, 8, 8, 4, 4, 4, 4, 2],
        [VselpSubframe::try_new(21, 37, 54, 170).unwrap(); SUBFRAMES],
    )
    .unwrap();
    let filler = TransmitFrame::facch(&SignalingWord::filler(), 0);
    let unassigned = TransmitFrame::interleave(&filler, &filler);
    let mut modulator = ForwardModulator::try_new(8).unwrap();
    let mut samples = Vec::new();
    let mut plans = Vec::new();
    let mut completions = Vec::new();
    for slot_index in 0..ASSIGNED_INTERVALS * 3 {
        let planned = layout.slot(&clock, slot_index).unwrap();
        let bits = match planned.role {
            SlotRole::Traffic { interval, sync, .. } => {
                if interval == 5 {
                    alert_command = Some(tx.enqueue_facch(&alert).unwrap());
                }
                tx.offer_media(interval, TransmitFrame::speech(&speech))
                    .unwrap();
                let burst = tx.prepare(interval, sync).unwrap().clone();
                let bits = *burst.bits();
                let completion = tx.transmitted(&burst).unwrap();
                if let Some(completed) = completion.facch {
                    assert_eq!(Some(completed), alert_command);
                    completions.push(("FACCH", interval));
                }
                if let Some(completed) = completion.sacch {
                    assert_eq!(completed, audit);
                    completions.push(("SACCH", interval));
                }
                bits
            }
            SlotRole::Unassigned { sync } => traffic_phy::encode_forward(
                sync,
                &[false; traffic_phy::SACCH_BITS],
                &unassigned,
                0,
                0,
            )
            .unwrap(),
            SlotRole::Reserved => panic!("generated carrier has no control reservation"),
        };
        samples.extend(modulator.encode_slot(&bits));
        plans.push(planned);
    }
    assert_eq!(completions, [("FACCH", 6), ("SACCH", 21)]);
    samples.extend(modulator.finish());
    let acquired = decode_forward_traffic(&samples, modulator.sample_rate_hz()).unwrap();
    assert_eq!(acquired.len() as u64, ASSIGNED_INTERVALS * 3);
    let mut sacch = SacchReceiver::default();
    let mut slow = TrafficMessageReceiver::new(bts136::l3::relay::Direction::Forward);
    let mut fast = TrafficMessageReceiver::new(bts136::l3::relay::Direction::Forward);
    let mut receiver = TrafficFrameReceiver::new(DVCC, VselpParameters::default());
    let mut slow_messages = Vec::new();
    let mut fast_messages = Vec::new();
    let mut speech_frames = 0;
    for (planned, burst) in plans.iter().zip(&acquired) {
        if let SlotRole::Traffic { sync, .. } = planned.role {
            assert_eq!(burst.fields.fields.sync, sync);
            if let Some(word) = sacch.push(burst.fields.fields.sacch, DVCC)
                && let Some(message) = slow.push(&SignalingWord::from(&word)).unwrap()
            {
                slow_messages.push(message);
            }
            if let Some(frame) = receiver.push(Some(&burst.fields.fields.data)).unwrap() {
                match frame.kind {
                    FrameKind::Facch(word) => {
                        if let Some(message) = fast.push(&SignalingWord::from(&word)).unwrap() {
                            fast_messages.push(message);
                        }
                    }
                    FrameKind::Speech {
                        crc: CrcStatus::Valid,
                        ..
                    } => {
                        assert_eq!(frame.parameters, Some(speech.clone()));
                        speech_frames += 1;
                    }
                    other => panic!("unexpected traffic frame: {other:?}"),
                }
            }
        }
    }
    assert_eq!(slow_messages, [TrafficMessage::Audit]);
    assert_eq!(fast_messages, [alert]);
    assert_eq!(speech_frames, 42);
}
