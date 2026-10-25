use super::*;
use crate::l3::legacy_traffic::{CallInformation, FlashMessage, ReverseFlashFields};
const REQUESTS: usize = 3;
const AFTER_INFORMATION_INTERVALS: u64 = 12;
#[derive(Default)]
pub(super) struct Probe {
    enabled: bool,
    network_received: usize,
    applications: usize,
    last_network: Option<(u8, CallInformation)>,
    last_network_interval: Option<u64>,
    queued: usize,
    acknowledged: usize,
    command: Option<crate::link::traffic_tx::CommandHandle>,
    completed: Option<u64>,
}
impl Probe {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }
    fn request(stage: usize) -> FlashMessage {
        FlashMessage::Reverse {
            request_number: if stage < 2 { 15 } else { 0 },
            request: ReverseFlashFields {
                feature: Some(vec![7]),
                keypad: Some("*2".into()),
                unparsed_bits: if stage == 2 {
                    "0111000001101000".chars().map(|c| c == '1').collect()
                } else {
                    vec![]
                },
                ..Default::default()
            },
        }
    }
    pub fn receive(
        &mut self,
        index: usize,
        interval: u64,
        message: FlashMessage,
        tx: &mut TrafficTx,
    ) -> Result<(), HalError> {
        match &message {
            FlashMessage::Forward {
                request_number,
                information,
            } => {
                self.network_received += 1;
                if let Some((last, fields)) = &self.last_network
                    && last == request_number
                {
                    if fields != information {
                        return Err(
                            "Flash retransmission changed fields without changing request number"
                                .into(),
                        );
                    }
                } else {
                    self.applications += 1;
                    self.last_network = Some((*request_number, information.clone()));
                }
                self.last_network_interval = Some(interval);
                if self.enabled && index == CALLEE && self.network_received == 1 {
                    tx.enqueue(&TrafficMessage::Ack {
                        ack_message_type: message.message_type(),
                        information: Default::default(),
                    })
                    .map_err(hal)?;
                    return Ok(());
                }
                if self.enabled && index == CALLER && self.network_received == 1 {
                    let mut wrong = message.acknowledgment().unwrap();
                    let FlashMessage::Ack(ack) = &mut wrong else {
                        unreachable!()
                    };
                    ack.request_number = (ack.request_number + 1) % 16;
                    tx.enqueue(&TrafficMessage::Flash(wrong)).map_err(hal)?;
                }
                tx.enqueue(&TrafficMessage::Flash(message.acknowledgment().unwrap()))
                    .map_err(hal)?;
            }
            FlashMessage::Ack(ack) => {
                if !self.enabled || self.acknowledged >= REQUESTS {
                    return Err("unexpected Flash acknowledgment".into());
                }
                let expected = Self::request(self.acknowledged).acknowledgment().unwrap();
                if expected != message {
                    return Err(
                        "Flash acknowledgment does not match request or last decoded parameter"
                            .into(),
                    );
                }
                let sent = self
                    .completed
                    .take()
                    .ok_or("Flash acknowledged before final reverse transmission")?;
                let symbols = interval
                    .checked_sub(sent)
                    .ok_or("Flash response predates request")?
                    * CONTROL_SLOT_PERIOD
                    * SYMBOLS_PER_SLOT as u64
                    + REVERSE_LEAD_SYMBOLS as u64;
                if symbols as f64 * 1000.0 > SYMBOL_RATE_HZ * FACCH_RESPONSE_TIMEOUT_MS as f64 {
                    return Err(
                        "Flash acknowledgment exceeded 200 ms FACCH response interval".into(),
                    );
                }
                assert_eq!(ack.request_number, expected.request_number());
                self.acknowledged += 1;
            }
            _ => return Err("reverse Flash request on forward channel".into()),
        }
        Ok(())
    }
    pub fn drive(&mut self, tx: &mut TrafficTx) -> Result<(), HalError> {
        if self.enabled && self.queued < REQUESTS && self.queued == self.acknowledged {
            self.command = Some(
                tx.enqueue(&TrafficMessage::Flash(Self::request(self.queued)))
                    .map_err(hal)?,
            );
            self.queued += 1;
        }
        Ok(())
    }
    pub fn submitted(
        &mut self,
        completed: &Option<crate::link::traffic_tx::CommandHandle>,
        interval: u64,
    ) {
        if completed.is_some() && completed == &self.command {
            self.completed = Some(interval);
            self.command = None;
        }
    }
    pub fn done(&self, interval: u64) -> bool {
        !self.enabled
            || (self.acknowledged == REQUESTS
                && self.applications == 1
                && self
                    .last_network_interval
                    .is_some_and(|at| interval > at + AFTER_INFORMATION_INTERVALS))
    }
    pub fn progress(&self) -> String {
        format!(
            "reverse={}/{}, network_received={}, applications={}",
            self.acknowledged, self.queued, self.network_received, self.applications
        )
    }
    pub fn verify(&self, index: usize) {
        assert_eq!(self.acknowledged, REQUESTS);
        assert_eq!(self.applications, 1);
        if index == CALLEE {
            assert!(self.network_received >= 2);
        }
        let (number, fields) = self.last_network.as_ref().unwrap();
        assert_eq!(*number, 0);
        assert_eq!(
            fields.calling_name.as_ref().unwrap().characters,
            if index == CALLEE {
                "Line 1001"
            } else {
                "Line 1002"
            }
        );
        assert_eq!(
            fields.voice_messages,
            Some(vec![if index == CALLEE { 63 } else { 0 }])
        );
        assert_eq!(fields.signals, Some(vec![0]));
        assert_eq!(
            fields.other_messages.as_ref().unwrap()[0].count,
            if index == CALLEE { 2 } else { 0 }
        );
    }
}
