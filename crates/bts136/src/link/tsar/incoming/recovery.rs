use super::*;
use crate::l3::tsar::Segment;

#[derive(Debug, Clone, Copy)]
pub struct RecoveryContext {
    pub peer: MobileIdentity,
    pub protocol: HigherLayerProtocol,
    pub transaction_id: u8,
    pub version: ProtocolVersion,
    pub maximum_message_octets: usize,
    pub created_monotonic_ms: u64,
}
#[derive(Debug, Clone)]
pub enum RecoveryEvent {
    Segment(Segment),
    Advance,
    ApplicationSettled(ApplicationOutcome),
    Cancel,
}
#[derive(Debug, Clone)]
pub struct RecoveryObservation {
    pub monotonic_ms: u64,
    pub event: RecoveryEvent,
}
impl RecoveredIncoming {
    /// Replay durable observations in order. Historical responses never become new transmission work.
    pub fn replay(
        context: RecoveryContext,
        observations: impl IntoIterator<Item = RecoveryObservation>,
    ) -> Result<Self, IncomingError> {
        if context.maximum_message_octets == 0 {
            return Err(IncomingError::Limits);
        }
        let receiver = Receiver::try_new(
            context.peer,
            context.protocol,
            context.transaction_id,
            context.version,
            context.maximum_message_octets,
            context.created_monotonic_ms,
        )
        .map_err(IncomingError::Receiver)?;
        let mut active = OwnedReceiver {
            owner: Arc::new(()),
            receiver,
            application: Application::None,
        };
        let mut last = context.created_monotonic_ms;
        for observation in observations {
            let now = observation.monotonic_ms;
            if now < last {
                return Err(IncomingError::ClockRegression);
            }
            match observation.event {
                RecoveryEvent::Segment(segment) => {
                    let reception = active
                        .receiver
                        .receive(
                            context.peer,
                            context.protocol,
                            &DecodedSegment {
                                segment,
                                unparsed_octets: Vec::new(),
                            },
                            now,
                        )
                        .map_err(IncomingError::Receiver)?;
                    IncomingTransactions::retain(&mut active, reception);
                }
                RecoveryEvent::Advance => {
                    active
                        .receiver
                        .advance(now)
                        .map_err(IncomingError::Receiver)?;
                }
                RecoveryEvent::Cancel => {
                    active
                        .receiver
                        .cancel(now)
                        .map_err(IncomingError::Receiver)?;
                }
                RecoveryEvent::ApplicationSettled(outcome) => match &active.application {
                    Application::Pending(_) => active.application = Application::Settled(outcome),
                    Application::Settled(previous) if *previous == outcome => {}
                    _ => return Err(IncomingError::StaleApplication),
                },
            }
            last = now;
        }
        Ok(Self {
            context,
            active,
            last_monotonic_ms: last,
        })
    }
}

#[cfg(test)]
mod tests;
