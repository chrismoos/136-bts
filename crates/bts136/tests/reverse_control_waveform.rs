use bts136::phy::{
    acquisition::decode_reverse_control,
    dcch::BlockFormat,
    modulation::{Dibit, DifferentialEncoder, Symbol},
    pulse::{RootRaisedCosine, SYMBOL_RATE_HZ},
    rach::{self, BurstFormat},
    sync::SyncWord,
    traffic::SLOT_BITS,
};

const SAMPLES_PER_SYMBOL: usize = 8;
const HALF_FILTER_SPAN_SYMBOLS: usize = 8;
const GUARD_SYMBOLS: usize = 3;
const RAMP_SYMBOLS: usize = 3;
const PREAMBLE_START_SYMBOL: usize = GUARD_SYMBOLS + RAMP_SYMBOLS;
const ABBREVIATED_GUARD_SYMBOLS: usize = 22;
const FREQUENCY_OFFSET_HZ: f64 = 1100.0;
const DVCC: u8 = 42;
const ZERO: Symbol = Symbol { i: 0.0, q: 0.0 };

fn waveform(entries: &[(BurstFormat, SyncWord, Vec<bool>)]) -> Vec<Symbol> {
    waveform_at(entries, SAMPLES_PER_SYMBOL)
}

fn waveform_at(
    entries: &[(BurstFormat, SyncWord, Vec<bool>)],
    samples_per_symbol: usize,
) -> Vec<Symbol> {
    let mut symbols = vec![ZERO; SLOT_BITS / 2];
    for (format, sync, information) in entries {
        let format = *format;
        let sync = *sync;
        let bits = rach::encode(format, sync, information, DVCC).unwrap();
        let end = match format {
            BurstFormat::Normal => SLOT_BITS / 2,
            BurstFormat::Abbreviated => SLOT_BITS / 2 - ABBREVIATED_GUARD_SYMBOLS,
        };
        let mut encoder = DifferentialEncoder::default();
        let mut burst = vec![ZERO; SLOT_BITS / 2];
        for ramp in 0..RAMP_SYMBOLS {
            burst[GUARD_SYMBOLS + ramp] = Symbol {
                i: (ramp + 1) as f64 / RAMP_SYMBOLS as f64,
                q: 0.0,
            };
        }
        for symbol in PREAMBLE_START_SYMBOL..end {
            burst[symbol] =
                encoder.encode(Dibit::from_bits([bits[symbol * 2], bits[symbol * 2 + 1]]));
        }
        symbols.extend(burst);
        symbols.extend([ZERO; SLOT_BITS / 2]);
    }
    let sample_rate_hz = SYMBOL_RATE_HZ * samples_per_symbol as f64;
    let filter = RootRaisedCosine::try_new(sample_rate_hz, HALF_FILTER_SPAN_SYMBOLS).unwrap();
    let mut impulses = vec![ZERO; symbols.len() * samples_per_symbol + filter.delay_samples() * 2];
    for (index, symbol) in symbols.into_iter().enumerate() {
        impulses[index * samples_per_symbol] = symbol;
    }
    let mut samples = filter.filter(&impulses);
    for (index, sample) in samples.iter_mut().enumerate() {
        let phase =
            std::f64::consts::TAU * FREQUENCY_OFFSET_HZ * index as f64 / sample_rate_hz + 0.73;
        let (sin, cos) = phase.sin_cos();
        *sample = Symbol {
            i: (sample.i * cos - sample.q * sin) * 0.2,
            q: (sample.i * sin + sample.q * cos) * 0.2,
        };
    }
    samples
}

#[test]
fn mixed_reverse_control_bursts_decode_with_silent_guards_and_carrier_offset() {
    let mut expected = Vec::new();
    for (index, (format, sync)) in [
        (BurstFormat::Normal, SyncWord::One),
        (BurstFormat::Abbreviated, SyncWord::Two),
        (BurstFormat::Normal, SyncWord::Three),
        (BurstFormat::Abbreviated, SyncWord::Four),
    ]
    .into_iter()
    .enumerate()
    {
        let block = match format {
            BurstFormat::Normal => BlockFormat::ReverseNormal,
            BurstFormat::Abbreviated => BlockFormat::ReverseAbbreviated,
        };
        let information: Vec<_> = (0..block.information_bits())
            .map(|bit| (bit + index).is_multiple_of(3))
            .collect();
        expected.push((format, sync, information));
    }
    let sample_rate_hz = SYMBOL_RATE_HZ * SAMPLES_PER_SYMBOL as f64;
    let samples = waveform(&expected);
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let bursts = decode_reverse_control(&samples, sample_rate_hz, format, DVCC).unwrap();
        assert_eq!(bursts.len(), 2);
        for read_size in [1, 7, 97, 1000] {
            let mut stream =
                bts136::phy::acquisition::reverse_stream::ReverseControlStream::try_new_control(
                    sample_rate_hz,
                    16,
                    format,
                    DVCC,
                )
                .unwrap();
            let mut output = Vec::with_capacity(16);
            let mut streamed = Vec::new();
            for (index, samples) in samples.chunks(read_size).enumerate() {
                stream
                    .push(0, (index * read_size) as u64, samples, &mut output)
                    .unwrap();
                streamed.append(&mut output);
            }
            stream.finish(&mut output).unwrap();
            streamed.append(&mut output);
            assert_eq!(streamed.len(), bursts.len());
            for (actual, expected) in streamed.iter().zip(&bursts) {
                assert_eq!(actual.acquired.fields, expected.fields);
                assert_eq!(actual.acquired.hard_bits, expected.hard_bits);
            }
        }

        let expected: Vec<_> = expected.iter().filter(|entry| entry.0 == format).collect();
        for (burst, expected) in bursts.iter().zip(expected) {
            assert_eq!(burst.fields.format, expected.0);
            assert_eq!(burst.fields.sync, expected.1);
            assert_eq!(burst.fields.block.information, expected.2);
            assert_eq!(burst.fields.block.corrected_coded_bits, 0);
            assert_eq!(burst.fields.additional_sync_errors, 0);
            assert_eq!(
                burst.hard_bits[..PREAMBLE_START_SYMBOL * 2],
                [false; PREAMBLE_START_SYMBOL * 2]
            );
            assert!((burst.frequency_offset_hz - FREQUENCY_OFFSET_HZ).abs() < 100.0);
        }
        let mut wrong_cell =
            bts136::phy::acquisition::reverse_stream::ReverseControlStream::try_new_control(
                sample_rate_hz,
                16,
                format,
                DVCC + 1,
            )
            .unwrap();
        let mut rejected = Vec::with_capacity(16);
        for (index, samples) in samples.chunks(97).enumerate() {
            wrong_cell
                .push(0, (index * 97) as u64, samples, &mut rejected)
                .unwrap();
            assert!(rejected.is_empty());
        }
        wrong_cell.finish(&mut rejected).unwrap();
        assert!(rejected.is_empty());
        assert!(
            decode_reverse_control(&samples, sample_rate_hz, format, DVCC + 1)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn registration_and_serial_number_reassemble_from_split_access_iq_in_both_formats() {
    use bts136::{
        core::registry::{LocalRegistry, ProvisionedSubscriber},
        l3::{
            broadcast::ProtocolVersion,
            registration::{self, AccessMessage, RegistrationType},
        },
        link::rach::{self as link, MobileIdentity, Outcome},
        radio::{
            access_rx::AccessRx,
            receiver::{Block, Continuity},
        },
    };
    let identity = MobileIdentity::Min34(0x123456789);
    let expected = [
        AccessMessage::Registration {
            registration_type: RegistrationType::PowerUp,
            scm: 21,
            protocol_version: ProtocolVersion::Is136RevA,
        },
        AccessMessage::SerialNumber { esn: 0x12345678 },
    ];
    let messages: Vec<_> = expected
        .iter()
        .map(|message| message.encode().unwrap())
        .collect();
    let references: Vec<_> = messages.iter().map(Vec::as_slice).collect();
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let entries: Vec<_> = link::encode(format, identity, &references)
            .unwrap()
            .into_iter()
            .map(|frame| (format, SyncWord::One, frame))
            .collect();
        let samples = waveform(&entries);
        let mut access =
            AccessRx::try_new(SYMBOL_RATE_HZ * SAMPLES_PER_SYMBOL as f64, format, DVCC, 16)
                .unwrap();
        let iq: Vec<_> = samples
            .iter()
            .map(|sample| num_complex::Complex32::new(sample.i as f32, sample.q as f32))
            .collect();
        let mut events = Vec::with_capacity(16);
        let mut completed = Vec::new();
        let mut count = 0;
        for (index, iq) in iq.chunks(97).enumerate() {
            let first_sample = (index * 97) as u64;
            let block = Block {
                samples_read: iq.len(),
                time_ticks: first_sample,
                first_sample,
                segment: 0,
                continuity: if index == 0 {
                    Continuity::First
                } else {
                    Continuity::Continuous
                },
                hardware_overflow: false,
            };
            access.process(block, iq, &mut events).unwrap();
            count += events.len();
            for event in events.drain(..) {
                if let Outcome::Complete(set) = event.outcome.unwrap() {
                    completed.push(set);
                }
            }
        }
        access.finish(&mut events).unwrap();
        count += events.len();
        for event in events {
            if let Outcome::Complete(set) = event.outcome.unwrap() {
                completed.push(set);
            }
        }
        assert_eq!(count, entries.len());
        assert_eq!(completed.len(), 1);
        let set = &completed[0];
        assert_eq!(set.identity, identity);
        assert_eq!(set.messages.len(), expected.len());
        for (bytes, expected) in set.messages.iter().zip(&expected) {
            assert_eq!(
                &registration::decode_prefix(bytes).unwrap().message,
                expected
            );
        }
        let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
            identity,
            number: "1001".into(),
        }])
        .unwrap();
        let reply = registry.register(set, 100).unwrap();
        assert_eq!(reply.identity, identity);
        assert!(registry.by_number("1001").unwrap().registration.is_some());
    }
}

#[test]
fn access_rx_segment_change_prevents_fragment_completion_then_recovers_fresh_attempt() {
    use bts136::{
        link::rach::{self as link, MobileIdentity, Outcome},
        radio::{
            access_rx::{AccessRx, Error},
            receiver::{Block, Continuity},
        },
    };
    use num_complex::Complex32;
    let identity = MobileIdentity::Min34(0x123456789);
    let message = vec![1u8; 60];
    for format in [BurstFormat::Normal, BurstFormat::Abbreviated] {
        let entries: Vec<_> = link::encode(format, identity, &[&message])
            .unwrap()
            .into_iter()
            .map(|frame| (format, SyncWord::One, frame))
            .collect();
        let samples = waveform(&entries);
        let iq: Vec<_> = samples
            .iter()
            .map(|sample| Complex32::new(sample.i as f32, sample.q as f32))
            .collect();
        let mut access =
            AccessRx::try_new(SYMBOL_RATE_HZ * SAMPLES_PER_SYMBOL as f64, format, DVCC, 16)
                .unwrap();
        let mut events = Vec::with_capacity(16);
        let mut segment = 0;
        let mut broke_attempt = false;
        let mut completed = Vec::new();
        for (index, iq) in iq.chunks(97).enumerate() {
            let first_sample = (index * 97) as u64;
            let block = Block {
                samples_read: iq.len(),
                time_ticks: first_sample,
                first_sample,
                segment,
                continuity: if index == 0 {
                    Continuity::First
                } else {
                    Continuity::Continuous
                },
                hardware_overflow: false,
            };
            if index == 0 {
                let mut bad = block;
                bad.samples_read += 1;
                assert!(matches!(
                    access.process(bad, iq, &mut events),
                    Err(Error::SampleCount { .. })
                ));
                assert!(matches!(
                    access.process(block, iq, &mut Vec::new()),
                    Err(Error::OutputCapacity)
                ));
            }
            access.process(block, iq, &mut events).unwrap();
            for event in events.drain(..) {
                match event.outcome {
                    Ok(Outcome::Pending(_)) if !broke_attempt => {
                        segment = 1;
                        broke_attempt = true;
                    }
                    Ok(Outcome::Complete(set)) => completed.push(set),
                    _ => {}
                }
            }
        }
        access.finish(&mut events).unwrap();
        for event in events.drain(..) {
            if let Ok(Outcome::Complete(set)) = event.outcome {
                completed.push(set);
            }
        }
        assert!(broke_attempt);
        assert!(completed.is_empty());
        access.discontinuity();
        for (index, iq) in iq.chunks(97).enumerate() {
            let first_sample = (index * 97) as u64;
            let block = Block {
                samples_read: iq.len(),
                time_ticks: first_sample,
                first_sample,
                segment: 2,
                continuity: if index == 0 {
                    Continuity::First
                } else {
                    Continuity::Continuous
                },
                hardware_overflow: false,
            };
            access.process(block, iq, &mut events).unwrap();
            for event in events.drain(..) {
                if let Outcome::Complete(set) = event.outcome.unwrap() {
                    completed.push(set);
                }
            }
        }
        access.finish(&mut events).unwrap();
        for event in events {
            if let Outcome::Complete(set) = event.outcome.unwrap() {
                completed.push(set);
            }
        }
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].identity, identity);
        assert_eq!(
            completed[0].messages.as_slice(),
            std::slice::from_ref(&message)
        );
    }
}

#[test]
fn reverse_quality_is_gain_invariant_and_reports_relative_received_level() {
    let bits = vec![false; BlockFormat::ReverseNormal.information_bits()];
    let samples = waveform(&[(BurstFormat::Normal, SyncWord::One, bits)]);
    let rate = SYMBOL_RATE_HZ * SAMPLES_PER_SYMBOL as f64;
    let original = decode_reverse_control(&samples, rate, BurstFormat::Normal, DVCC).unwrap();
    let quieter: Vec<_> = samples
        .iter()
        .map(|s| Symbol {
            i: s.i * 0.5,
            q: s.q * 0.5,
        })
        .collect();
    let scaled = decode_reverse_control(&quieter, rate, BurstFormat::Normal, DVCC).unwrap();
    assert_eq!(original.len(), 1);
    assert_eq!(scaled.len(), 1);
    let first = original[0].quality.unwrap();
    let second = scaled[0].quality.unwrap();
    assert!(first.sync_evm_percent < 10.0, "{first:?}");
    assert!((first.sync_evm_percent - second.sync_evm_percent).abs() < 1e-8);
    assert!((first.sync_level_dbfs - second.sync_level_dbfs - 20.0 * 2.0_f64.log10()).abs() < 1e-8);
}

#[test]
fn reverse_control_bursts_decode_at_doubled_oversampling() {
    let samples_per_symbol = 2 * bts136::config::SAMPLES_PER_SYMBOL;
    let information: Vec<_> = (0..BlockFormat::ReverseNormal.information_bits())
        .map(|bit| bit.is_multiple_of(5))
        .collect();
    let samples = waveform_at(
        &[(BurstFormat::Normal, SyncWord::One, information.clone())],
        samples_per_symbol,
    );
    let sample_rate_hz = SYMBOL_RATE_HZ * samples_per_symbol as f64;
    let bursts =
        decode_reverse_control(&samples, sample_rate_hz, BurstFormat::Normal, DVCC).unwrap();
    assert_eq!(bursts.len(), 1);
    assert_eq!(bursts[0].fields.block.information, information);
}
