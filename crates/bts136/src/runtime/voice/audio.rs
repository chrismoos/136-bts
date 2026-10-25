use super::*;
impl VoiceRuntime {
    pub(crate) fn begin_audio_call(
        &mut self,
        registry: &LocalRegistry,
        identity: crate::link::rach::MobileIdentity,
        calling_number: &str,
        now_ms: u64,
    ) -> Result<u64, Box<dyn Error>> {
        let phase = self.allocation.phase(identity)?;
        let transmission = paging::Transmission::new(identity, paging::message(calling_number)?)?;
        let request = self.paging.begin_audio(
            registry,
            identity,
            calling_number,
            now_ms,
            super::super::timers::ORIGINATION_RESPONSE_MS,
        )?;
        let call_id = request.handle.id().value();
        self.rejections
            .retain(|rejection| rejection.identity != identity);
        self.pages.push(Page {
            request,
            started_ms: now_ms,
            phase,
            transmission,
        });
        tracing::info!(
            call_id,
            %identity,
            calling_number,
            "network audio call enters paging"
        );
        Ok(call_id)
    }
    pub(super) fn audio_operation_calls(&self, now_ms: u64) -> Vec<crate::operations::proto::Call> {
        use crate::{
            core::calls::AudioDirection,
            operations::{
                metric,
                proto::{Call, CallLeg},
            },
        };
        self.paging
            .audio_reservations()
            .map(|reservation| {
                let page = self
                    .pages
                    .iter()
                    .find(|page| page.request.handle.matches(&reservation.handle));
                let paging = page.is_some();
                let active = self
                    .calls
                    .iter()
                    .find(|active| active.session.handle().matches(&reservation.handle));
                let started_ms = active
                    .map(|active| active.started_ms)
                    .or(page.map(|page| page.started_ms))
                    .unwrap_or(now_ms);
                let party = match reservation.direction {
                    AudioDirection::MobileOriginated => Party::Caller,
                    AudioDirection::MobileTerminated => Party::Callee,
                };
                let mut measurements = vec![metric("Codec", reservation.mobile.codec.label(), "")];
                if let Some(active) = active {
                    let leg = &active.legs[index(party)];
                    status::append_metrics(&mut measurements, &leg.measurements, now_ms);
                    measurements.push(metric("In-flight TX slots", leg.in_flight, ""));
                }
                let mobile = CallLeg {
                    party: format!("{party:?}"),
                    identity: format!("{:?}", reservation.mobile.identity),
                    number: reservation.mobile.number.clone(),
                    channel: u32::from(reservation.mobile.channel),
                    slots: format!("{:?}", reservation.mobile.assigned_slots),
                    phase: active.map_or(
                        if paging {
                            crate::operations::proto::BearerPhase::Paging
                        } else {
                            crate::operations::proto::BearerPhase::Reserved
                        } as i32,
                        |active| {
                            crate::operations::bearer_phase(active.session.bearer_state(party))
                                as i32
                        },
                    ),
                    source: String::new(),
                    measurements,
                };
                let local = CallLeg {
                    party: if party == Party::Caller {
                        "Callee"
                    } else {
                        "Caller"
                    }
                    .into(),
                    identity: String::new(),
                    number: reservation.remote_number.clone(),
                    channel: 0,
                    slots: String::new(),
                    phase: crate::operations::proto::BearerPhase::Unspecified as i32,
                    source: if self
                        .sip_calls
                        .iter()
                        .any(|binding| binding.call_id == reservation.handle.id().value())
                        || active.is_some_and(|a| {
                            a.legs
                                .iter()
                                .any(|leg| matches!(leg.source, media::Source::Sip { .. }))
                        }) {
                        "SIP"
                    } else {
                        "Sample audio"
                    }
                    .into(),
                    measurements: Vec::new(),
                };
                let legs = if party == Party::Caller {
                    vec![mobile, local]
                } else {
                    vec![local, mobile]
                };
                Call {
                    id: reservation.handle.id().value(),
                    phase: active.map_or(
                        if paging {
                            crate::operations::proto::CallPhase::Paging
                        } else {
                            crate::operations::proto::CallPhase::Establishing
                        } as i32,
                        |active| crate::operations::call_phase(active.session.state()).0 as i32,
                    ),
                    end_reason: active.map_or_else(String::new, |active| {
                        crate::operations::call_phase(active.session.state()).1
                    }),
                    legs,
                    measurements: vec![metric("Duration", now_ms.saturating_sub(started_ms), "ms")],
                }
            })
            .collect()
    }
}
