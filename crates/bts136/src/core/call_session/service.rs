use crate::l3::legacy_traffic::{ServiceMessage, ServiceOptions, ServiceRejection, TrafficMessage};
use std::sync::Arc;
// TIA-136-133-E §§8.5,4.7.2 start the FACCH response timeout after transmission.
const ACK_TIMEOUT_MS: u64 = 200;
const OVERALL_TIMEOUT_MS: u64 = 5000;
const MAX_TRANSMISSIONS: u8 = 3;
const REQUEST_MODULUS: u8 = 16;
const REJECTED: u8 = 6;
const DIGITAL_SPEECH: u8 = 1;
const ANALOG_PREFERRED: u8 = 2;
const DIGITAL_PREFERRED: u8 = 3;
const HALF_RATE: u8 = 0;
const FULL_RATE_ALLOWED: [u8; 5] = [1, 2, 3, 6, 7];
const VOICE_CODER_SHIFT: u32 = 3;
const PRIVACY_MASK: u8 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceError {
    Busy,
    ChangedRetransmission,
    Fields,
    ClockRegression,
    Overflow,
    StaleTransmission,
    Closed,
    NotTransmitted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceState {
    WaitingRequestAck,
    Prepared,
    AwaitingAck { deadline_ms: u64 },
    Acknowledged,
    TimedOut,
    Cancelled,
}
#[derive(Debug, Clone)]
pub(crate) struct ServiceTransmission {
    token: Arc<()>,
    message: TrafficMessage,
}
impl ServiceTransmission {
    pub fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
    pub fn message(&self) -> &TrafficMessage {
        &self.message
    }
}
#[derive(Clone, Default)]
pub(super) struct Endpoint {
    codec: crate::codec::speech::Codec,
    request: Option<ServiceMessage>,
    transmission: Option<ServiceTransmission>,
    state: Option<ServiceState>,
    next_number: u8,
    received_count: u64,
    last_ms: u64,
    deadline_ms: u64,
    transmissions: u8,
}
impl Endpoint {
    pub fn new(codec: crate::codec::speech::Codec) -> Self {
        Self {
            codec,
            ..Self::default()
        }
    }
    pub fn state(&self) -> Option<ServiceState> {
        self.state
    }
    pub fn request(&self) -> Option<&ServiceMessage> {
        self.request.as_ref()
    }
    pub fn response(&self) -> Option<&TrafficMessage> {
        self.transmission.as_ref().map(|tx| tx.message())
    }
    pub fn count(&self) -> u64 {
        self.received_count
    }
    pub fn transmission(&self) -> Option<&ServiceTransmission> {
        if self.state == Some(ServiceState::Prepared) {
            self.transmission.as_ref()
        } else {
            None
        }
    }
    pub fn validate(&self, request: &ServiceMessage) -> Result<bool, ServiceError> {
        if !matches!(request, ServiceMessage::Request { .. })
            || TrafficMessage::Service(request.clone()).encode().is_err()
        {
            return Err(ServiceError::Fields);
        }
        if let Some(previous) = &self.request {
            if previous.request_number() == request.request_number() {
                return if previous == request {
                    Ok(false)
                } else {
                    Err(ServiceError::ChangedRetransmission)
                };
            }
            if self.busy() {
                return Err(ServiceError::Busy);
            }
        }
        Ok(true)
    }
    pub fn receive(&mut self, request: ServiceMessage, now: u64) -> Result<(), ServiceError> {
        self.advance(now)?;
        let is_new = self.validate(&request)?;
        if !is_new && self.busy() {
            return Ok(());
        }
        let deadline = now
            .checked_add(OVERALL_TIMEOUT_MS)
            .ok_or(ServiceError::Overflow)?;
        if is_new {
            let message =
                response(&request, self.next_number, self.codec).ok_or(ServiceError::Fields)?;
            self.next_number = (self.next_number + 1) % REQUEST_MODULUS;
            self.transmission = Some(ServiceTransmission {
                token: Arc::new(()),
                message: TrafficMessage::Service(message),
            });
            self.request = Some(request);
            self.received_count += 1;
        } else if let Some(tx) = &mut self.transmission {
            tx.token = Arc::new(());
        }
        self.deadline_ms = deadline;
        self.transmissions = 0;
        self.state = Some(ServiceState::WaitingRequestAck);
        Ok(())
    }
    pub fn request_ack_transmitted(&mut self) {
        if self.state == Some(ServiceState::WaitingRequestAck) {
            self.state = Some(ServiceState::Prepared)
        }
    }
    pub fn transmitted(&mut self, tx: &ServiceTransmission, now: u64) -> Result<(), ServiceError> {
        self.advance(now)?;
        if self
            .transmission
            .as_ref()
            .is_none_or(|current| !current.matches(tx))
        {
            return Err(ServiceError::StaleTransmission);
        }
        if self.state != Some(ServiceState::Prepared) {
            return Err(ServiceError::Closed);
        }
        self.transmissions += 1;
        self.state = Some(ServiceState::AwaitingAck {
            deadline_ms: now
                .checked_add(ACK_TIMEOUT_MS)
                .ok_or(ServiceError::Overflow)?
                .min(self.deadline_ms),
        });
        Ok(())
    }
    pub fn acknowledge(&mut self, now: u64) -> Result<(), ServiceError> {
        self.advance(now)?;
        if self.state == Some(ServiceState::Acknowledged) {
            return Ok(());
        }
        if !self.busy() {
            return Err(ServiceError::Closed);
        }
        if self.transmissions == 0 {
            return Err(ServiceError::NotTransmitted);
        }
        self.state = Some(ServiceState::Acknowledged);
        Ok(())
    }
    pub fn advance(&mut self, now: u64) -> Result<(), ServiceError> {
        if now < self.last_ms {
            return Err(ServiceError::ClockRegression);
        }
        self.last_ms = now;
        if self.busy() && now >= self.deadline_ms {
            self.state = Some(ServiceState::TimedOut)
        }
        if let Some(ServiceState::AwaitingAck { deadline_ms }) = self.state
            && now >= deadline_ms
        {
            if self.transmissions >= MAX_TRANSMISSIONS {
                self.state = Some(ServiceState::TimedOut)
            } else {
                if let Some(tx) = &mut self.transmission {
                    tx.token = Arc::new(())
                }
                self.state = Some(ServiceState::Prepared);
            }
        }
        Ok(())
    }
    pub fn cancel(&mut self) {
        if self.busy() {
            self.state = Some(ServiceState::Cancelled)
        }
    }
    fn busy(&self) -> bool {
        matches!(
            self.state,
            Some(
                ServiceState::WaitingRequestAck
                    | ServiceState::Prepared
                    | ServiceState::AwaitingAck { .. }
            )
        )
    }
}
fn rejection_for_codec(
    options: &ServiceOptions,
    service: u8,
    codec: crate::codec::speech::Codec,
) -> Option<ServiceRejection> {
    if !matches!(
        service,
        DIGITAL_SPEECH | ANALOG_PREFERRED | DIGITAL_PREFERRED
    ) {
        return Some(ServiceRejection::UnsupportedService);
    }
    if !options.unparsed_bits.is_empty() || options.data_mode.is_some() {
        return Some(ServiceRejection::Unknown);
    }
    if options.bandwidth.as_ref().is_some_and(|values| {
        values
            .iter()
            .any(|value| !FULL_RATE_ALLOWED.contains(value))
    }) {
        return Some(
            if options
                .bandwidth
                .as_ref()
                .is_some_and(|values| values.contains(&HALF_RATE))
            {
                ServiceRejection::Unknown
            } else {
                ServiceRejection::TryFullRate
            },
        );
    }
    if options.extended_voice_mode.is_some() || options.interleaving.is_some() {
        return Some(ServiceRejection::UnsupportedCodec);
    }
    if let Some(values) = &options.voice_mode {
        if values.iter().any(|value| value & PRIVACY_MASK != 0) {
            return Some(ServiceRejection::UnsupportedPrivacy);
        }
        if values
            .iter()
            .any(|value| *value != codec.coder() << VOICE_CODER_SHIFT)
        {
            return Some(ServiceRejection::UnsupportedCodec);
        }
    }
    None
}

#[cfg(test)]
fn rejection(options: &ServiceOptions, service: u8) -> Option<ServiceRejection> {
    rejection_for_codec(options, service, crate::codec::speech::Codec::Vselp)
}

fn response(
    request: &ServiceMessage,
    number: u8,
    codec: crate::codec::speech::Codec,
) -> Option<ServiceMessage> {
    let ServiceMessage::Request {
        service_code,
        options,
        ..
    } = request
    else {
        return None;
    };
    let cause = rejection_for_codec(options, *service_code, codec);
    Some(ServiceMessage::Response {
        request_number: number,
        service_code: if cause.is_some() {
            REJECTED
        } else {
            *service_code
        },
        causes: cause.map(|cause| vec![cause as u8]),
        unparsed_bits: Vec::new(),
    })
}

#[cfg(test)]
mod tests;
