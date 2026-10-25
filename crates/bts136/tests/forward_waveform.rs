use bts136::phy::{
    acquisition::{decode_forward_control, decode_forward_traffic},
    control::{self, INFORMATION_BITS, LogicalChannel, SharedFeedback},
    forward::ForwardModulator,
    modulation::Symbol,
    sync::SyncWord,
    traffic::{self, DATA_BITS, SACCH_BITS},
};

#[test]
fn continuous_mixed_carrier_decodes_control_and_traffic_slots_with_carrier_offset() {
    const SAMPLES_PER_SYMBOL: usize = 8;
    const SLOTS: usize = 12;
    const OFFSET_HZ: f64 = 700.0;
    let information = std::array::from_fn::<_, INFORMATION_BITS, _>(|i| i % 5 == 0);
    let traffic_data = std::array::from_fn::<_, DATA_BITS, _>(|i| i % 7 == 0);
    let mut transmitter = ForwardModulator::try_new(SAMPLES_PER_SYMBOL).unwrap();
    let mut waveform = Vec::new();
    for slot in 0..SLOTS {
        let bits = if slot.is_multiple_of(3) {
            control::encode_forward(
                SyncWord::One,
                (slot / 3) as u8,
                &SharedFeedback {
                    leading: [false; 12],
                    trailing: [false; 10],
                },
                &information,
                LogicalChannel::Fbcch,
                0,
            )
            .unwrap()
        } else {
            traffic::encode_forward(
                SyncWord::ALL[slot % 6],
                &[false; SACCH_BITS],
                &traffic_data,
                27,
                0,
            )
            .unwrap()
        };
        waveform.extend(transmitter.encode_slot(&bits));
    }
    waveform.extend(transmitter.finish());
    for (index, sample) in waveform.iter_mut().enumerate() {
        let phase =
            std::f64::consts::TAU * OFFSET_HZ * index as f64 / transmitter.sample_rate_hz() + 0.71;
        let (sin, cos) = phase.sin_cos();
        *sample = Symbol {
            i: (sample.i * cos - sample.q * sin) * 0.25,
            q: (sample.i * sin + sample.q * cos) * 0.25,
        };
    }
    let control = decode_forward_control(
        &waveform,
        transmitter.sample_rate_hz(),
        LogicalChannel::Fbcch,
        0,
    )
    .unwrap();
    assert_eq!(control.len(), 4);
    assert_eq!(
        control
            .iter()
            .map(|b| b.fields.superframe_phase)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    for burst in control {
        assert_eq!(burst.fields.block.information, information);
        assert!((burst.frequency_offset_hz - OFFSET_HZ).abs() < 100.0);
    }
    let traffic = decode_forward_traffic(&waveform, transmitter.sample_rate_hz()).unwrap();
    assert_eq!(traffic.len(), 8);
    for burst in traffic {
        assert_eq!(burst.fields.fields.data, traffic_data);
        assert_eq!(burst.fields.fields.color.dvcc, 27);
    }
}
