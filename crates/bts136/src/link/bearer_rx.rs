use super::{
    traffic::{SignalingWord, TrafficLinkError, TrafficMessageReceiver},
    voice::{FrameKind, TrafficFrame, TrafficFrameReceiver},
};
use crate::link::voice::DecodeError;
use crate::{
    codec::vselp::VselpParameters,
    l3::{legacy_traffic::TrafficMessage, relay::Direction},
    phy::{
        sacch::{ReceiveOutcome, SacchError, SacchReceiver, SacchWord},
        speech::CrcStatus,
        traffic::TrafficFields,
    },
};
use std::{error::Error as StdError, fmt};

#[derive(Debug)]
pub enum Error {
    Interval,
    Segment,
    Voice(DecodeError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bearer RX failed: {self:?}")
    }
}
impl StdError for Error {}
pub struct Received {
    pub discontinuity: bool,
    pub frame: Option<TrafficFrame>,
    pub facch_message: Result<Option<TrafficMessage>, TrafficLinkError>,
    pub sacch_word: Option<SacchWord>,
    pub sacch_error: Option<SacchError>,
    pub sacch_message: Result<Option<TrafficMessage>, TrafficLinkError>,
}
pub struct BearerRx {
    dvcc: u8,
    seed: VselpParameters,
    voice: TrafficFrameReceiver,
    facch: TrafficMessageReceiver,
    sacch: SacchReceiver,
    sacch_messages: TrafficMessageReceiver,
    previous: Option<(u64, u64)>,
}
impl BearerRx {
    pub(crate) fn has_pending_signaling(&self) -> bool {
        self.facch.has_pending() || self.sacch_messages.has_pending()
    }
    pub fn new(dvcc: u8, preceding: VselpParameters) -> Self {
        Self::with_direction(dvcc, preceding, Direction::Reverse)
    }
    pub fn with_direction(dvcc: u8, preceding: VselpParameters, direction: Direction) -> Self {
        Self {
            dvcc,
            seed: preceding.clone(),
            voice: TrafficFrameReceiver::new(dvcc, preceding),
            facch: TrafficMessageReceiver::new(direction),
            sacch: SacchReceiver::default(),
            sacch_messages: TrafficMessageReceiver::new(direction),
            previous: None,
        }
    }
    /// Use the common protocol version selected in this bearer's assignment.
    pub fn with_version(
        dvcc: u8,
        preceding: VselpParameters,
        direction: Direction,
        version: crate::l3::broadcast::ProtocolVersion,
    ) -> Self {
        let mut receiver = Self::with_direction(dvcc, preceding, direction);
        receiver.facch = TrafficMessageReceiver::with_version(direction, version);
        receiver.sacch_messages = TrafficMessageReceiver::with_version(direction, version);
        receiver
    }
    pub fn with_codec(
        dvcc: u8,
        codec: crate::codec::speech::Codec,
        direction: Direction,
        version: crate::l3::broadcast::ProtocolVersion,
    ) -> Self {
        let mut receiver = Self::with_version(dvcc, VselpParameters::default(), direction, version);
        if codec == crate::codec::speech::Codec::AcelpCc1 {
            receiver.voice = TrafficFrameReceiver::acelp(dvcc);
        }
        receiver
    }
    pub fn discontinuity(&mut self) {
        self.voice.reset(self.seed.clone());
        self.facch.discontinuity();
        self.sacch.discontinuity();
        self.sacch_messages.discontinuity();
    }
    /// Interval is the assigned 20 ms slot index. Segment identifies uninterrupted RX samples. Supply None for a missed slot.
    pub fn push(
        &mut self,
        interval: u64,
        segment: u64,
        fields: Option<&TrafficFields>,
    ) -> Result<Received, Error> {
        let discontinuity = if let Some((previous_interval, previous_segment)) = self.previous {
            if interval <= previous_interval {
                return Err(Error::Interval);
            }
            if segment < previous_segment {
                return Err(Error::Segment);
            }
            segment != previous_segment || interval - previous_interval != 1
        } else {
            false
        };
        if discontinuity {
            self.discontinuity();
        }
        let fields = fields.filter(|fields| fields.color.dvcc == self.dvcc);
        let frame = self
            .voice
            .push(fields.map(|fields| &fields.data))
            .map_err(Error::Voice)?;
        self.previous = Some((interval, segment));
        let facch_message = match frame.as_ref().map(|frame| &frame.kind) {
            Some(FrameKind::Facch(word)) => self.facch.push(&SignalingWord::from(word)),
            Some(
                FrameKind::Missing
                | FrameKind::UnavailableSpeech(_)
                | FrameKind::UnavailableAcelp(_)
                | FrameKind::Speech {
                    crc: CrcStatus::Failed,
                    ..
                },
            ) => {
                self.facch.discontinuity();
                Ok(None)
            }
            _ => Ok(None),
        };
        let (sacch_word, sacch_error) = if let Some(fields) = fields {
            match self.sacch.push_outcome(fields.sacch, self.dvcc) {
                ReceiveOutcome::Word(word) => (Some(word), None),
                ReceiveOutcome::Pending => (None, None),
                ReceiveOutcome::Rejected(error) => {
                    self.sacch_messages.discontinuity();
                    (None, Some(error))
                }
            }
        } else {
            self.sacch.discontinuity();
            self.sacch_messages.discontinuity();
            (None, None)
        };
        let sacch_message = if let Some(word) = &sacch_word {
            self.sacch_messages.push(&SignalingWord::from(word))
        } else {
            Ok(None)
        };
        Ok(Received {
            discontinuity,
            frame,
            facch_message,
            sacch_word,
            sacch_error,
            sacch_message,
        })
    }
}

#[cfg(test)]
mod tests;
