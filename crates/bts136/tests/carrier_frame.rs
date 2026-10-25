use bts136::{
    codec::vselp::{SUBFRAMES, VselpParameters, VselpSubframe, decoder::Decoder},
    l3::{designation::AssignedSlots, legacy_traffic::TrafficMessage},
    link::{
        bearer_rx::BearerRx,
        traffic::SignalingWord,
        traffic_tx::TrafficTx,
        voice::{FrameKind, TransmitFrame},
    },
    phy::{
        acquisition::decode_forward_traffic, forward::ForwardModulator, speech::CrcStatus,
        sync::SyncWord, traffic,
    },
    scheduler::frame::{CarrierLayout, FrameClock, SlotRole},
};
const SAMPLE_RATE_HZ: u64 = 194400;
const SAMPLES_PER_SYMBOL: usize = 8;
const DVCCS: [u8; 2] = [10, 42];
fn parameters(r0: u8, code: u8) -> VselpParameters {
    VselpParameters::try_new(
        r0,
        [32, 16, 16, 8, 8, 4, 4, 4, 4, 2],
        [VselpSubframe::try_new(21, code, code ^ 0x7f, 170).unwrap(); SUBFRAMES],
    )
    .unwrap()
}
#[test]
fn six_slot_carrier_preserves_bearer_interleavers_sync_identities_and_nominal_completion_times() {
    let clock = FrameClock::try_new(1000, 1000003, SAMPLE_RATE_HZ).unwrap();
    let layout =
        CarrierLayout::try_new(&[], &[AssignedSlots::Full14, AssignedSlots::Full25]).unwrap();
    let mut transmitters = [
        TrafficTx::try_new(DVCCS[0], 4, 0).unwrap(),
        TrafficTx::try_new(DVCCS[1], 4, 0).unwrap(),
    ];
    let messages = [
        TrafficMessage::AlertWithInfo {
            information: Default::default(),
        },
        TrafficMessage::Release {
            information: Default::default(),
        },
    ];
    let media = [parameters(20, 37), parameters(21, 54)];
    let commands = [
        transmitters[0].enqueue(&messages[0]).unwrap(),
        transmitters[1].enqueue(&messages[1]).unwrap(),
    ];
    let filler = TransmitFrame::facch(&SignalingWord::filler(), 0);
    let unused = TransmitFrame::interleave(&filler, &filler);
    let mut modulator = ForwardModulator::try_new(SAMPLES_PER_SYMBOL).unwrap();
    let mut samples = Vec::new();
    let mut plans = Vec::new();
    let mut completions = Vec::new();
    for slot_index in 0..12 {
        let planned = layout.slot(&clock, slot_index).unwrap();
        assert_eq!(planned.timing.first_sample, samples.len() as u64);
        let (sync, data, dvcc) = match planned.role {
            SlotRole::Traffic {
                bearer,
                interval,
                sync,
            } => {
                let tx = &mut transmitters[bearer];
                if interval == 2 {
                    tx.offer_media(interval, TransmitFrame::speech(&media[bearer]))
                        .unwrap();
                }
                let prepared = tx.prepare(interval).unwrap().clone();
                let data = *prepared.data();
                if let Some(command) = tx.transmitted(&prepared).unwrap() {
                    assert_eq!(command, commands[bearer]);
                    completions.push((bearer, planned.timing.end_tick));
                }
                (sync, data, DVCCS[bearer])
            }
            SlotRole::Unassigned { sync } => (sync, unused, 0),
            SlotRole::Reserved => {
                panic!("no reserved control slot in this generated traffic carrier")
            }
        };
        let bits =
            traffic::encode_forward(sync, &[false; traffic::SACCH_BITS], &data, dvcc, 0).unwrap();
        samples.extend(modulator.encode_slot(&bits));
        plans.push(planned);
    }
    assert_eq!(completions, [(0, 27667), (1, 34334)]);
    let group_delay = modulator.delay_samples() as f64;
    let nearest_symbol_limit_samples = SAMPLES_PER_SYMBOL as f64 / 2.0;
    samples.extend(modulator.finish());
    let acquired = decode_forward_traffic(&samples, modulator.sample_rate_hz()).unwrap();
    assert_eq!(acquired.len(), 12);
    let mut receivers = [
        BearerRx::with_direction(
            DVCCS[0],
            VselpParameters::default(),
            bts136::l3::relay::Direction::Forward,
        ),
        BearerRx::with_direction(
            DVCCS[1],
            VselpParameters::default(),
            bts136::l3::relay::Direction::Forward,
        ),
    ];
    let mut decoders = [
        Decoder::new(VselpParameters::default()),
        Decoder::new(VselpParameters::default()),
    ];
    let mut decoded_messages = [Vec::new(), Vec::new()];
    let mut speech_counts = [0, 0];
    for (planned, burst) in plans.iter().zip(&acquired) {
        let sync = match planned.role {
            SlotRole::Traffic { sync, .. } | SlotRole::Unassigned { sync } => sync,
            SlotRole::Reserved => unreachable!(),
        };
        assert_eq!(burst.fields.fields.sync, sync);
        assert!(
            (burst.slot_start_sample - planned.timing.first_sample as f64 - group_delay).abs()
                < nearest_symbol_limit_samples,
            "slot={} acquired={} nominal={} filter_delay={}",
            planned.timing.slot_index,
            burst.slot_start_sample,
            planned.timing.first_sample,
            group_delay
        );
        if let SlotRole::Traffic {
            bearer, interval, ..
        } = planned.role
        {
            assert_eq!(burst.fields.fields.color.dvcc, DVCCS[bearer]);
            let received = receivers[bearer]
                .push(interval, 0, Some(&burst.fields.fields))
                .unwrap();
            if let Some(message) = received.facch_message.unwrap() {
                decoded_messages[bearer].push(message);
            }
            if let Some(frame) = received.frame {
                match frame.kind {
                    FrameKind::Facch(_) => {
                        assert_eq!(
                            frame.samples,
                            decoders[bearer].conceal_missing_frame().unwrap()
                        );
                    }
                    FrameKind::Speech {
                        crc: CrcStatus::Valid,
                        ..
                    } => {
                        assert_eq!(frame.parameters, Some(media[bearer].clone()));
                        assert_eq!(
                            frame.samples,
                            decoders[bearer].decode(&media[bearer]).unwrap()
                        );
                        speech_counts[bearer] += 1;
                    }
                    other => panic!("unexpected bearer frame: {other:?}"),
                }
            }
        }
    }
    assert_eq!(
        decoded_messages,
        [vec![messages[0].clone()], vec![messages[1].clone()]]
    );
    assert_eq!(speech_counts, [1, 1]);
    assert_eq!(acquired[5].fields.fields.sync, SyncWord::Six);
}
