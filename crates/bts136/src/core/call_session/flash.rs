use crate::l3::legacy_traffic::{
    CallInformation, FlashAcknowledgment, FlashMessage, ReverseFlashFields, TrafficMessage,
};
use std::sync::Arc;
// TIA-136-133-E §4.7.2 bounds the FACCH response interval after final transmission.
const ACK_TIMEOUT_MS: u64 = 200;
const OVERALL_TIMEOUT_MS: u64 = 5000;
const MAX_TRANSMISSIONS: u8 = 3;
const REQUEST_MODULUS: u8 = 16;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashError {
    Busy,
    Empty,
    ClockRegression,
    Overflow,
    StaleTransmission,
    NotTransmitted,
    Closed,
    RequestNumber,
    Parameter,
    ChangedRetransmission,
    Fields,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlashState {
    Prepared,
    AwaitingAck { deadline_ms: u64 },
    Acknowledged,
    Partial { last_parameter: Option<u8> },
    TimedOut,
    Cancelled,
}
#[derive(Debug, Clone)]
pub(crate) struct FlashTransmission {
    token: Arc<()>,
    message: TrafficMessage,
}
impl FlashTransmission {
    pub fn message(&self) -> &TrafficMessage {
        &self.message
    }
    pub fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
}
#[derive(Default)]
pub(super) struct Endpoint {
    state: Option<FlashState>,
    next_number: u8,
    transmission: Option<FlashTransmission>,
    fields: Option<CallInformation>,
    overall_deadline_ms: u64,
    last_ms: u64,
    transmissions: u8,
    received: Option<(u8, ReverseFlashFields)>,
    received_count: u64,
}
impl Endpoint {
    pub fn state(&self) -> Option<FlashState> {
        self.state
    }
    pub fn transmission(&self) -> Option<&FlashTransmission> {
        if self.state == Some(FlashState::Prepared) {
            self.transmission.as_ref()
        } else {
            None
        }
    }
    pub fn number(&self) -> Option<u8> {
        self.transmission.as_ref().and_then(|tx| match &tx.message {
            TrafficMessage::Flash(message) => Some(message.request_number()),
            _ => None,
        })
    }
    pub fn received(&self) -> Option<&ReverseFlashFields> {
        self.received.as_ref().map(|(_, fields)| fields)
    }
    pub fn received_count(&self) -> u64 {
        self.received_count
    }
    pub fn request(&mut self, fields: CallInformation, now: u64) -> Result<(), FlashError> {
        self.advance(now)?;
        if fields.is_empty() {
            return Err(FlashError::Empty);
        }
        if !fields.unparsed_bits.is_empty() || fields.validate().is_err() {
            return Err(FlashError::Fields);
        }
        if self.busy() {
            return if self.fields.as_ref() == Some(&fields) {
                Ok(())
            } else {
                Err(FlashError::Busy)
            };
        }
        let overall = now
            .checked_add(OVERALL_TIMEOUT_MS)
            .ok_or(FlashError::Overflow)?;
        let message = TrafficMessage::Flash(FlashMessage::Forward {
            request_number: self.next_number,
            information: fields.clone(),
        });
        self.next_number = (self.next_number + 1) % REQUEST_MODULUS;
        self.fields = Some(fields);
        self.transmission = Some(FlashTransmission {
            token: Arc::new(()),
            message,
        });
        self.state = Some(FlashState::Prepared);
        self.overall_deadline_ms = overall;
        self.transmissions = 0;
        Ok(())
    }
    pub fn transmitted(&mut self, tx: &FlashTransmission, now: u64) -> Result<(), FlashError> {
        self.advance(now)?;
        if self
            .transmission
            .as_ref()
            .is_none_or(|current| !current.matches(tx))
        {
            return Err(FlashError::StaleTransmission);
        }
        if self.state != Some(FlashState::Prepared) {
            return Err(FlashError::Closed);
        }
        let deadline = now
            .checked_add(ACK_TIMEOUT_MS)
            .ok_or(FlashError::Overflow)?
            .min(self.overall_deadline_ms);
        self.transmissions += 1;
        self.state = Some(FlashState::AwaitingAck {
            deadline_ms: deadline,
        });
        Ok(())
    }
    pub fn acknowledge(&mut self, ack: &FlashAcknowledgment, now: u64) -> Result<(), FlashError> {
        self.advance(now)?;
        if self.number() != Some(ack.request_number) {
            return Err(FlashError::RequestNumber);
        }
        if !self.busy()
            && self.state != Some(FlashState::Acknowledged)
            && !matches!(self.state, Some(FlashState::Partial { .. }))
        {
            return Err(FlashError::Closed);
        }
        if self.transmissions == 0 {
            return Err(FlashError::NotTransmitted);
        }
        let fields = self.fields.as_ref().ok_or(FlashError::Closed)?;
        if let Some(parameter) = ack.last_decoded_parameter
            && !fields.contains_parameter(parameter)
        {
            return Err(FlashError::Parameter);
        }
        if matches!(
            self.state,
            Some(FlashState::Acknowledged | FlashState::Partial { .. })
        ) {
            return Ok(());
        }
        self.state = Some(
            if !ack.unparsed_bits.is_empty()
                || ack
                    .last_decoded_parameter
                    .is_some_and(|parameter| Some(parameter) != fields.last_parameter())
            {
                FlashState::Partial {
                    last_parameter: ack.last_decoded_parameter,
                }
            } else {
                FlashState::Acknowledged
            },
        );
        Ok(())
    }
    pub fn reverse_is_new(
        &self,
        number: u8,
        fields: &ReverseFlashFields,
    ) -> Result<bool, FlashError> {
        let message = TrafficMessage::Flash(FlashMessage::Reverse {
            request_number: number,
            request: fields.clone(),
        });
        if message.encode().is_err() {
            return Err(FlashError::Fields);
        }
        if let Some((last_number, last_fields)) = &self.received
            && *last_number == number
        {
            return if last_fields == fields {
                Ok(false)
            } else {
                Err(FlashError::ChangedRetransmission)
            };
        }
        Ok(true)
    }
    pub fn record_reverse(&mut self, number: u8, fields: ReverseFlashFields) {
        self.received = Some((number, fields));
        self.received_count += 1;
    }
    pub fn advance(&mut self, now: u64) -> Result<(), FlashError> {
        if now < self.last_ms {
            return Err(FlashError::ClockRegression);
        }
        self.last_ms = now;
        if self.busy() && now >= self.overall_deadline_ms {
            self.state = Some(FlashState::TimedOut);
        }
        if let Some(FlashState::AwaitingAck { deadline_ms }) = self.state
            && now >= deadline_ms
        {
            if self.transmissions >= MAX_TRANSMISSIONS {
                self.state = Some(FlashState::TimedOut);
            } else {
                if let Some(tx) = &mut self.transmission {
                    tx.token = Arc::new(());
                }
                self.state = Some(FlashState::Prepared);
            }
        }
        Ok(())
    }
    pub fn cancel(&mut self) {
        if self.busy() {
            self.state = Some(FlashState::Cancelled);
        }
    }
    fn busy(&self) -> bool {
        matches!(
            self.state,
            Some(FlashState::Prepared | FlashState::AwaitingAck { .. })
        )
    }
}
#[cfg(test)]
mod tests;
