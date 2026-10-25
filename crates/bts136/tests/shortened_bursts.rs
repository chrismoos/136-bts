use bts136::phy::{
    acquisition::{AcquisitionWorkspace, reverse_stream::ReverseStream},
    modulation::{Dibit, DifferentialEncoder, Symbol},
    pulse::{RootRaisedCosine, SYMBOL_RATE_HZ},
    shortened::{self, ShortenedBurst},
    sync::SyncWord,
};
const SLOT_SYMBOLS: usize = 162;
const FILTER_SPAN: usize = 8;
const PREFIX_SYMBOLS: usize = 12;
const DVCC: u8 = 42;
const CFO_HZ: f64 = 350.0;

fn waveform(samples_per_symbol: usize, syncs: &[usize]) -> Vec<Symbol> {
    let bits = shortened::encode(SyncWord::Two, DVCC).unwrap();
    let mut encoder = DifferentialEncoder::default();
    let mut impulses =
        vec![Symbol { i: 0.0, q: 0.0 }; (SLOT_SYMBOLS * 3 + PREFIX_SYMBOLS) * samples_per_symbol];
    for symbol in 6..shortened::ACTIVE_END_SYMBOL {
        let value = encoder.encode(Dibit::from_bits([bits[symbol * 2], bits[symbol * 2 + 1]]));
        let erased = shortened::SYNC_OFFSETS
            .iter()
            .enumerate()
            .any(|(index, start)| {
                !syncs.contains(&index) && (*start..*start + 14).contains(&symbol)
            });
        if !erased {
            impulses[(PREFIX_SYMBOLS + symbol) * samples_per_symbol] = value;
        }
    }
    let rate = SYMBOL_RATE_HZ * samples_per_symbol as f64;
    let pulse = RootRaisedCosine::try_new(rate, FILTER_SPAN).unwrap();
    pulse
        .filter(&impulses)
        .into_iter()
        .enumerate()
        .map(|(index, symbol)| {
            let phase = std::f64::consts::TAU * CFO_HZ * index as f64 / rate;
            let (sin, cos) = phase.sin_cos();
            Symbol {
                i: symbol.i * cos - symbol.q * sin,
                q: symbol.i * sin + symbol.q * cos,
            }
        })
        .collect()
}

#[test]
fn shortened_acquisition_recovers_the_origin_with_the_first_four_syncs_erased() {
    for samples_per_symbol in [4, 16] {
        let samples = waveform(samples_per_symbol, &[4, 5]);
        let mut workspace = AcquisitionWorkspace::try_new(
            SYMBOL_RATE_HZ * samples_per_symbol as f64,
            samples.len(),
        )
        .unwrap();
        let mut found = Vec::with_capacity(16);
        workspace.shortened(&samples, &mut found).unwrap();
        assert_eq!(found.len(), 1, "sps={samples_per_symbol}: {found:?}");
        let burst = &found[0];
        assert_eq!(burst.fields.sync, SyncWord::Two);
        assert_eq!(burst.fields.color.dvcc, DVCC);
        assert_eq!(burst.fields.sync_words, 2);
        assert!(
            (burst.slot_start_sample
                - ((PREFIX_SYMBOLS + FILTER_SPAN) * samples_per_symbol) as f64)
                .abs()
                <= samples_per_symbol as f64 / 4.0
        );
        assert!((burst.frequency_offset_hz - CFO_HZ).abs() < 30.0);
    }
}

#[test]
fn shortened_stream_preserves_origin_and_segment_across_arbitrary_chunks() {
    let samples_per_symbol = 4;
    let samples = waveform(samples_per_symbol, &[0, 1, 2, 3, 4, 5]);
    let mut stream = ReverseStream::<ShortenedBurst>::try_new_shortened(
        SYMBOL_RATE_HZ * samples_per_symbol as f64,
        16,
    )
    .unwrap();
    let mut output = Vec::with_capacity(16);
    let mut decoded = Vec::new();
    let mut first = 0;
    while first < samples.len() {
        let length = (17 + first % 193)
            .min(samples.len() - first)
            .min(stream.max_read_samples());
        stream
            .push(
                7,
                first as u64,
                &samples[first..first + length],
                &mut output,
            )
            .unwrap();
        decoded.append(&mut output);
        first += length;
    }
    stream.finish(&mut output).unwrap();
    decoded.append(&mut output);
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].segment, 7);
    assert_eq!(decoded[0].acquired.fields.color.dvcc, DVCC);
    assert!(
        (decoded[0].first_sample - ((PREFIX_SYMBOLS + FILTER_SPAN) * samples_per_symbol) as f64)
            .abs()
            <= 1.0
    );
    stream.discontinuity();
    let mut first = 0;
    while first < samples.len() {
        let length = stream.max_read_samples().min(samples.len() - first);
        stream
            .push(
                8,
                10000 + first as u64,
                &samples[first..first + length],
                &mut output,
            )
            .unwrap();
        decoded.append(&mut output);
        first += length;
    }
    stream.finish(&mut output).unwrap();
    decoded.append(&mut output);
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[1].segment, 8);
    assert!((decoded[1].first_sample - 10000.0 - decoded[0].first_sample).abs() <= 0.01);
}

#[test]
fn a_single_sync_does_not_identify_a_shortened_burst() {
    let samples = waveform(4, &[5]);
    let mut workspace = AcquisitionWorkspace::try_new(SYMBOL_RATE_HZ * 4.0, samples.len()).unwrap();
    let mut output = Vec::with_capacity(16);
    workspace.shortened(&samples, &mut output).unwrap();
    assert!(output.is_empty());
}
