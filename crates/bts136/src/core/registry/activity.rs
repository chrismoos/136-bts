use super::*;
use crate::{codec::speech::Codec, l3::access_call};

const OBSERVED_REACHABILITY_LIFETIME_SUPERFRAMES: u64 = 1880;
const SERIAL_NUMBER_MESSAGE_TYPE: u8 = 13;

pub(crate) struct CallActivity {
    protocol_version: ProtocolVersion,
    scm: u8,
    pub serial: Option<registration::SerialNumber>,
    voice_coder_map: u8,
}

impl CallActivity {
    pub(crate) fn decode(set: &MessageSet) -> Result<Self, RegistryError> {
        let (_, width, value) = set.identity.parts();
        if value == 0 || value >= 1 << width {
            return Err(RegistryError::Identity);
        }
        let primary = set
            .messages
            .first()
            .ok_or(RegistryError::MissingRegistration)?;
        let (kind, protocol_version, scm, options) = match primary.first() {
            Some(&access_call::ORIGINATION_TYPE) => {
                let request = access_call::OriginationRequest::decode(primary)
                    .map_err(RegistryError::Options)?;
                (
                    access_call::Kind::Origination,
                    request.message.protocol_version,
                    request.message.scm,
                    request.options,
                )
            }
            Some(&access_call::PAGE_RESPONSE_TYPE) => {
                let request = access_call::PageResponseRequest::decode(primary)
                    .map_err(RegistryError::Options)?;
                (
                    access_call::Kind::PageResponse,
                    request.message.protocol_version,
                    request.message.scm,
                    request.options,
                )
            }
            _ => {
                return Err(RegistryError::Options(OptionsError::Field(
                    "call_primary_order",
                )));
            }
        };
        access_call::validate_companions(set, kind, protocol_version)
            .map_err(RegistryError::Options)?;
        options
            .select_codec(&[Codec::AcelpCc1, Codec::Vselp])
            .map_err(RegistryError::CallAdmission)?;
        let voice_coder_map = [Codec::AcelpCc1, Codec::Vselp]
            .into_iter()
            .filter(|codec| options.select_codec(&[*codec]).is_ok())
            .fold(0, |mask, codec| mask | codec.capability_mask());
        let serial = set
            .messages
            .iter()
            .find(|bytes| bytes.first() == Some(&SERIAL_NUMBER_MESSAGE_TYPE))
            .map(|bytes| registration::SerialNumber::decode(bytes))
            .transpose()
            .map_err(RegistryError::Options)?;
        Ok(Self {
            protocol_version,
            scm,
            serial,
            voice_coder_map,
        })
    }
}

impl LocalRegistry {
    pub(crate) fn observe_call_activity(
        &mut self,
        identity: MobileIdentity,
        activity: CallActivity,
        superframe: u64,
    ) -> Result<bool, RegistryError> {
        let Some(subscriber) = self
            .subscribers
            .iter_mut()
            .find(|subscriber| subscriber.provisioning.identity == identity)
        else {
            return Ok(false);
        };
        let esn = activity.serial.as_ref().map(|serial| serial.esn);
        let previous_esn = subscriber
            .registration
            .and_then(|registration| registration.esn);
        if previous_esn.is_some() && esn.is_some() && previous_esn != esn {
            return Err(RegistryError::SerialConflict);
        }
        if let Some(registration) = &mut subscriber.registration {
            if let Some(last_seen) = &mut registration.last_observed_access_superframe {
                *last_seen = superframe;
                registration.protocol_version = activity.protocol_version;
                registration.scm = activity.scm;
            }
            registration.esn = esn.or(previous_esn);
            registration.access_voice_coder_map = Some(activity.voice_coder_map);
        } else {
            subscriber.registration = Some(Registration {
                protocol_version: activity.protocol_version,
                scm: activity.scm,
                esn,
                last_registration_superframe: superframe,
                last_observed_access_superframe: Some(superframe),
                access_voice_coder_map: Some(activity.voice_coder_map),
            });
            self.registration_owners.insert(identity, Arc::new(()));
        }
        if let Some(serial) = activity.serial {
            subscriber.reported_serial = Some(serial);
        }
        Ok(true)
    }

    pub(crate) fn expire_observed_access(
        &mut self,
        superframe: u64,
        busy: impl Fn(MobileIdentity) -> bool,
    ) -> Vec<MobileIdentity> {
        let mut expired = Vec::new();
        for subscriber in &mut self.subscribers {
            let Some(observed) = subscriber
                .registration
                .as_mut()
                .and_then(|registration| registration.last_observed_access_superframe.as_mut())
            else {
                continue;
            };
            let identity = subscriber.provisioning.identity;
            if busy(identity) {
                *observed = superframe;
            } else if superframe.saturating_sub(*observed)
                >= OBSERVED_REACHABILITY_LIFETIME_SUPERFRAMES
            {
                clear_registration(subscriber);
                self.registration_owners.remove(&identity);
                expired.push(identity);
            }
        }
        expired
    }
}

#[cfg(test)]
mod tests;
