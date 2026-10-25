use super::*;
use crate::l3::relay::addressing::RelayAddresses;
use serde::{Deserialize, Serialize};

const FORMAT_VERSION: u8 = 1;
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Context {
    format: u8,
    recipient: Option<Recipient>,
    number: String,
    pub sender_number: String,
    esn: Option<u32>,
    maximum_message_octets: usize,
    server: Option<Vec<u8>>,
    originating: Option<Vec<u8>>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
enum Recipient {
    Tmsi20(u32),
    Tmsi24(u32),
    Min34(u64),
    Imsi50(u64),
}
impl From<MobileIdentity> for Recipient {
    fn from(peer: MobileIdentity) -> Self {
        match peer {
            MobileIdentity::Tmsi20(value) => Self::Tmsi20(value),
            MobileIdentity::Tmsi24(value) => Self::Tmsi24(value),
            MobileIdentity::Min34(value) => Self::Min34(value),
            MobileIdentity::Imsi50(value) => Self::Imsi50(value),
        }
    }
}
impl Recipient {
    fn identity(&self) -> MobileIdentity {
        match *self {
            Self::Tmsi20(value) => MobileIdentity::Tmsi20(value),
            Self::Tmsi24(value) => MobileIdentity::Tmsi24(value),
            Self::Min34(value) => MobileIdentity::Min34(value),
            Self::Imsi50(value) => MobileIdentity::Imsi50(value),
        }
    }
}
fn wire(
    address: &Option<crate::l3::address::AddressInfo>,
) -> Result<Option<Vec<u8>>, IncomingSmsError> {
    address
        .as_ref()
        .map(|address| address.encode().map_err(|_| IncomingSmsError::Context))
        .transpose()
}
impl Context {
    pub(super) fn new(
        core: &LocalMessaging,
        peer: MobileIdentity,
        addresses: &RelayAddresses,
    ) -> Result<Self, IncomingSmsError> {
        let destination = core
            .bind_sms_destination(
                addresses
                    .destination
                    .as_ref()
                    .ok_or(IncomingSmsError::Context)?,
            )
            .map_err(IncomingSmsError::Messaging)?;
        let subscriber = core
            .registry()
            .subscribers()
            .iter()
            .find(|subscriber| subscriber.provisioning.identity == peer)
            .ok_or(IncomingSmsError::Context)?;
        let registration = subscriber.registration.ok_or(IncomingSmsError::Context)?;
        Ok(Self {
            format: FORMAT_VERSION,
            recipient: destination.recipient().map(Into::into),
            number: destination.number().into(),
            sender_number: subscriber.provisioning.number.clone(),
            esn: registration.esn,
            maximum_message_octets: LIMITS.maximum_message_octets,
            server: wire(&addresses.server)?,
            originating: wire(&addresses.originating)?,
        })
    }
    pub(super) fn destination(
        &self,
    ) -> Result<crate::core::messaging::SmsDestination, IncomingSmsError> {
        match &self.recipient {
            Some(recipient) => crate::core::messaging::SmsDestination::try_from_binding(
                recipient.identity(),
                self.number.clone(),
            ),
            None => crate::core::messaging::SmsDestination::external(self.number.clone()),
        }
        .map_err(IncomingSmsError::Messaging)
    }
    pub(super) fn encode(&self) -> Result<Vec<u8>, IncomingSmsError> {
        serde_yaml_ng::to_string(self)
            .map(|text| text.into_bytes())
            .map_err(|_| IncomingSmsError::Context)
    }
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, IncomingSmsError> {
        let context: Self =
            serde_yaml_ng::from_slice(bytes).map_err(|_| IncomingSmsError::Context)?;
        if context.format != FORMAT_VERSION
            || context.maximum_message_octets != LIMITS.maximum_message_octets
            || context.sender_number.is_empty()
            || !context
                .sender_number
                .bytes()
                .all(|digit| digit.is_ascii_digit())
        {
            return Err(IncomingSmsError::Context);
        }
        context.destination()?;
        Ok(context)
    }
    pub(super) fn matches(
        &self,
        core: &LocalMessaging,
        addresses: &RelayAddresses,
        esn: Option<u32>,
    ) -> Result<bool, IncomingSmsError> {
        if self
            .esn
            .zip(esn)
            .is_some_and(|(before, after)| before != after)
        {
            return Ok(false);
        }
        if let Some(address) = &addresses.destination {
            let number = crate::core::messaging::directory_number(address)
                .map_err(IncomingSmsError::Messaging)?;
            if number != self.number {
                return Ok(false);
            }
            if let Some(recipient) = &self.recipient {
                let current = core
                    .bind_sms_destination(address)
                    .map_err(IncomingSmsError::Messaging)?;
                if current.recipient() != Some(recipient.identity()) {
                    return Ok(false);
                }
            }
        }
        if addresses.server.is_some() && wire(&addresses.server)? != self.server {
            return Ok(false);
        }
        if addresses.originating.is_some() && wire(&addresses.originating)? != self.originating {
            return Ok(false);
        }
        Ok(true)
    }
}
