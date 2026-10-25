use super::traffic::SignalingWord;
use crate::{
    codec::vselp::{
        FRAME_SAMPLES, VselpParameters,
        decoder::{DecodeError as VselpDecodeError, Decoder},
    },
    codec::{
        mr74,
        speech::{Codec, Parameters},
    },
    phy::{
        facch::{self, FacchWord, WordHalf},
        speech::{self, CrcStatus, SpeechError},
        traffic::DATA_BITS,
    },
};

const PCM_FULL_SCALE: f64 = 32768.0;
const INTERLEAVER_COLUMNS: usize = 10;
#[derive(Debug, Clone, PartialEq, Eq)]
enum Payload {
    Speech([bool; DATA_BITS]),
    Acelp([bool; DATA_BITS]),
    Facch([bool; DATA_BITS]),
}
impl Payload {
    fn bits(&self) -> &[bool; DATA_BITS] {
        match self {
            Self::Speech(bits) | Self::Acelp(bits) | Self::Facch(bits) => bits,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransmitFrame {
    payload: Payload,
}
impl TransmitFrame {
    pub fn speech(parameters: &VselpParameters) -> Self {
        Self {
            payload: Payload::Speech(speech::encode(&parameters.speech_bits())),
        }
    }
    pub fn acelp(parameters: &mr74::Parameters) -> Self {
        Self {
            payload: Payload::Acelp(crate::phy::acelp_cc1::encode(parameters)),
        }
    }
    pub fn from_parameters(parameters: &Parameters) -> Self {
        match parameters {
            Parameters::Vselp(value) => Self::speech(value),
            Parameters::AcelpCc1(value) => Self::acelp(value),
        }
    }
    pub fn facch(word: &SignalingWord, dvcc: u8) -> Self {
        Self {
            payload: Payload::Facch(facch::encode(word.continuation, &word.contents, dvcc)),
        }
    }
    // IS-54-B §2.1.3.3.4 and Table 2.7.3.1.1.2-1 use previous-frame even rows and current-frame odd rows.
    pub fn interleave(previous: &Self, current: &Self) -> [bool; DATA_BITS] {
        let speech = speech::interleave(previous.payload.bits(), current.payload.bits());
        let acelp = crate::phy::acelp_cc1::interleave(
            &[
                [false; DATA_BITS],
                *previous.payload.bits(),
                *current.payload.bits(),
            ],
            crate::phy::acelp_cc1::Interleaving::Two,
        );
        let facch = facch::interleave(previous.payload.bits(), current.payload.bits());
        std::array::from_fn(|bit| {
            let payload = if (bit / INTERLEAVER_COLUMNS).is_multiple_of(2) {
                &previous.payload
            } else {
                &current.payload
            };
            match payload {
                Payload::Speech(_) => speech[bit],
                Payload::Acelp(_) => acelp[bit],
                Payload::Facch(_) => facch[bit],
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameKind {
    Speech {
        crc: CrcStatus,
        corrected_coded_bits: u16,
        coded_bits: u16,
    },
    Facch(FacchWord),
    Missing,
    UnavailableSpeech(SpeechError),
    UnavailableAcelp(crate::phy::acelp_cc1::DecodeError),
}

impl FrameKind {
    /// Channel bit errors of a CRC-valid speech or FACCH frame. Speech counts only the
    /// convolutionally coded class-1 bits, since class-2 bits have no redundancy.
    pub fn bit_errors(&self) -> Option<crate::phy::bit_errors::BitErrors> {
        match self {
            Self::Speech {
                crc: CrcStatus::Valid,
                corrected_coded_bits,
                coded_bits,
            } => Some(crate::phy::bit_errors::BitErrors {
                corrected: u32::from(*corrected_coded_bits),
                checked: u32::from(*coded_bits),
            }),
            Self::Facch(word) => Some(crate::phy::bit_errors::BitErrors {
                corrected: u32::from(word.corrected_coded_bits),
                checked: word.observed_coded_bits as u32,
            }),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct TrafficFrame {
    pub samples: [f64; FRAME_SAMPLES],
    pub kind: FrameKind,
    /// Channel-decoded parameters retain the received CRC status. Synthesized samples can use masked parameters instead.
    pub parameters: Option<VselpParameters>,
    pub acelp_parameters: Option<mr74::Parameters>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum SlotStatus {
    Uninitialized,
    Missing,
    Received,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviousSlot {
    status: SlotStatus,
    bits: [bool; DATA_BITS],
}
impl PreviousSlot {
    fn data(&self) -> Option<&[bool; DATA_BITS]> {
        match self.status {
            SlotStatus::Received => Some(&self.bits),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Vselp(VselpDecodeError),
    Acelp(mr74::noise_receiver::ReceiveError),
}
impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "traffic speech decode failed: {self:?}")
    }
}
impl std::error::Error for DecodeError {}
impl From<VselpDecodeError> for DecodeError {
    fn from(error: VselpDecodeError) -> Self {
        Self::Vselp(error)
    }
}
#[derive(Debug, Clone, PartialEq)]
enum SpeechDecoder {
    Vselp(Box<Decoder>),
    Acelp(Box<mr74::receiver::Receiver>),
}
#[derive(Debug, Clone, PartialEq)]
pub struct TrafficFrameReceiver {
    dvcc: u8,
    previous: PreviousSlot,
    decoder: SpeechDecoder,
}
impl TrafficFrameReceiver {
    pub fn new(dvcc: u8, preceding: VselpParameters) -> Self {
        Self {
            dvcc,
            previous: PreviousSlot {
                status: SlotStatus::Uninitialized,
                bits: [false; DATA_BITS],
            },
            decoder: SpeechDecoder::Vselp(Box::new(Decoder::new(preceding))),
        }
    }
    pub fn acelp(dvcc: u8) -> Self {
        let mut receiver = Self::new(dvcc, VselpParameters::default());
        receiver.decoder = SpeechDecoder::Acelp(Box::default());
        receiver
    }
    pub fn codec(&self) -> Codec {
        match self.decoder {
            SpeechDecoder::Vselp(_) => Codec::Vselp,
            SpeechDecoder::Acelp(_) => Codec::AcelpCc1,
        }
    }
    pub fn reset(&mut self, preceding: VselpParameters) {
        *self = match self.codec() {
            Codec::Vselp => Self::new(self.dvcc, preceding),
            Codec::AcelpCc1 => Self::acelp(self.dvcc),
        };
    }
    /// Supply every assigned 20 ms traffic-slot interval, using None for a missing slot.
    pub fn push(
        &mut self,
        data: Option<&[bool; DATA_BITS]>,
    ) -> Result<Option<TrafficFrame>, DecodeError> {
        let mut next = self.clone();
        let output = next.receive(data)?;
        *self = next;
        Ok(output)
    }
    fn receive(
        &mut self,
        data: Option<&[bool; DATA_BITS]>,
    ) -> Result<Option<TrafficFrame>, DecodeError> {
        let output = if self.previous.status == SlotStatus::Uninitialized {
            None
        } else {
            let previous = self.previous.data().copied();
            Some(self.frame(previous.as_ref(), data)?)
        };
        match data {
            Some(data) => {
                self.previous.bits = *data;
                self.previous.status = SlotStatus::Received;
            }
            None => self.previous.status = SlotStatus::Missing,
        }
        Ok(output)
    }
    // IS-54-B §2.7.3.1.1 uses DVCC-addressed CRC to distinguish FACCH replacements from speech.
    fn frame(
        &mut self,
        previous: Option<&[bool; DATA_BITS]>,
        current: Option<&[bool; DATA_BITS]>,
    ) -> Result<TrafficFrame, DecodeError> {
        let start = previous.map(|slot| facch::observe_half(slot, WordHalf::Start));
        let end = current.map(|slot| facch::observe_half(slot, WordHalf::End));
        let facch_bits = std::array::from_fn(|bit| {
            start
                .as_ref()
                .and_then(|bits| bits[bit])
                .or_else(|| end.as_ref().and_then(|bits| bits[bit]))
        });
        if let Ok(word) = facch::decode_erased(&facch_bits, self.dvcc) {
            return Ok(TrafficFrame {
                samples: self.conceal()?,
                kind: FrameKind::Facch(word),
                parameters: None,
                acelp_parameters: None,
            });
        }
        let (Some(previous), Some(current)) = (previous, current) else {
            return Ok(TrafficFrame {
                samples: self.conceal()?,
                kind: FrameKind::Missing,
                parameters: None,
                acelp_parameters: None,
            });
        };
        if matches!(self.decoder, SpeechDecoder::Acelp(_)) {
            return self.acelp_frame(previous, current);
        }
        let speech = speech::deinterleave(previous, current);
        match speech::decode_candidate(&speech) {
            Ok(candidate) => Ok(TrafficFrame {
                samples: match &mut self.decoder {
                    SpeechDecoder::Vselp(decoder) => decoder.decode_candidate(&candidate)?,
                    SpeechDecoder::Acelp(_) => {
                        unreachable!("ACELP dispatch precedes VSELP decoding")
                    }
                },
                kind: FrameKind::Speech {
                    crc: candidate.crc,
                    corrected_coded_bits: candidate.frame.corrected_coded_bits,
                    coded_bits: speech::PROTECTED_BITS as u16,
                },
                parameters: Some(VselpParameters::from_speech_bits(&candidate.frame.bits)),
                acelp_parameters: None,
            }),
            Err(error) => Ok(TrafficFrame {
                samples: self.conceal()?,
                kind: FrameKind::UnavailableSpeech(error),
                parameters: None,
                acelp_parameters: None,
            }),
        }
    }
    fn conceal(&mut self) -> Result<[f64; FRAME_SAMPLES], DecodeError> {
        match &mut self.decoder {
            SpeechDecoder::Vselp(decoder) => {
                decoder.conceal_missing_frame().map_err(DecodeError::Vselp)
            }
            SpeechDecoder::Acelp(decoder) => decoder
                .receive(mr74::receiver::Input::NoData)
                .map(|frame| frame.samples_q0.map(|s| f64::from(s) / PCM_FULL_SCALE))
                .map_err(DecodeError::Acelp),
        }
    }
    fn acelp_frame(
        &mut self,
        previous: &[bool; DATA_BITS],
        current: &[bool; DATA_BITS],
    ) -> Result<TrafficFrame, DecodeError> {
        use crate::phy::acelp_cc1;
        let channel = acelp_cc1::deinterleave(
            &[*previous, *current, [false; DATA_BITS]],
            acelp_cc1::Interleaving::Two,
        );
        let decoded = match acelp_cc1::decode(&channel) {
            Ok(decoded) => decoded,
            Err(error) => {
                return Ok(TrafficFrame {
                    samples: self.conceal()?,
                    kind: FrameKind::UnavailableAcelp(error),
                    parameters: None,
                    acelp_parameters: None,
                });
            }
        };
        let SpeechDecoder::Acelp(decoder) = &mut self.decoder else {
            unreachable!("ACELP receiver dispatch")
        };
        let input = if decoded.crc_valid {
            mr74::receiver::Input::SpeechGood(decoded.parameters)
        } else {
            mr74::receiver::Input::SpeechBad(decoded.parameters)
        };
        let samples = decoder
            .receive(input)
            .map_err(DecodeError::Acelp)?
            .samples_q0
            .map(|s| f64::from(s) / PCM_FULL_SCALE);
        Ok(TrafficFrame {
            samples,
            kind: FrameKind::Speech {
                crc: if decoded.crc_valid {
                    CrcStatus::Valid
                } else {
                    CrcStatus::Failed
                },
                corrected_coded_bits: decoded.corrected_coded_bits,
                coded_bits: acelp_cc1::PROTECTED_BITS as u16,
            },
            parameters: None,
            acelp_parameters: Some(decoded.parameters),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vselp::{SUBFRAMES, VselpSubframe};
    const DVCC: u8 = 10;
    fn seed() -> VselpParameters {
        VselpParameters::try_new(
            20,
            [32, 16, 16, 8, 8, 4, 4, 4, 4, 2],
            [VselpSubframe::try_new(21, 37, 54, 170).unwrap(); SUBFRAMES],
        )
        .unwrap()
    }
    #[test]
    fn gaps_advance_audio_once_per_interval_and_reset_restores_initial_buffering() {
        let seed = seed();
        let mut receiver = TrafficFrameReceiver::new(DVCC, seed.clone());
        let mut decoder = Decoder::new(seed.clone());
        assert_eq!(receiver.push(None).unwrap(), None);
        for _ in 0..7 {
            let frame = receiver.push(None).unwrap().unwrap();
            assert_eq!(frame.kind, FrameKind::Missing);
            assert_eq!(frame.samples, decoder.conceal_missing_frame().unwrap());
        }
        receiver.reset(seed);
        assert_eq!(receiver.push(None).unwrap(), None);
    }
    #[test]
    fn crc_valid_facch_is_recovered_from_either_available_half_while_audio_is_concealed() {
        let seed = seed();
        let message = std::array::from_fn(|bit| bit.is_multiple_of(3));
        let coded = facch::encode(true, &message, DVCC);
        let padding = facch::encode(false, &[false; facch::MESSAGE_BITS], DVCC);
        let first = facch::interleave(&padding, &coded);
        let last = facch::interleave(&coded, &padding);
        let mut receiver = TrafficFrameReceiver::new(DVCC, seed.clone());
        let mut decoder = Decoder::new(seed);
        assert_eq!(receiver.push(Some(&first)).unwrap(), None);
        for slot in [None, Some(&last)] {
            let frame = receiver.push(slot).unwrap().unwrap();
            let FrameKind::Facch(word) = frame.kind else {
                panic!("FACCH was not classified");
            };
            assert_eq!(word.message, message);
            assert!(word.continuation);
            assert_eq!(word.observed_coded_bits, facch::CODED_BITS / 2);
            assert_eq!(frame.samples, decoder.conceal_missing_frame().unwrap());
        }
    }
    #[test]
    fn clean_speech_decodes_and_facch_addressed_to_another_dvcc_is_not_delivered() {
        let seed = seed();
        let coded = speech::encode(&seed.speech_bits());
        let slots = [speech::interleave(&coded, &coded); 2];
        let mut receiver = TrafficFrameReceiver::new(DVCC, seed.clone());
        let mut decoder = Decoder::new(seed.clone());
        assert_eq!(receiver.push(Some(&slots[0])).unwrap(), None);
        let frame = receiver.push(Some(&slots[1])).unwrap().unwrap();
        assert_eq!(
            frame.kind,
            FrameKind::Speech {
                crc: CrcStatus::Valid,
                corrected_coded_bits: 0,
                coded_bits: speech::PROTECTED_BITS as u16,
            }
        );
        assert_eq!(frame.samples, decoder.decode(&seed).unwrap());
        let coded = facch::encode(false, &[false; facch::MESSAGE_BITS], DVCC + 1);
        let slot = facch::interleave(&coded, &coded);
        let mut receiver = TrafficFrameReceiver::new(DVCC, seed);
        receiver.push(Some(&slot)).unwrap();
        assert!(!matches!(
            receiver.push(Some(&slot)).unwrap().unwrap().kind,
            FrameKind::Facch(_)
        ));
    }
    #[test]
    fn valid_facch_takes_precedence_even_when_the_speech_crc_also_passes() {
        let message = [false; facch::MESSAGE_BITS];
        let coded = facch::encode(false, &message, DVCC);
        let slot = facch::interleave(&coded, &coded);
        let speech = speech::decode_candidate(&speech::deinterleave(&slot, &slot)).unwrap();
        assert_eq!(speech.crc, CrcStatus::Valid);
        let seed = seed();
        let mut receiver = TrafficFrameReceiver::new(DVCC, seed.clone());
        let mut decoder = Decoder::new(seed);
        receiver.push(Some(&slot)).unwrap();
        let frame = receiver.push(Some(&slot)).unwrap().unwrap();
        let FrameKind::Facch(word) = frame.kind else {
            panic!("valid FACCH was treated as speech");
        };
        assert_eq!(word.message, message);
        assert_eq!(frame.samples, decoder.conceal_missing_frame().unwrap());
    }
    #[test]
    fn ambiguous_speech_preserves_cadence_and_reports_the_rejection_reason() {
        let slot = [true; DATA_BITS];
        assert_eq!(
            speech::decode_candidate(&speech::deinterleave(&slot, &slot)),
            Err(SpeechError::Ambiguous)
        );
        let seed = seed();
        let mut receiver = TrafficFrameReceiver::new(DVCC, seed.clone());
        let mut decoder = Decoder::new(seed);
        receiver.push(Some(&slot)).unwrap();
        let output = receiver.push(Some(&slot)).unwrap().unwrap();
        assert_eq!(
            output.kind,
            FrameKind::UnavailableSpeech(SpeechError::Ambiguous)
        );
        assert_eq!(output.samples, decoder.conceal_missing_frame().unwrap());
    }
}
