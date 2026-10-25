use super::*;
use crate::codec::vselp::decoder::Decoder;
fn burst(number: u8, digits: Vec<DtmfDigit>) -> DtmfMessage {
    DtmfMessage::Burst {
        request_number: number,
        digits: Some(digits),
    }
}
fn admit(sender: &mut Sender, request: &DtmfMessage) {
    if sender.is_new(request).unwrap() {
        sender.accept(request);
    }
}
#[test]
fn retries_are_per_type_and_wrap_without_replaying_or_losing_stop() {
    let mut sender = Sender::default();
    let request = burst(15, vec![DtmfDigit::One]);
    admit(&mut sender, &request);
    admit(&mut sender, &request);
    assert_eq!(sender.burst.len(), 1);
    assert_eq!(
        sender.is_new(&burst(15, vec![DtmfDigit::Two])),
        Err(AdmissionError::ChangedRetransmission)
    );
    admit(
        &mut sender,
        &DtmfMessage::Continuous {
            request_number: 15,
            digit: DtmfDigit::Three,
        },
    );
    assert_eq!(sender.continuous, Some(DtmfDigit::Three));
    admit(&mut sender, &burst(0, vec![DtmfDigit::Two]));
    assert_eq!(sender.burst.len(), 2);
    let stop = DtmfMessage::Continuous {
        request_number: 0,
        digit: DtmfDigit::Null,
    };
    admit(&mut sender, &stop);
    admit(&mut sender, &stop);
    assert_eq!(sender.continuous, None);
    assert_eq!(sender.burst.len(), 2);
    sender.clear();
    assert!(sender.burst.is_empty());
    assert_eq!(sender.is_new(&request), Ok(true));
}
#[test]
fn admission_is_bounded_and_stop_remains_available_when_bursts_are_full() {
    let mut sender = Sender::default();
    for number in 0..4 {
        admit(&mut sender, &burst(number, vec![DtmfDigit::One; 63]));
    }
    assert_eq!(sender.burst.len(), MAX_PENDING_DIGITS);
    assert_eq!(
        sender.is_new(&burst(4, vec![DtmfDigit::One])),
        Err(AdmissionError::Capacity)
    );
    admit(
        &mut sender,
        &DtmfMessage::Continuous {
            request_number: 4,
            digit: DtmfDigit::Null,
        },
    );
    assert_eq!(
        sender.is_new(&burst(3, vec![DtmfDigit::One; 63])),
        Ok(false)
    );
    assert_eq!(
        sender.is_new(&DtmfMessage::ContinuousAck { request_number: 0 }),
        Err(AdmissionError::NotRequest)
    );
}
#[test]
fn burst_duration_gaps_duplicate_intervals_and_skips_preserve_timeline() {
    let mut sender = Sender::default();
    admit(&mut sender, &burst(0, vec![DtmfDigit::One, DtmfDigit::Two]));
    for _ in 0..BURST_TONE_FRAMES {
        assert_eq!(sender.next_digit(), Some(DtmfDigit::One));
    }
    for _ in 0..BURST_GAP_FRAMES {
        assert_eq!(sender.next_digit(), None);
    }
    sender.skip(10);
    sender.skip(10);
    assert_eq!(sender.burst_frame, 1);
    let frame = sender.frame(11, VselpParameters::default()).unwrap();
    assert_eq!(sender.frame(11, VselpParameters::default()).unwrap(), frame);
    assert_eq!(sender.burst_frame, 2);
    sender.clear();
    assert!(
        sender
            .frame(12, VselpParameters::default())
            .unwrap()
            .is_none()
    );
}
fn power(samples: &[f64], hz: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, value) in samples.iter().enumerate() {
        let phase = TAU * hz * n as f64 / PCM_SAMPLE_RATE_HZ as f64;
        re += value * phase.cos();
        im += value * phase.sin();
    }
    re * re + im * im
}
#[test]
fn every_keypad_pair_survives_production_tone_generation_and_vselp() {
    for code in 1..=12 {
        let digit = DtmfDigit::decode(code).unwrap();
        let mut sender = Sender::default();
        admit(
            &mut sender,
            &DtmfMessage::Continuous {
                request_number: 0,
                digit,
            },
        );
        let mut decoder = Decoder::new(VselpParameters::default());
        let mut samples = Vec::new();
        for interval in 0..20 {
            if let Some(parameters) = sender.frame(interval, VselpParameters::default()).unwrap() {
                let pcm = decoder.decode(&parameters).unwrap();
                if interval >= 5 {
                    samples.extend(pcm);
                }
            }
        }
        let expected = frequencies(digit).unwrap();
        let row = ROW_HZ
            .into_iter()
            .max_by(|a, b| power(&samples, *a).total_cmp(&power(&samples, *b)))
            .unwrap();
        let column = COLUMN_HZ
            .into_iter()
            .max_by(|a, b| power(&samples, *a).total_cmp(&power(&samples, *b)))
            .unwrap();
        assert_eq!((row, column), expected, "{}", digit.symbol());
        admit(
            &mut sender,
            &DtmfMessage::Continuous {
                request_number: 1,
                digit: DtmfDigit::Null,
            },
        );
        for interval in 20..24 {
            sender.frame(interval, VselpParameters::default()).unwrap();
        }
        assert!(
            sender
                .frame(24, VselpParameters::default())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn facch_preemption_pauses_bursts_without_consuming_retained_digits() {
    let mut sender = Sender::default();
    admit(&mut sender, &burst(0, vec![DtmfDigit::One]));
    for interval in 0..20 {
        sender.pause(interval);
    }
    assert_eq!(sender.burst_frame, 0);
    assert_eq!(sender.burst.len(), 1);
    assert!(
        sender
            .frame(20, VselpParameters::default())
            .unwrap()
            .is_none()
    );
    assert!(
        sender
            .frame(21, VselpParameters::default())
            .unwrap()
            .is_some()
    );
    for interval in 22..30 {
        sender.pause(interval);
    }
    assert_eq!(sender.burst_frame, 2);
    assert!(
        sender
            .frame(30, VselpParameters::default())
            .unwrap()
            .is_some()
    );
    assert_eq!(sender.burst_frame, 3);
}
