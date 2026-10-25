use super::*;
use crate::{
    core::{registry::ProvisionedSubscriber, store::DeviceSubscriber},
    l3::registration::{RejectCause, SerialNumber},
};

const SERIAL_NUMBER_MESSAGE_TYPE: u8 = 13;

impl LocalMessaging {
    pub(crate) fn devices(&self) -> &[DeviceSubscriber] {
        &self.devices
    }

    pub(crate) fn save_device(
        &mut self,
        esn: u32,
        number: String,
        requested_identity: Option<MobileIdentity>,
    ) -> Result<(), MessagingError> {
        if esn == 0 {
            return Err(MessagingError::DeviceProvisioning("ESN must be nonzero"));
        }
        if number.is_empty() || !number.bytes().all(|digit| digit.is_ascii_digit()) {
            return Err(RegistryError::Number.into());
        }
        let existing = self.devices.iter().find(|device| device.esn == esn);
        let identity = existing
            .and_then(|device| device.identity)
            .or(requested_identity)
            .or_else(|| self.registry.unprovisioned_by_esn(esn));
        if existing
            .and_then(|device| device.identity)
            .is_some_and(|old| requested_identity.is_some_and(|requested| requested != old))
        {
            return Err(MessagingError::DeviceProvisioning(
                "The learned mobile identity cannot be changed while editing",
            ));
        }
        if self.devices.iter().any(|device| {
            device.esn != esn
                && (device.number == number || identity.is_some() && device.identity == identity)
        }) {
            return Err(MessagingError::DeviceProvisioning(
                "The number or mobile identity belongs to another ESN",
            ));
        }
        if let Some(identity) = identity {
            let subscriber = self
                .registry
                .by_identity(identity)
                .or_else(|| self.registry.unprovisioned_phone(identity))
                .ok_or(RegistryError::UnknownSubscriber)?;
            if subscriber
                .registration
                .and_then(|registration| registration.esn)
                .is_some_and(|reported| reported != esn)
            {
                return Err(MessagingError::DeviceProvisioning(
                    "ESN differs from the handset's reported serial number",
                ));
            }
        }
        if self.registry.subscribers().iter().any(|subscriber| {
            subscriber.provisioning.number == number
                && Some(subscriber.provisioning.identity) != identity
        }) {
            return Err(RegistryError::DuplicateNumber.into());
        }
        self.store.save_device_subscriber(&DeviceSubscriber {
            esn,
            number,
            identity,
        })?;
        self.refresh_devices()
    }

    pub(crate) fn delete_device(&mut self, esn: u32) -> Result<(), MessagingError> {
        let device = self
            .devices
            .iter()
            .find(|device| device.esn == esn)
            .ok_or(RegistryError::UnknownSubscriber)?;
        self.store.disable_device_subscriber(device)?;
        self.refresh_devices()
    }

    fn refresh_devices(&mut self) -> Result<(), MessagingError> {
        self.registry
            .replace_provisioning(self.store.enabled_subscribers()?)?;
        self.devices = self.store.device_subscribers()?;
        Ok(())
    }

    pub(crate) fn observe_call_access(
        &mut self,
        set: &MessageSet,
        superframe: u64,
    ) -> Result<bool, MessagingError> {
        let activity = super::super::registry::CallActivity::decode(set)?;
        let esn = activity.serial.as_ref().map(|serial| serial.esn);
        let bound = self
            .devices
            .iter()
            .find(|device| device.identity == Some(set.identity));
        let device = esn.and_then(|esn| self.devices.iter().find(|device| device.esn == esn));
        if bound.is_some_and(|bound| Some(bound.esn) != esn)
            || device.is_some_and(|device| {
                device
                    .identity
                    .is_some_and(|identity| identity != set.identity)
            })
        {
            return Err(RegistryError::SerialConflict.into());
        }
        if let Some(device) = device.filter(|device| device.identity.is_none()).cloned() {
            if self.registry.by_identity(set.identity).is_some() {
                return Err(RegistryError::SerialConflict.into());
            }
            let device = DeviceSubscriber {
                identity: Some(set.identity),
                ..device
            };
            self.store.save_device_subscriber(&device)?;
            self.refresh_devices()?;
        }
        Ok(self
            .registry
            .observe_call_activity(set.identity, activity, superframe)?)
    }

    pub(crate) fn expire_observed_access(
        &mut self,
        superframe: u64,
        busy: impl Fn(MobileIdentity) -> bool,
    ) -> Vec<MobileIdentity> {
        self.registry.expire_observed_access(superframe, busy)
    }

    pub(super) fn resolve_device_registration(
        &mut self,
        set: &MessageSet,
        superframe: u64,
    ) -> Result<Option<RegistrationReply>, MessagingError> {
        let serial = set
            .messages
            .iter()
            .find(|bytes| bytes.first() == Some(&SERIAL_NUMBER_MESSAGE_TYPE))
            .map(|bytes| SerialNumber::decode(bytes))
            .transpose()
            .map_err(RegistryError::Options)?;
        let bound = self
            .devices
            .iter()
            .find(|device| device.identity == Some(set.identity));
        let device = serial
            .as_ref()
            .and_then(|serial| self.devices.iter().find(|device| device.esn == serial.esn));
        let conflict = bound
            .is_some_and(|bound| serial.as_ref().is_none_or(|serial| bound.esn != serial.esn))
            || device.is_some_and(|device| {
                device
                    .identity
                    .is_some_and(|identity| identity != set.identity)
            });
        if conflict {
            tracing::warn!(identity=%set.identity, esn=?serial.as_ref().map(|serial| serial.esn), "Registration conflicts with provisioned ESN binding");
            return Ok(Some(device_reject(set.identity)));
        }
        let Some(device) = device.filter(|device| device.identity.is_none()).cloned() else {
            return Ok(None);
        };
        if self.registry.by_identity(set.identity).is_some() {
            tracing::warn!(identity=%set.identity, esn=device.esn, "ESN registration cannot claim an existing subscriber identity");
            return Ok(Some(device_reject(set.identity)));
        }
        let mut provisioning: Vec<_> = self
            .registry
            .subscribers()
            .iter()
            .map(|subscriber| subscriber.provisioning.clone())
            .collect();
        provisioning.push(ProvisionedSubscriber {
            identity: set.identity,
            number: device.number.clone(),
        });
        let mut candidate = LocalRegistry::try_new(provisioning)?;
        let reply = candidate.register(set, superframe)?;
        if reply.message != RegistrationResponse::Accept {
            return Ok(Some(reply));
        }
        let device = DeviceSubscriber {
            identity: Some(set.identity),
            ..device
        };
        self.store.save_device_subscriber(&device)?;
        self.refresh_devices()?;
        tracing::info!(esn=format_args!("0x{:08X}",device.esn), identity=%set.identity, number=device.number, "Learned mobile identity for provisioned ESN");
        Ok(None)
    }
}

fn device_reject(identity: MobileIdentity) -> RegistrationReply {
    RegistrationReply {
        identity,
        message: RegistrationResponse::Reject {
            cause: RejectCause::UnknownMsid,
        },
        options: Default::default(),
    }
}

#[cfg(test)]
mod tests;
