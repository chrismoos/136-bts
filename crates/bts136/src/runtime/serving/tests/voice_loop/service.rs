use super::*;
use crate::l3::legacy_traffic::{ServiceMessage, ServiceOptions};
const REQUESTS: usize = 5;
const AFTER_RESPONSE_INTERVALS: u64 = 12;
const RETRY_INTERVALS: u64 = 10;
const NUMBER_MASK: u8 = 15;
#[derive(Default)]
pub(super) struct Probe {
    enabled: bool,
    stage: usize,
    command: Option<crate::link::traffic_tx::CommandHandle>,
    sent: Option<u64>,
    request_ack: Option<u64>,
    last_response: Option<ServiceMessage>,
    applied_response: Option<ServiceMessage>,
    response_interval: Option<u64>,
    completed: Option<u64>,
    retries: usize,
    restored_speech: usize,
}
impl Probe {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }
    fn request(stage: usize) -> TrafficMessage {
        let (service_code, options) = match stage {
            0 | 1 => (4, ServiceOptions::default()),
            2 => (
                1,
                ServiceOptions {
                    voice_mode: Some(vec![9]),
                    ..Default::default()
                },
            ),
            3 => (
                1,
                ServiceOptions {
                    voice_mode: Some(vec![16]),
                    ..Default::default()
                },
            ),
            _ => (
                1,
                ServiceOptions {
                    bandwidth: Some(vec![1]),
                    voice_mode: Some(vec![8]),
                    ..Default::default()
                },
            ),
        };
        TrafficMessage::Service(ServiceMessage::Request {
            request_number: if stage < 2 {
                NUMBER_MASK
            } else {
                stage as u8 - 2
            },
            service_code,
            options,
        })
    }
    pub fn acknowledged(&mut self, interval: u64) -> Result<(), HalError> {
        if !self.enabled || self.stage >= REQUESTS {
            return Err("unexpected Service Request acknowledgment".into());
        }
        let sent = self
            .sent
            .ok_or("Service Request ACK before final reverse transmission")?;
        let symbols = interval
            .checked_sub(sent)
            .ok_or("Service Request ACK predates request")?
            * CONTROL_SLOT_PERIOD
            * SYMBOLS_PER_SLOT as u64
            + REVERSE_LEAD_SYMBOLS as u64;
        if symbols as f64 * 1000.0 > SYMBOL_RATE_HZ * FACCH_RESPONSE_TIMEOUT_MS as f64 {
            return Err("Service Request ACK exceeded 200 ms".into());
        }
        self.request_ack = Some(interval);
        Ok(())
    }
    pub fn receive(
        &mut self,
        interval: u64,
        message: ServiceMessage,
        tx: &mut TrafficTx,
    ) -> Result<(), HalError> {
        let ServiceMessage::Response {
            request_number,
            service_code,
            causes,
            unparsed_bits,
        } = &message
        else {
            return Err("reverse service request on forward channel".into());
        };
        // TIA-136-133-E §§4.5,8.5 requires acknowledging a response retried before its prior ACK arrives.
        if self.request_ack.is_none() && self.applied_response.as_ref() == Some(&message) {
            tx.enqueue(&TrafficMessage::Ack {
                ack_message_type: ServiceMessage::RESPONSE_TYPE,
                information: Default::default(),
            })
            .map_err(hal)?;
            return Ok(());
        }
        let ack = self.request_ack.ok_or_else(|| -> HalError {
            format!("Service Response before Service Request ACK: stage={} interval={interval} response={message:?} sent={:?} completed={:?} previous={:?}",
                self.stage, self.sent, self.completed, self.last_response).into()
        })?;
        if interval <= ack {
            return Err("Service Response did not follow request acknowledgment".into());
        }
        if !unparsed_bits.is_empty() {
            return Err("unexpected service response extensions".into());
        }
        let expected = match self.stage {
            0 | 1 => Some(7),
            2 => Some(20),
            3 => Some(25),
            _ => None,
        };
        if *service_code != if expected.is_some() { 6 } else { 1 }
            || *causes != expected.map(|cause| vec![cause])
        {
            return Err(
                "service response changed unsupported bearer or has wrong rejection cause".into(),
            );
        }
        let expected_number = if self.stage < 2 {
            0
        } else {
            self.stage as u8 - 1
        };
        if *request_number != expected_number {
            return Err("Service Response did not use its own sequence".into());
        }
        if self.stage == 0 {
            if let Some(previous) = &self.last_response {
                if previous != &message {
                    return Err("service retry changed request number or cause".into());
                }
                if interval < self.response_interval.unwrap() + RETRY_INTERVALS {
                    return Err("Service Response retried before 200 ms".into());
                }
                self.retries += 1;
            } else {
                self.last_response = Some(message);
                self.response_interval = Some(interval);
                // An unrelated ACK must not satisfy the pending service response.
                tx.enqueue(&TrafficMessage::Ack {
                    ack_message_type: ServiceMessage::REQUEST_TYPE,
                    information: Default::default(),
                })
                .map_err(hal)?;
                return Ok(());
            }
        }
        tx.enqueue(&TrafficMessage::Ack {
            ack_message_type: ServiceMessage::RESPONSE_TYPE,
            information: Default::default(),
        })
        .map_err(hal)?;
        self.applied_response = Some(message);
        self.stage += 1;
        self.sent = None;
        self.request_ack = None;
        self.completed = Some(interval);
        Ok(())
    }
    pub fn drive(&mut self, interval: u64, tx: &mut TrafficTx) -> Result<(), HalError> {
        if self.enabled
            && self.stage < REQUESTS
            && self.command.is_none()
            && self.sent.is_none()
            && self
                .completed
                .is_none_or(|at| interval > at + AFTER_RESPONSE_INTERVALS)
        {
            self.command = Some(tx.enqueue(&Self::request(self.stage)).map_err(hal)?);
        }
        Ok(())
    }
    pub fn submitted(
        &mut self,
        command: &Option<crate::link::traffic_tx::CommandHandle>,
        interval: u64,
    ) {
        if command.is_some() && command == &self.command {
            self.sent = Some(interval);
            self.command = None;
        }
    }
    pub fn speech(&mut self) {
        if self.stage == REQUESTS {
            self.restored_speech += 1;
        }
    }
    pub fn done(&self, interval: u64) -> bool {
        !self.enabled
            || (self.stage == REQUESTS
                && self.restored_speech >= REQUIRED_SPEECH_FRAMES
                && self
                    .completed
                    .is_some_and(|at| interval > at + AFTER_RESPONSE_INTERVALS))
    }
    pub fn verify(&self) {
        if self.enabled {
            assert_eq!(self.stage, REQUESTS);
            assert_eq!(self.retries, 1);
            assert!(self.restored_speech >= REQUIRED_SPEECH_FRAMES);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rejected(number: u8, cause: u8) -> ServiceMessage {
        ServiceMessage::Response {
            request_number: number,
            service_code: 6,
            causes: Some(vec![cause]),
            unparsed_bits: vec![],
        }
    }
    #[test]
    fn accepted_service_response_retry_is_reacknowledged_without_advancing_next_request() {
        let mut probe = Probe::new(true);
        let mut tx = TrafficTx::try_new(DVCC, 16, 0).unwrap();
        probe.sent = Some(10);
        probe.acknowledged(11).unwrap();
        let response = rejected(0, 7);
        probe.receive(12, response.clone(), &mut tx).unwrap();
        probe.receive(22, response.clone(), &mut tx).unwrap();
        assert_eq!(probe.stage, 1);
        assert_eq!(probe.completed, Some(22));
        let pending = tx.pending_words();
        probe.receive(32, response, &mut tx).unwrap();
        assert_eq!(tx.pending_words(), pending + 1);
        assert_eq!(probe.stage, 1);
        assert_eq!(probe.completed, Some(22));
        assert_eq!(probe.sent, None);
        assert_eq!(probe.request_ack, None);
        assert_eq!(probe.retries, 1);
        assert!(probe.receive(33, rejected(1, 20), &mut tx).is_err());
        assert!(probe.receive(33, rejected(0, 20), &mut tx).is_err());
    }
}
