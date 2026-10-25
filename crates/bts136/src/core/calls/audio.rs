use super::*;
use crate::codec::speech::Codec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDirection {
    MobileOriginated,
    MobileTerminated,
}

#[derive(Debug, Clone)]
pub struct AudioReservation {
    pub handle: CallHandle,
    pub mobile: ReservedParty,
    pub direction: AudioDirection,
    pub remote_number: String,
}

#[derive(Debug, Clone)]
pub enum OriginationRoute {
    Mobile(CallReservation),
    SampleAudio(AudioReservation),
}

pub(super) struct ReservedAudioCall {
    reservation: AudioReservation,
    lease: TrafficLease,
}

impl CallReservations {
    pub fn audio_reservations(&self) -> impl Iterator<Item = &AudioReservation> {
        self.audio.iter().map(|call| &call.reservation)
    }

    pub fn route_origination(
        &mut self,
        registry: &LocalRegistry,
        set: &MessageSet,
    ) -> Result<OriginationRoute, CallError> {
        let call = originating_request(registry, set)?;
        if registry.by_number(&call.destination).is_some() {
            return self
                .reserve_originating(registry, call.caller, &call.destination)
                .map(OriginationRoute::Mobile);
        }
        self.reserve_audio_station(
            call.caller,
            &call.destination,
            AudioDirection::MobileOriginated,
        )
        .map(OriginationRoute::SampleAudio)
    }

    pub fn reserve_audio(
        &mut self,
        registry: &LocalRegistry,
        identity: MobileIdentity,
        remote_number: &str,
        direction: AudioDirection,
        codec: Option<Codec>,
    ) -> Result<AudioReservation, CallError> {
        if !(remote_number.is_empty() && direction == AudioDirection::MobileTerminated)
            && !crate::sip::number(remote_number)
        {
            return Err(CallError::Number);
        }
        let subscriber = registry
            .subscribers()
            .iter()
            .find(|subscriber| subscriber.provisioning.identity == identity)
            .ok_or(CallError::UnknownCaller)?;
        if subscriber.registration.is_none() && direction == AudioDirection::MobileOriginated {
            return Err(CallError::CallerOffline);
        }
        if self.busy(identity) {
            return Err(CallError::Busy(identity));
        }
        let supported_codecs = reported_mask(subscriber);
        let codec = codec.map_or_else(|| reported_codec(subscriber), Ok)?;
        if codec.capability_mask() & supported_codecs == 0 {
            return Err(CallError::Admission(AdmissionError::Codec));
        }
        let station = CallStation {
            identity,
            number: subscriber.provisioning.number.clone(),
            codec,
            supported_codecs,
            protocol_version: subscriber
                .registration
                .map(|registration| registration.protocol_version),
        };
        self.reserve_audio_station(station, remote_number, direction)
    }

    fn reserve_audio_station(
        &mut self,
        station: CallStation,
        remote_number: &str,
        direction: AudioDirection,
    ) -> Result<AudioReservation, CallError> {
        if self.busy(station.identity) {
            return Err(CallError::Busy(station.identity));
        }
        let next_id = self.next_id.checked_add(1).ok_or(CallError::IdExhausted)?;
        let lease = self
            .resources
            .allocate_full_rate()
            .map_err(CallError::Allocation)?;
        let reservation = AudioReservation {
            handle: CallHandle {
                id: CallId(self.next_id),
                token: Arc::new(()),
            },
            mobile: ReservedParty {
                identity: station.identity,
                number: station.number,
                channel: lease.channel(),
                assigned_slots: lease.assigned_slots(),
                codec: station.codec,
                supported_codecs: station.supported_codecs,
                protocol_version: station.protocol_version,
            },
            direction,
            remote_number: remote_number.to_owned(),
        };
        self.audio.push(ReservedAudioCall {
            reservation: reservation.clone(),
            lease,
        });
        self.next_id = next_id;
        Ok(reservation)
    }

    pub fn select_audio_codec(
        &mut self,
        handle: &CallHandle,
        options: &crate::l3::access_call::CallOptions,
    ) -> Result<AudioReservation, CallError> {
        let call = self
            .audio
            .iter_mut()
            .find(|call| call.reservation.handle.matches(handle))
            .ok_or(CallError::StaleHandle)?;
        let supported: Vec<_> = [Codec::AcelpCc1, Codec::Vselp]
            .into_iter()
            .filter(|codec| call.reservation.mobile.supported_codecs & codec.capability_mask() != 0)
            .collect();
        call.reservation.mobile.codec = options
            .select_codec(&supported)
            .map_err(CallError::Admission)?;
        Ok(call.reservation.clone())
    }
    /// Release only before assignment starts or after radio work and completions have drained.
    pub fn release_audio(&mut self, handle: &CallHandle) -> Result<AudioReservation, CallError> {
        let index = self
            .audio
            .iter()
            .position(|call| call.reservation.handle.matches(handle))
            .ok_or(CallError::StaleHandle)?;
        self.resources
            .release(&self.audio[index].lease)
            .map_err(CallError::Allocation)?;
        Ok(self.audio.remove(index).reservation)
    }

    pub(super) fn busy(&self, identity: MobileIdentity) -> bool {
        self.calls.iter().any(|call| {
            call.reservation.caller.identity == identity
                || call.reservation.callee.identity == identity
        }) || self
            .audio
            .iter()
            .any(|call| call.reservation.mobile.identity == identity)
    }
}

#[cfg(test)]
mod tests;
