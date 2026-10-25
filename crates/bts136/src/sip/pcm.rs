use super::{PCM_SAMPLES, Pcm};
use cdma_voice_gw::{
    codec::{CodecError, EncodedSpeech, SpeechCodec, SpeechCodecFactory},
    proto::AirCodec,
};
pub(super) const RATE_BPS: u32 = 128_000;
const SAMPLE_BITS: usize = 16;
pub(super) struct Factory;
pub(super) fn encode(pcm: &Pcm) -> Vec<u8> {
    pcm.iter()
        .flat_map(|s| {
            (0..SAMPLE_BITS)
                .rev()
                .map(move |bit| (((*s as u16) >> bit) & 1) as u8)
        })
        .collect()
}
pub(super) fn decode(bits: &[u8]) -> Result<Pcm, CodecError> {
    if bits.len() != PCM_SAMPLES * SAMPLE_BITS || bits.iter().any(|b| *b > 1) {
        return Err(CodecError::Frame("PCM frame length or bit value".into()));
    }
    Ok(std::array::from_fn(|i| {
        bits[i * SAMPLE_BITS..(i + 1) * SAMPLE_BITS]
            .iter()
            .fold(0u16, |v, b| (v << 1) | u16::from(*b)) as i16
    }))
}
impl SpeechCodecFactory for Factory {
    fn air_codec(&self) -> AirCodec {
        AirCodec::Pcm168k
    }
    fn create(&self) -> Result<Box<dyn SpeechCodec>, CodecError> {
        Ok(Box::new(Adapter))
    }
}
struct Adapter;
impl SpeechCodec for Adapter {
    fn frame_samples(&self) -> usize {
        PCM_SAMPLES
    }
    fn encode(&mut self, pcm: &[i16]) -> Result<EncodedSpeech, CodecError> {
        let frame: &Pcm = pcm.try_into().map_err(|_| CodecError::PcmLength {
            actual: pcm.len(),
            expected: PCM_SAMPLES,
        })?;
        Ok(EncodedSpeech {
            bits: encode(frame),
            rate_bps: RATE_BPS,
        })
    }
    fn decode(&mut self, bits: &[u8], rate_bps: u32) -> Result<Vec<i16>, CodecError> {
        if rate_bps != RATE_BPS {
            return Err(CodecError::Frame("PCM rate mismatch".into()));
        }
        Ok(decode(bits)?.to_vec())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_pcm_is_lossless() {
        let a = std::array::from_fn(|i| match i % 3 {
            0 => i16::MIN,
            1 => i16::MAX,
            _ => i as i16,
        });
        assert_eq!(decode(&encode(&a)).unwrap(), a);
        assert!(decode(&[2; PCM_SAMPLES * SAMPLE_BITS]).is_err());
    }
}
