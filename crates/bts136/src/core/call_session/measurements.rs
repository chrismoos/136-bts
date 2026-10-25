use super::{BearerHandle, BearerState, CallSession, CallState, Leg, Party, SessionError};
use crate::{
    l3::{
        broadcast::ProtocolVersion,
        legacy_traffic::{
            ExtendedChannelQualityReport, ExtendedMeasurementOrder, ExtendedMeasurementTargets,
            ExtendedServingMeasurement, TrafficMessageError,
        },
    },
    link::traffic_order::{OrderKind, OrderPolicy, OrderState, TrafficOrderExchange},
};
use std::sync::Arc;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AlignmentRestriction {
    Requested,
    Confirmed,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AlignmentState {
    Unrestricted,
    Restricted,
    AwaitingMeasurementTransmission,
}
impl AlignmentState {
    pub(super) fn restrict(&mut self) {
        if *self == Self::Unrestricted {
            *self = Self::Restricted;
        }
    }
}
impl Leg {
    // TIA-136-133-E §6.1 requires Stop or serving-only measurements at the maximum advance.
    pub(super) fn restrict_neighbor_measurements(
        &mut self,
        policy: OrderPolicy,
        now_ms: u64,
        cause: AlignmentRestriction,
    ) -> Result<(), SessionError> {
        let needs_stop = self.measurements.as_ref().is_some_and(|order| {
            order
                .extended_targets()
                .is_some_and(|targets| !targets.channels().is_empty())
                || (cause == AlignmentRestriction::Requested
                    && order.kind() == OrderKind::StopMeasurements
                    && matches!(order.state(), OrderState::TimedOut | OrderState::Cancelled))
        });
        if needs_stop {
            let stop = TrafficOrderExchange::try_new(
                self.subscriber.identity,
                OrderKind::StopMeasurements,
                policy,
                now_ms,
            )
            .map_err(SessionError::Order)?;
            self.measurements = Some(stop);
            self.extended_measurements = None;
            self.alignment_measurements = AlignmentState::AwaitingMeasurementTransmission;
        }
        Ok(())
    }
    pub(super) fn measurement_alignment_transmitted(&mut self) {
        if self.alignment_measurements == AlignmentState::AwaitingMeasurementTransmission
            && self.measurements.as_ref().is_some_and(|order| {
                matches!(order.state(), OrderState::AwaitingAck { .. })
                    && (matches!(
                        order.kind(),
                        OrderKind::StopMeasurements | OrderKind::MeasureServingChannel
                    ) || order
                        .extended_targets()
                        .is_some_and(|targets| targets.channels().is_empty()))
            })
        {
            self.alignment_measurements = AlignmentState::Restricted;
        }
    }
}
#[derive(Debug, Clone)]
pub struct MeasurementHandle {
    bearer: BearerHandle,
    token: Arc<()>,
}
impl PartialEq for MeasurementHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
}
impl Eq for MeasurementHandle {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedMeasurementSnapshot {
    pub targets: ExtendedMeasurementTargets,
    pub serving: Option<ExtendedServingMeasurement>,
    pub neighbor_rssi_codes: Vec<Option<u8>>,
    pub received_monotonic_ms: Option<u64>,
}
pub(super) struct Active {
    handle: MeasurementHandle,
    snapshot: Option<ExtendedMeasurementSnapshot>,
}
impl Active {
    pub(super) fn transmitted(&mut self, targets: ExtendedMeasurementTargets) {
        if self.snapshot.is_none() {
            self.snapshot = Some(ExtendedMeasurementSnapshot {
                neighbor_rssi_codes: vec![None; targets.channels().len()],
                targets,
                serving: None,
                received_monotonic_ms: None,
            });
        }
    }
}
impl CallSession {
    pub fn confirmed_physical_control(
        &self,
        party: Party,
    ) -> &crate::l3::legacy_traffic::PhysicalControlFields {
        self.legs
            .get(party.index())
            .map_or(&self.unassigned_control, |leg| {
                &leg.confirmed_physical_control
            })
    }
    // The bound version must equal the value encoded in this bearer's assignment.
    pub fn bind_protocol(
        &mut self,
        handle: &BearerHandle,
        version: ProtocolVersion,
    ) -> Result<(), SessionError> {
        self.check_bearer(handle)?;
        let leg = &mut self.legs[handle.party.index()];
        if self.state != CallState::Establishing || leg.state != BearerState::Reserved {
            return Err(SessionError::State);
        }
        if leg.protocol_version.is_some_and(|bound| bound != version) {
            return Err(SessionError::ProtocolContext);
        }
        leg.protocol_version = Some(version);
        Ok(())
    }
    pub fn protocol_version(&self, party: Party) -> Option<ProtocolVersion> {
        self.legs.get(party.index())?.protocol_version
    }
    pub fn request_extended_measurements(
        &mut self,
        handle: &BearerHandle,
        order: ExtendedMeasurementOrder,
        monotonic_ms: u64,
    ) -> Result<MeasurementHandle, SessionError> {
        self.check_bearer(handle)?;
        self.check_clock(monotonic_ms)?;
        let version = self.legs[handle.party.index()]
            .protocol_version
            .ok_or(SessionError::ProtocolContext)?;
        order.validate_version(version).map_err(|_| {
            SessionError::Order(crate::link::traffic_order::OrderError::MeasurementFields)
        })?;
        self.advance(monotonic_ms)?;
        if self.state != CallState::Connected {
            return Err(SessionError::State);
        }
        let leg = &mut self.legs[handle.party.index()];
        if leg.alignment_measurements != AlignmentState::Unrestricted
            && !order.targets.channels().is_empty()
        {
            return Err(SessionError::Control(TrafficMessageError::Field(
                "neighbor_measurement_alignment",
            )));
        }
        if let Some(current) = &leg.measurements {
            if !matches!(
                current.state(),
                OrderState::TimedOut | OrderState::Cancelled
            ) && current.extended_order() == Some(&order)
            {
                return leg
                    .extended_measurements
                    .as_ref()
                    .map(|active| active.handle.clone())
                    .ok_or(SessionError::State);
            }
            if matches!(
                current.state(),
                OrderState::Prepared | OrderState::AwaitingAck { .. }
            ) {
                return Err(SessionError::State);
            }
        }
        let replacement = TrafficOrderExchange::try_new_extended_measurements(
            leg.subscriber.identity,
            order,
            version,
            self.policy.measurements,
            monotonic_ms,
        )
        .map_err(SessionError::Order)?;
        let owner = MeasurementHandle {
            bearer: handle.clone(),
            token: Arc::new(()),
        };
        leg.measurements = Some(replacement);
        leg.extended_measurements = Some(Active {
            handle: owner.clone(),
            snapshot: None,
        });
        Ok(owner)
    }
    pub fn extended_measurements(&self, party: Party) -> Option<&ExtendedMeasurementSnapshot> {
        self.legs
            .get(party.index())?
            .extended_measurements
            .as_ref()?
            .snapshot
            .as_ref()
    }
    /// Capture at the start of report assembly. A replacement or stop invalidates the returned owner.
    pub fn active_measurement_handle(&self, party: Party) -> Option<MeasurementHandle> {
        let active = self
            .legs
            .get(party.index())?
            .extended_measurements
            .as_ref()?;
        active.snapshot.as_ref()?;
        Some(active.handle.clone())
    }
    /// Supply a CRC-validated complete report with the owner captured when assembly began. Wire reports contain no request ID.
    pub fn record_extended_measurement(
        &mut self,
        owner: &MeasurementHandle,
        report: &ExtendedChannelQualityReport,
        monotonic_ms: u64,
    ) -> Result<(), SessionError> {
        self.check_bearer(&owner.bearer)?;
        self.advance(monotonic_ms)?;
        if self.state != CallState::Connected {
            return Err(SessionError::State);
        }
        let leg = &mut self.legs[owner.bearer.party.index()];
        let active = leg
            .extended_measurements
            .as_mut()
            .ok_or(SessionError::State)?;
        if !Arc::ptr_eq(&active.handle.token, &owner.token) {
            return Err(SessionError::State);
        }
        let snapshot = active.snapshot.as_mut().ok_or(SessionError::State)?;
        let (start, _) = report.neighbors();
        if start != 0 && start >= snapshot.targets.channels().len() {
            return Err(SessionError::Control(TrafficMessageError::Field(
                "unexpected_extended_report_group",
            )));
        }
        let resolved = snapshot
            .targets
            .resolve(report)
            .map_err(SessionError::Control)?;
        if let Some(serving) = resolved.serving {
            snapshot.serving = Some(serving);
        }
        for (offset, value) in resolved.neighbors.into_iter().enumerate() {
            snapshot.neighbor_rssi_codes[start + offset] = Some(value.rssi_code);
        }
        snapshot.received_monotonic_ms = Some(monotonic_ms);
        Ok(())
    }
}
