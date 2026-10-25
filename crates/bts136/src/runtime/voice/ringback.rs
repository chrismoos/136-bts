use crate::{
    codec::vselp::{FRAME_SAMPLES, PCM_SAMPLE_RATE_HZ},
    config::RingbackStyle,
};

const FRAMES_PER_SECOND: u64 = (PCM_SAMPLE_RATE_HZ / FRAME_SAMPLES) as u64;
const US_ON_FRAMES: u64 = 2 * FRAMES_PER_SECOND;
const US_PERIOD_FRAMES: u64 = 6 * FRAMES_PER_SECOND;
const EU_ON_FRAMES: u64 = FRAMES_PER_SECOND;
const EU_PERIOD_FRAMES: u64 = 5 * FRAMES_PER_SECOND;
const US_LOW_HZ: f64 = 440.0;
const US_HIGH_HZ: f64 = 480.0;
const EU_HZ: f64 = 425.0;
const US_AMPLITUDE: f64 = 0.07;
const EU_AMPLITUDE: f64 = 0.1;

pub(super) fn frame(style: RingbackStyle, interval: u64) -> [f64; FRAME_SAMPLES] {
    let (on, period) = match style {
        RingbackStyle::Us => (US_ON_FRAMES, US_PERIOD_FRAMES),
        RingbackStyle::Eu => (EU_ON_FRAMES, EU_PERIOD_FRAMES),
    };
    if interval % period >= on {
        return [0.0; FRAME_SAMPLES];
    }
    std::array::from_fn(|i| {
        let sample = (interval % period) * FRAME_SAMPLES as u64 + i as u64;
        let seconds = sample as f64 / PCM_SAMPLE_RATE_HZ as f64;
        let phase = std::f64::consts::TAU * seconds;
        match style {
            RingbackStyle::Us => {
                US_AMPLITUDE * ((US_LOW_HZ * phase).sin() + (US_HIGH_HZ * phase).sin())
            }
            RingbackStyle::Eu => EU_AMPLITUDE * (EU_HZ * phase).sin(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn energy(samples: &[f64]) -> f64 {
        samples.iter().map(|s| s * s).sum()
    }

    #[test]
    fn ringback_cadences_include_silence_and_restart_at_the_period_boundary() {
        for (style, on, period) in [
            (RingbackStyle::Us, US_ON_FRAMES, US_PERIOD_FRAMES),
            (RingbackStyle::Eu, EU_ON_FRAMES, EU_PERIOD_FRAMES),
        ] {
            assert!(energy(&frame(style, 0)) > 0.0);
            assert!(energy(&frame(style, on - 1)) > 0.0);
            assert_eq!(energy(&frame(style, on)), 0.0);
            assert_eq!(energy(&frame(style, period - 1)), 0.0);
            assert_eq!(frame(style, period), frame(style, 0));
        }
    }

    #[test]
    fn ringback_spectrum_distinguishes_us_dual_tone_from_eu_single_tone() {
        const ANALYSIS_FRAMES: u64 = 10;
        const TOLERANCE: f64 = 1e-10;
        for (style, expected) in [
            (RingbackStyle::Us, [US_AMPLITUDE, US_AMPLITUDE, 0.0]),
            (RingbackStyle::Eu, [0.0, 0.0, EU_AMPLITUDE]),
        ] {
            let samples: Vec<_> = (0..ANALYSIS_FRAMES)
                .flat_map(|interval| frame(style, interval))
                .collect();
            for (frequency, amplitude) in [US_LOW_HZ, US_HIGH_HZ, EU_HZ].into_iter().zip(expected) {
                let measured = samples
                    .iter()
                    .enumerate()
                    .map(|(i, sample)| {
                        sample
                            * (std::f64::consts::TAU * frequency * i as f64
                                / PCM_SAMPLE_RATE_HZ as f64)
                                .sin()
                    })
                    .sum::<f64>()
                    * 2.0
                    / samples.len() as f64;
                assert!(
                    (measured - amplitude).abs() < TOLERANCE,
                    "{style:?}, {frequency}: {measured}"
                );
            }
        }
    }
}
