use super::{mr74, vselp};
use std::{error::Error, fmt};
const PCM_SCALE: f64 = 32768.0;
const VSELP_CODER: u8 = 1;
const ACELP_CC1_CODER: u8 = 2;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Codec {
    #[default]
    Vselp,
    AcelpCc1,
}
impl Codec {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vselp => "VSELP",
            Self::AcelpCc1 => "ACELP CC1",
        }
    }
    pub fn coder(self) -> u8 {
        match self {
            Self::Vselp => VSELP_CODER,
            Self::AcelpCc1 => ACELP_CC1_CODER,
        }
    }
    pub const fn capability_mask(self) -> u8 {
        match self {
            Self::Vselp => 1,
            Self::AcelpCc1 => 2,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parameters {
    Vselp(vselp::VselpParameters),
    AcelpCc1(mr74::Parameters),
}
impl Parameters {
    pub fn codec(&self) -> Codec {
        match self {
            Self::Vselp(_) => Codec::Vselp,
            Self::AcelpCc1(_) => Codec::AcelpCc1,
        }
    }
}
impl From<vselp::VselpParameters> for Parameters {
    fn from(value: vselp::VselpParameters) -> Self {
        Self::Vselp(value)
    }
}
impl From<mr74::Parameters> for Parameters {
    fn from(value: mr74::Parameters) -> Self {
        Self::AcelpCc1(value)
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum EncodeError {
    Vselp(vselp::encoder::EncodeError),
    Acelp(mr74::encoder::EncodeError),
    NonFinite { sample: usize },
}
impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "speech encoding failed: {self:?}")
    }
}
impl Error for EncodeError {}
#[derive(Debug, Clone, PartialEq)]
pub enum Encoder {
    Vselp(Box<vselp::encoder::Encoder>),
    Acelp(Box<mr74::encoder::Encoder>),
}
impl Encoder {
    pub fn new(codec: Codec) -> Self {
        match codec {
            Codec::Vselp => Self::Vselp(Box::new(vselp::encoder::Encoder::new(
                vselp::VselpParameters::default(),
            ))),
            Codec::AcelpCc1 => Self::Acelp(Box::default()),
        }
    }
    /// PCM is normalized to signed 16-bit full scale. VSELP buffers its first frame for lookahead.
    pub fn encode(
        &mut self,
        samples: &[f64; vselp::FRAME_SAMPLES],
    ) -> Result<Option<Parameters>, EncodeError> {
        if let Some(sample) = samples.iter().position(|s| !s.is_finite()) {
            return Err(EncodeError::NonFinite { sample });
        }
        match self {
            Self::Vselp(encoder) => encoder
                .push(samples)
                .map(|value| value.map(Parameters::Vselp))
                .map_err(EncodeError::Vselp),
            Self::Acelp(encoder) => {
                let samples = samples.map(|s| {
                    (s * PCM_SCALE)
                        .round()
                        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
                });
                encoder
                    .encode(&samples)
                    .map(|value| Some(Parameters::AcelpCc1(value.parameters)))
                    .map_err(EncodeError::Acelp)
            }
        }
    }
}
