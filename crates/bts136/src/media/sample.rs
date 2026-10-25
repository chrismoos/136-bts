use crate::codec::vselp::FRAME_SAMPLES;

const WAVE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/sample-sound.wav"
));
const PCM_OFFSET: usize = 44;
const PCM_SAMPLE_BYTES: usize = 2;
const PCM_SAMPLES: usize = (WAVE.len() - PCM_OFFSET) / PCM_SAMPLE_BYTES;
const PCM_NORMALIZATION: f64 = 32768.0;

pub struct SampleAudio {
    first_interval: u64,
}

impl SampleAudio {
    pub fn new(first_interval: u64) -> Self {
        Self { first_interval }
    }

    pub fn frame(&self, interval: u64) -> Option<[f64; FRAME_SAMPLES]> {
        let elapsed = interval.checked_sub(self.first_interval)?;
        let offset = ((u128::from(elapsed) * FRAME_SAMPLES as u128) % PCM_SAMPLES as u128) as usize;
        Some(std::array::from_fn(|index| {
            let sample = (offset + index) % PCM_SAMPLES;
            let byte = PCM_OFFSET + sample * PCM_SAMPLE_BYTES;
            f64::from(i16::from_le_bytes([WAVE[byte], WAVE[byte + 1]])) / PCM_NORMALIZATION
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_wave_is_mono_eight_khz_signed_sixteen_bit_pcm() {
        assert_eq!(&WAVE[..4], b"RIFF");
        assert_eq!(&WAVE[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(WAVE[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(WAVE[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(WAVE[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(WAVE[24..28].try_into().unwrap()), 8000);
        assert_eq!(u16::from_le_bytes(WAVE[34..36].try_into().unwrap()), 16);
        assert_eq!(&WAVE[36..40], b"data");
        assert_eq!(
            u32::from_le_bytes(WAVE[40..44].try_into().unwrap()) as usize,
            WAVE.len() - PCM_OFFSET
        );
        assert_eq!(PCM_SAMPLES, 54_912);
    }

    #[test]
    fn playback_preserves_samples_across_wrap_retries_and_skipped_intervals() {
        let source = SampleAudio::new(900);
        assert!(source.frame(899).is_none());
        for frame in 0..700 {
            let samples = source.frame(900 + frame).unwrap();
            assert_eq!(samples, source.frame(900 + frame).unwrap());
            for (index, actual) in samples.into_iter().enumerate() {
                let sample = (frame as usize * FRAME_SAMPLES + index) % PCM_SAMPLES;
                let byte = PCM_OFFSET + sample * PCM_SAMPLE_BYTES;
                let expected = i16::from_le_bytes([WAVE[byte], WAVE[byte + 1]]);
                assert_eq!(actual * PCM_NORMALIZATION, f64::from(expected));
            }
        }
        assert_eq!(
            source.frame(900 + 600),
            SampleAudio::new(900).frame(900 + 600)
        );
        assert!(source.frame(u64::MAX).is_some());
    }
}
