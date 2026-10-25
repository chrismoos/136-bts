use std::{error::Error, fmt, sync::Arc};

use crate::{l3::legacy_traffic::TrafficMessage, link::rach::MobileIdentity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    Alert,
    Release,
    MeasureServingChannel,
    ExtendedMeasurements,
    StopMeasurements,
    PhysicalControl(crate::l3::legacy_traffic::PhysicalControlRequest),
    QueryStatus,
    QueryCapabilities(crate::l3::broadcast::ProtocolVersion),
}
impl OrderKind {
    fn message(self) -> Result<TrafficMessage, OrderError> {
        Ok(match self {
            Self::ExtendedMeasurements => return Err(OrderError::MeasurementFields),
            Self::PhysicalControl(control) => {
                TrafficMessage::PhysicalControl(crate::l3::legacy_traffic::PhysicalControlFields {
                    control,
                    ..Default::default()
                })
            }
            Self::QueryCapabilities(version) => TrafficMessage::CapabilityUpdateRequest(version),
            Self::Alert => TrafficMessage::AlertWithInfo {
                information: Default::default(),
            },
            Self::MeasureServingChannel => TrafficMessage::MeasurementOrder {
                rf_channels: Vec::new(),
            },
            Self::StopMeasurements => TrafficMessage::StopMeasurements,
            Self::QueryStatus => TrafficMessage::StatusRequest(
                crate::l3::legacy_traffic::StatusFields::identity_query(),
            ),
            Self::Release => TrafficMessage::Release {
                information: Default::default(),
            },
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderPolicy {
    pub response_timeout_ms: u64,
    pub overall_timeout_ms: u64,
    pub max_transmissions: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    Prepared,
    AwaitingAck { deadline_monotonic_ms: u64 },
    Acknowledged,
    TimedOut,
    Cancelled,
}
#[derive(Debug, Clone)]
pub struct OrderTransmission {
    token: Arc<()>,
    message: TrafficMessage,
}
impl OrderTransmission {
    pub(crate) fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
    pub fn message(&self) -> &TrafficMessage {
        &self.message
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderError {
    Identity,
    Policy,
    TimeOverflow,
    ClockRegression,
    StaleTransmission,
    AlreadyTransmitted,
    NotTransmitted,
    Recipient,
    NotAck,
    MeasurementChannels,
    MeasurementFields,
    StatusFields,
    ControlFields,
    AlertFields,
    ReleaseFields,
    AckType { expected: u8, received: u8 },
    Closed,
}
impl fmt::Display for OrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "traffic order exchange failed: {self:?}")
    }
}
impl Error for OrderError {}

/// One exchange belongs to one bearer lifetime. Responses have no transaction ID.
pub struct TrafficOrderExchange {
    recipient: MobileIdentity,
    kind: OrderKind,
    policy: OrderPolicy,
    state: OrderState,
    transmission: OrderTransmission,
    transmissions: u16,
    overall_deadline_monotonic_ms: u64,
    last_monotonic_ms: u64,
}
impl TrafficOrderExchange {
    // TIA-136-133-E §§8.5 and 9.1.2.2 define repeated traffic orders and their matching responses.
    pub fn try_new(
        recipient: MobileIdentity,
        kind: OrderKind,
        policy: OrderPolicy,
        monotonic_ms: u64,
    ) -> Result<Self, OrderError> {
        if let OrderKind::PhysicalControl(request) = kind
            && (request.validate().is_err() || request.is_empty())
        {
            return Err(OrderError::ControlFields);
        }
        Self::try_new_message(recipient, kind, kind.message()?, policy, monotonic_ms)
    }
    fn try_new_message(
        recipient: MobileIdentity,
        kind: OrderKind,
        message: TrafficMessage,
        policy: OrderPolicy,
        monotonic_ms: u64,
    ) -> Result<Self, OrderError> {
        let (_, width, value) = recipient.parts();
        if value == 0 || value >= 1 << width {
            return Err(OrderError::Identity);
        }
        if policy.response_timeout_ms == 0
            || policy.overall_timeout_ms == 0
            || policy.max_transmissions == 0
        {
            return Err(OrderError::Policy);
        }
        let overall_deadline_monotonic_ms = monotonic_ms
            .checked_add(policy.overall_timeout_ms)
            .ok_or(OrderError::TimeOverflow)?;
        Ok(Self {
            recipient,
            kind,
            policy,
            state: OrderState::Prepared,
            transmission: OrderTransmission {
                token: Arc::new(()),
                message,
            },
            transmissions: 0,
            overall_deadline_monotonic_ms,
            last_monotonic_ms: monotonic_ms,
        })
    }
    pub fn try_new_extended_measurements(
        recipient: MobileIdentity,
        order: crate::l3::legacy_traffic::ExtendedMeasurementOrder,
        version: crate::l3::broadcast::ProtocolVersion,
        policy: OrderPolicy,
        monotonic_ms: u64,
    ) -> Result<Self, OrderError> {
        order
            .validate_version(version)
            .map_err(|_| OrderError::MeasurementFields)?;
        Self::try_new_message(
            recipient,
            OrderKind::ExtendedMeasurements,
            TrafficMessage::ExtendedMeasurementOrder(order),
            policy,
            monotonic_ms,
        )
    }
    /// Retains parameters after timeout or cancellation. This does not authorize report acceptance.
    pub fn extended_targets(
        &self,
    ) -> Option<&crate::l3::legacy_traffic::ExtendedMeasurementTargets> {
        self.extended_order().map(|order| &order.targets)
    }
    pub fn extended_order(&self) -> Option<&crate::l3::legacy_traffic::ExtendedMeasurementOrder> {
        match &self.transmission.message {
            TrafficMessage::ExtendedMeasurementOrder(order) => Some(order),
            _ => None,
        }
    }
    pub fn try_new_alert(
        recipient: MobileIdentity,
        information: crate::l3::legacy_traffic::AlertInformation,
        policy: OrderPolicy,
        monotonic_ms: u64,
    ) -> Result<Self, OrderError> {
        information
            .validate()
            .map_err(|_| OrderError::AlertFields)?;
        let mut exchange = Self::try_new(recipient, OrderKind::Alert, policy, monotonic_ms)?;
        exchange.transmission.message = TrafficMessage::AlertWithInfo { information };
        Ok(exchange)
    }
    pub fn try_new_release(
        recipient: MobileIdentity,
        information: crate::l3::legacy_traffic::ReleaseInformation,
        policy: OrderPolicy,
        monotonic_ms: u64,
    ) -> Result<Self, OrderError> {
        information
            .validate_in(crate::l3::relay::Direction::Forward)
            .map_err(|_| OrderError::ReleaseFields)?;
        let mut exchange = Self::try_new(recipient, OrderKind::Release, policy, monotonic_ms)?;
        exchange.transmission.message = TrafficMessage::Release { information };
        Ok(exchange)
    }
    pub fn kind(&self) -> OrderKind {
        self.kind
    }
    pub fn state(&self) -> OrderState {
        self.state
    }
    pub fn transmissions(&self) -> u16 {
        self.transmissions
    }
    pub fn transmission(&self) -> Option<&OrderTransmission> {
        (self.state == OrderState::Prepared).then_some(&self.transmission)
    }
    /// Report transmission of the final FACCH word, not queue insertion or the first interleaved half.
    pub fn transmitted(
        &mut self,
        transmission: &OrderTransmission,
        monotonic_ms: u64,
    ) -> Result<OrderState, OrderError> {
        self.check_clock(monotonic_ms)?;
        if !Arc::ptr_eq(&transmission.token, &self.transmission.token) {
            return Err(OrderError::StaleTransmission);
        }
        match self.state {
            OrderState::Prepared => {}
            OrderState::AwaitingAck { .. } => return Err(OrderError::AlreadyTransmitted),
            _ => return Err(OrderError::Closed),
        }
        if monotonic_ms >= self.overall_deadline_monotonic_ms {
            self.state = OrderState::TimedOut;
        } else {
            let remaining_ms = self.overall_deadline_monotonic_ms - monotonic_ms;
            let deadline_monotonic_ms =
                monotonic_ms + self.policy.response_timeout_ms.min(remaining_ms);
            self.transmissions += 1;
            self.state = OrderState::AwaitingAck {
                deadline_monotonic_ms,
            };
        }
        self.last_monotonic_ms = monotonic_ms;
        Ok(self.state)
    }
    /// Input must be a CRC-validated response from this bearer, with at most one outstanding order there.
    pub fn acknowledge(
        &mut self,
        recipient: MobileIdentity,
        message: &TrafficMessage,
        monotonic_ms: u64,
    ) -> Result<OrderState, OrderError> {
        self.check_clock(monotonic_ms)?;
        if recipient != self.recipient {
            return Err(OrderError::Recipient);
        }
        if self.kind == OrderKind::ExtendedMeasurements {
            let TrafficMessage::ExtendedMeasurementOrder(order) = &self.transmission.message else {
                return Err(OrderError::MeasurementFields);
            };
            match message {
                TrafficMessage::ExtendedMeasurementAck(targets) if *targets == order.targets => {}
                TrafficMessage::ExtendedMeasurementAck(_) => {
                    return Err(OrderError::MeasurementChannels);
                }
                TrafficMessage::Ack {
                    ack_message_type,
                    information,
                } if order.mobile_ack_support == Some(true) => {
                    information
                        .validate_in(crate::l3::relay::Direction::Reverse)
                        .map_err(|_| OrderError::MeasurementFields)?;
                    let expected = self.transmission.message.message_type();
                    if *ack_message_type != expected {
                        return Err(OrderError::AckType {
                            expected,
                            received: *ack_message_type,
                        });
                    }
                }
                _ => return Err(OrderError::NotAck),
            }
        } else if let OrderKind::PhysicalControl(request) = self.kind {
            let TrafficMessage::PhysicalControlAck(fields) = message else {
                return Err(OrderError::NotAck);
            };
            if !request.answered_by(fields) {
                return Err(OrderError::ControlFields);
            }
        } else if matches!(self.kind, OrderKind::QueryCapabilities(_)) {
            if !matches!(
                message,
                TrafficMessage::CapabilityUpdateResponse(
                    crate::l3::legacy_traffic::CapabilityResponse::Mobile(_)
                )
            ) {
                return Err(OrderError::NotAck);
            }
        } else if self.kind == OrderKind::QueryStatus {
            let TrafficMessage::Status(fields) = message else {
                return Err(OrderError::NotAck);
            };
            if !fields.answers_identity_query() {
                return Err(OrderError::StatusFields);
            }
        } else if self.kind == OrderKind::MeasureServingChannel {
            let TrafficMessage::MeasurementOrderAck { rf_channels } = message else {
                return Err(OrderError::NotAck);
            };
            if !rf_channels.is_empty() {
                return Err(OrderError::MeasurementChannels);
            }
        } else {
            let TrafficMessage::Ack {
                ack_message_type, ..
            } = message
            else {
                return Err(OrderError::NotAck);
            };
            let expected = self.transmission.message.message_type();
            if *ack_message_type != expected {
                return Err(OrderError::AckType {
                    expected,
                    received: *ack_message_type,
                });
            }
        }
        if self.transmissions == 0 {
            return Err(OrderError::NotTransmitted);
        }
        self.advance(monotonic_ms)?;
        match self.state {
            OrderState::Prepared | OrderState::AwaitingAck { .. } | OrderState::Acknowledged => {
                self.state = OrderState::Acknowledged
            }
            _ => return Err(OrderError::Closed),
        }
        Ok(self.state)
    }
    pub fn advance(&mut self, monotonic_ms: u64) -> Result<OrderState, OrderError> {
        self.check_clock(monotonic_ms)?;
        match self.state {
            OrderState::Prepared | OrderState::AwaitingAck { .. }
                if monotonic_ms >= self.overall_deadline_monotonic_ms =>
            {
                self.state = OrderState::TimedOut
            }
            OrderState::AwaitingAck {
                deadline_monotonic_ms,
            } if monotonic_ms >= deadline_monotonic_ms => {
                if self.transmissions >= self.policy.max_transmissions {
                    self.state = OrderState::TimedOut;
                } else {
                    self.transmission = OrderTransmission {
                        token: Arc::new(()),
                        message: self.transmission.message.clone(),
                    };
                    self.state = OrderState::Prepared;
                }
            }
            _ => {}
        }
        self.last_monotonic_ms = monotonic_ms;
        Ok(self.state)
    }
    pub fn cancel(&mut self, monotonic_ms: u64) -> Result<OrderState, OrderError> {
        self.check_clock(monotonic_ms)?;
        match self.state {
            OrderState::Prepared | OrderState::AwaitingAck { .. } => {
                self.state = OrderState::Cancelled
            }
            _ => return Err(OrderError::Closed),
        }
        self.last_monotonic_ms = monotonic_ms;
        Ok(self.state)
    }
    fn check_clock(&self, monotonic_ms: u64) -> Result<(), OrderError> {
        if monotonic_ms < self.last_monotonic_ms {
            return Err(OrderError::ClockRegression);
        }
        Ok(())
    }
}

#[cfg(test)]
mod extended_tests;
#[cfg(test)]
mod tests {
    #[test]
    fn measurement_acknowledgments_match_the_channel_list_and_require_air_completion() {
        let mut exchange =
            TrafficOrderExchange::try_new(RECIPIENT, OrderKind::MeasureServingChannel, policy(), 0)
                .unwrap();
        let correct = TrafficMessage::MeasurementOrderAck {
            rf_channels: vec![],
        };
        assert_eq!(
            exchange.acknowledge(RECIPIENT, &correct, 1),
            Err(OrderError::NotTransmitted)
        );
        let tx = exchange.transmission().unwrap().clone();
        exchange.transmitted(&tx, 10).unwrap();
        assert_eq!(
            exchange.acknowledge(
                RECIPIENT,
                &TrafficMessage::MeasurementOrderAck {
                    rf_channels: vec![1]
                },
                11
            ),
            Err(OrderError::MeasurementChannels)
        );
        assert_eq!(
            exchange.acknowledge(MobileIdentity::Min34(2), &correct, 11),
            Err(OrderError::Recipient)
        );
        assert_eq!(
            exchange.acknowledge(RECIPIENT, &correct, 12).unwrap(),
            OrderState::Acknowledged
        );
        assert_eq!(
            exchange.acknowledge(RECIPIENT, &correct, 13).unwrap(),
            OrderState::Acknowledged
        );
        let mut stop =
            TrafficOrderExchange::try_new(RECIPIENT, OrderKind::StopMeasurements, policy(), 0)
                .unwrap();
        let tx = stop.transmission().unwrap().clone();
        stop.transmitted(&tx, 1).unwrap();
        let ack = TrafficMessage::Ack {
            ack_message_type: TrafficMessage::StopMeasurements.message_type(),
            information: Default::default(),
        };
        assert_eq!(
            stop.acknowledge(RECIPIENT, &ack, 2).unwrap(),
            OrderState::Acknowledged
        );
    }

    use super::*;
    const RECIPIENT: MobileIdentity = MobileIdentity::Min34(1);
    fn policy() -> OrderPolicy {
        OrderPolicy {
            response_timeout_ms: 100,
            overall_timeout_ms: 1000,
            max_transmissions: 3,
        }
    }
    fn exchange(kind: OrderKind) -> TrafficOrderExchange {
        TrafficOrderExchange::try_new(RECIPIENT, kind, policy(), 100).unwrap()
    }
    fn ack(kind: OrderKind) -> TrafficMessage {
        TrafficMessage::Ack {
            ack_message_type: kind.message().unwrap().message_type(),
            information: Default::default(),
        }
    }
    #[test]
    fn retransmissions_replace_callback_tokens_and_stop_after_the_exact_transmission_limit() {
        let mut exchange = exchange(OrderKind::Alert);
        let mut old = None;
        for attempt in 0..3 {
            let sent_at = 110 + attempt * 100;
            let token = exchange.transmission().unwrap().clone();
            if let Some(old) = &old {
                assert_eq!(
                    exchange.transmitted(old, sent_at),
                    Err(OrderError::StaleTransmission)
                );
            }
            assert_eq!(
                exchange.transmitted(&token, sent_at),
                Ok(OrderState::AwaitingAck {
                    deadline_monotonic_ms: sent_at + 100
                })
            );
            assert_eq!(
                exchange.transmitted(&token, sent_at),
                Err(OrderError::AlreadyTransmitted)
            );
            assert_eq!(exchange.transmissions(), attempt as u16 + 1);
            assert!(exchange.transmission().is_none());
            assert_eq!(
                exchange.advance(sent_at + 99).unwrap(),
                OrderState::AwaitingAck {
                    deadline_monotonic_ms: sent_at + 100
                }
            );
            let expected = if attempt == 2 {
                OrderState::TimedOut
            } else {
                OrderState::Prepared
            };
            assert_eq!(exchange.advance(sent_at + 100).unwrap(), expected);
            old = Some(token);
        }
        assert!(exchange.transmission().is_none());
        assert_eq!(
            exchange.acknowledge(RECIPIENT, &ack(OrderKind::Alert), 410),
            Err(OrderError::Closed)
        );
        assert_eq!(exchange.advance(10000).unwrap(), OrderState::TimedOut);
    }
    #[test]
    fn ack_requires_transmission_correct_bearer_and_type_and_remains_idempotent() {
        let mut exchange = exchange(OrderKind::Release);
        assert_eq!(
            exchange.acknowledge(RECIPIENT, &ack(OrderKind::Release), 100),
            Err(OrderError::NotTransmitted)
        );
        let token = exchange.transmission().unwrap().clone();
        exchange.transmitted(&token, 110).unwrap();
        assert_eq!(
            exchange.acknowledge(MobileIdentity::Min34(2), &ack(OrderKind::Release), 120),
            Err(OrderError::Recipient)
        );
        assert!(matches!(
            exchange.acknowledge(RECIPIENT, &ack(OrderKind::Alert), 120),
            Err(OrderError::AckType {
                expected: 0xda,
                received: 0x31
            })
        ));
        assert_eq!(
            exchange.acknowledge(RECIPIENT, &TrafficMessage::Connect, 120),
            Err(OrderError::NotAck)
        );
        assert_eq!(exchange.advance(210).unwrap(), OrderState::Prepared);
        let unsent_retry = exchange.transmission().unwrap().clone();
        assert_eq!(
            exchange
                .acknowledge(RECIPIENT, &ack(OrderKind::Release), 220)
                .unwrap(),
            OrderState::Acknowledged
        );
        assert_eq!(
            exchange.transmitted(&unsent_retry, 220),
            Err(OrderError::Closed)
        );
        assert_eq!(
            exchange
                .acknowledge(RECIPIENT, &ack(OrderKind::Release), 230)
                .unwrap(),
            OrderState::Acknowledged
        );
        assert_eq!(exchange.transmissions(), 1);
        assert_eq!(exchange.advance(10000).unwrap(), OrderState::Acknowledged);
    }
    #[test]
    fn overall_deadline_bounds_queueing_retries_and_response_wait_without_overflow() {
        let policy = OrderPolicy {
            response_timeout_ms: 100,
            overall_timeout_ms: 150,
            max_transmissions: 3,
        };
        let mut queued =
            TrafficOrderExchange::try_new(RECIPIENT, OrderKind::Alert, policy, 100).unwrap();
        assert_eq!(queued.advance(249).unwrap(), OrderState::Prepared);
        assert_eq!(queued.advance(250).unwrap(), OrderState::TimedOut);
        assert_eq!(queued.transmissions(), 0);
        let mut late =
            TrafficOrderExchange::try_new(RECIPIENT, OrderKind::Alert, policy, 100).unwrap();
        let token = late.transmission().unwrap().clone();
        assert_eq!(
            late.transmitted(&token, 240).unwrap(),
            OrderState::AwaitingAck {
                deadline_monotonic_ms: 250
            }
        );
        assert_eq!(
            late.acknowledge(RECIPIENT, &ack(OrderKind::Alert), 250),
            Err(OrderError::Closed)
        );
        assert_eq!(late.state(), OrderState::TimedOut);
        let mut boundary =
            TrafficOrderExchange::try_new(RECIPIENT, OrderKind::Release, policy, u64::MAX - 150)
                .unwrap();
        let token = boundary.transmission().unwrap().clone();
        assert_eq!(
            boundary.transmitted(&token, u64::MAX - 10).unwrap(),
            OrderState::AwaitingAck {
                deadline_monotonic_ms: u64::MAX
            }
        );
        assert_eq!(boundary.advance(u64::MAX).unwrap(), OrderState::TimedOut);
    }
    #[test]
    fn cancellation_clock_regression_and_foreign_callbacks_cannot_restart_an_exchange() {
        let mut exchange = exchange(OrderKind::Alert);
        let foreign =
            TrafficOrderExchange::try_new(RECIPIENT, OrderKind::Alert, policy(), 100).unwrap();
        assert_eq!(
            exchange.transmitted(foreign.transmission().unwrap(), 100),
            Err(OrderError::StaleTransmission)
        );
        assert_eq!(exchange.advance(99), Err(OrderError::ClockRegression));
        assert_eq!(exchange.cancel(99), Err(OrderError::ClockRegression));
        let token = exchange.transmission().unwrap().clone();
        exchange.transmitted(&token, 110).unwrap();
        assert_eq!(exchange.cancel(120).unwrap(), OrderState::Cancelled);
        assert_eq!(
            exchange.acknowledge(RECIPIENT, &ack(OrderKind::Alert), 130),
            Err(OrderError::Closed)
        );
        assert_eq!(exchange.transmitted(&token, 130), Err(OrderError::Closed));
        assert_eq!(exchange.advance(10000).unwrap(), OrderState::Cancelled);
    }
    #[test]
    fn invalid_identity_policy_and_initial_time_overflow_are_rejected() {
        assert!(matches!(
            TrafficOrderExchange::try_new(
                MobileIdentity::Min34(0),
                OrderKind::Alert,
                policy(),
                100
            ),
            Err(OrderError::Identity)
        ));
        for policy in [
            OrderPolicy {
                response_timeout_ms: 0,
                ..policy()
            },
            OrderPolicy {
                overall_timeout_ms: 0,
                ..policy()
            },
            OrderPolicy {
                max_transmissions: 0,
                ..policy()
            },
        ] {
            assert!(matches!(
                TrafficOrderExchange::try_new(RECIPIENT, OrderKind::Alert, policy, 100),
                Err(OrderError::Policy)
            ));
        }
        assert!(matches!(
            TrafficOrderExchange::try_new(RECIPIENT, OrderKind::Release, policy(), u64::MAX),
            Err(OrderError::TimeOverflow)
        ));
    }
}

#[cfg(test)]
mod alert_information_tests {
    use super::*;
    use crate::l3::legacy_traffic::{AlertInformation, CallingName, CallingNumber};
    #[test]
    fn alert_retry_keeps_exact_fields_and_rejects_the_old_transmission_token() {
        let information = AlertInformation {
            signals: Some(vec![1]),
            calling_numbers: vec![CallingNumber::network("1001").unwrap()],
            calling_name: Some(CallingName::network("Caller").unwrap()),
            ..Default::default()
        };
        let policy = OrderPolicy {
            response_timeout_ms: 100,
            overall_timeout_ms: 500,
            max_transmissions: 3,
        };
        let mut order = TrafficOrderExchange::try_new_alert(
            MobileIdentity::Min34(2),
            information.clone(),
            policy,
            0,
        )
        .unwrap();
        let first = order.transmission().unwrap().clone();
        let wire = first.message().encode().unwrap();
        order.transmitted(&first, 10).unwrap();
        order.advance(110).unwrap();
        let second = order.transmission().unwrap().clone();
        assert_eq!(
            second.message(),
            &TrafficMessage::AlertWithInfo { information }
        );
        assert_eq!(second.message().encode().unwrap(), wire);
        assert_eq!(
            order.transmitted(&first, 111),
            Err(OrderError::StaleTransmission)
        );
        order.transmitted(&second, 112).unwrap();
        assert_eq!(order.transmissions(), 2);
        assert_eq!(order.advance(500).unwrap(), OrderState::TimedOut);
    }
}
