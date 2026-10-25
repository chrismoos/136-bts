use crate::link::voice::DecodeError;
use std::{error::Error, fmt};

use crate::{
    codec::vselp::VselpParameters,
    core::{
        call_session::{BearerHandle, BearerState, CallSession, Party, SessionError},
        calls::CallHandle,
    },
    l3::legacy_traffic::TrafficMessage,
    link::{
        traffic::{SignalingWord, TrafficLinkError, TrafficMessageReceiver},
        voice::{FrameKind, TrafficFrame, TrafficFrameReceiver, TransmitFrame},
    },
    phy::{speech::CrcStatus, traffic::DATA_BITS},
};
#[derive(Debug, Clone)]
pub struct LegConfig {
    pub dvcc: u8,
    pub preceding: VselpParameters,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDisposition {
    Speech,
    Concealment,
    Silence,
}
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeFrame {
    pub source: Party,
    pub destination: Party,
    pub source_interval: u64,
    pub received: TrafficFrame,
    pub signaling: Result<Option<TrafficMessage>, TrafficLinkError>,
    pub disposition: MediaDisposition,
    pub transmit: TransmitFrame,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeError {
    Call,
    Session(SessionError),
    InactiveBearer,
    Interval { expected: u64, received: u64 },
    IntervalOverflow,
    Decode(DecodeError),
    Parameters,
}
impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VSELP media bridge failed: {self:?}")
    }
}
impl Error for BridgeError {}
#[derive(Clone)]
pub struct VselpBridge {
    call: CallHandle,
    receivers: [TrafficFrameReceiver; 2],
    signaling: [TrafficMessageReceiver; 2],
    next_interval: [Option<u64>; 2],
    preceding_connected: [bool; 2],
    colors: [u8; 2],
}
impl VselpBridge {
    pub fn new(session: &CallSession, caller: LegConfig, callee: LegConfig) -> Self {
        Self {
            call: session.handle().clone(),
            receivers: [
                TrafficFrameReceiver::new(caller.dvcc, caller.preceding),
                TrafficFrameReceiver::new(callee.dvcc, callee.preceding),
            ],
            signaling: [
                TrafficMessageReceiver::default(),
                TrafficMessageReceiver::default(),
            ],
            next_interval: [None; 2],
            preceding_connected: [false; 2],
            colors: [caller.dvcc, callee.dvcc],
        }
    }
    /// Supply each assigned 20 ms interval. Missing slots use None, and skipped or repeated intervals are rejected.
    pub fn push(
        &mut self,
        session: &CallSession,
        source: &BearerHandle,
        interval: u64,
        data: Option<&[bool; DATA_BITS]>,
    ) -> Result<Option<BridgeFrame>, BridgeError> {
        if !self.call.matches(session.handle()) {
            return Err(BridgeError::Call);
        }
        session.check_bearer(source).map_err(BridgeError::Session)?;
        if matches!(
            session.bearer_state(source.party()),
            BearerState::Reserved | BearerState::Stopped
        ) {
            return Err(BridgeError::InactiveBearer);
        }
        let source_index = index(source.party());
        if let Some(expected) = self.next_interval[source_index]
            && interval != expected
        {
            return Err(BridgeError::Interval {
                expected,
                received: interval,
            });
        }
        let next_interval = interval
            .checked_add(1)
            .ok_or(BridgeError::IntervalOverflow)?;
        let mut receiver = self.receivers[source_index].clone();
        let mut signaling = self.signaling[source_index].clone();
        let received = receiver.push(data).map_err(BridgeError::Decode)?;
        let connected = session.media_ready();
        let output = if let Some(received) = received {
            let destination = opposite(source.party());
            let messages = match &received.kind {
                FrameKind::Facch(word) => signaling.push(&SignalingWord::from(word)),
                FrameKind::Missing
                | FrameKind::UnavailableSpeech(_)
                | FrameKind::UnavailableAcelp(_)
                | FrameKind::Speech {
                    crc: CrcStatus::Failed,
                    ..
                } => {
                    signaling.discontinuity();
                    Ok(None)
                }
                _ => Ok(None),
            };
            let (disposition, transmit) = if !connected || !self.preceding_connected[source_index] {
                (
                    MediaDisposition::Silence,
                    TransmitFrame::speech(&VselpParameters::default()),
                )
            } else if matches!(
                received.kind,
                FrameKind::Speech {
                    crc: CrcStatus::Valid,
                    ..
                }
            ) {
                let parameters = received
                    .parameters
                    .as_ref()
                    .ok_or(BridgeError::Parameters)?;
                (MediaDisposition::Speech, TransmitFrame::speech(parameters))
            } else {
                // IS-54-B §2.6.5.6.3 filler contains no peer call-control message and FACCH replacement advances voice masking.
                (
                    MediaDisposition::Concealment,
                    TransmitFrame::facch(&SignalingWord::filler(), self.colors[index(destination)]),
                )
            };
            Some(BridgeFrame {
                source: source.party(),
                destination,
                source_interval: self.next_interval[source_index]
                    .expect("receiver output follows an accepted preceding interval")
                    - 1,
                received,
                signaling: messages,
                disposition,
                transmit,
            })
        } else {
            None
        };
        self.receivers[source_index] = receiver;
        self.signaling[source_index] = signaling;
        self.next_interval[source_index] = Some(next_interval);
        self.preceding_connected[source_index] = connected;
        Ok(output)
    }
}
fn index(party: Party) -> usize {
    match party {
        Party::Caller => 0,
        Party::Callee => 1,
    }
}
fn opposite(party: Party) -> Party {
    match party {
        Party::Caller => Party::Callee,
        Party::Callee => Party::Caller,
    }
}

#[cfg(test)]
mod tests;
