use super::{Error, Procedure, classify_primary};
use crate::{
    core::maca::AccessTrigger,
    l3::{
        broadcast::ProtocolVersion,
        maca::MacaReport,
        registration::{AccessMessage, RegistrationType},
    },
    link::rach::MessageSet,
};
use std::borrow::Cow;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacaSource {
    Broadcast(AccessTrigger),
    QueueNotification,
}
pub(crate) struct MacaCompanion {
    pub source: MacaSource,
    pub version: Option<ProtocolVersion>,
    pub report: MacaReport,
}
pub(crate) struct ParsedAccess<'a> {
    pub primary: Cow<'a, MessageSet>,
    pub procedure: Procedure,
    pub maca: Option<MacaCompanion>,
    pub audit: Option<crate::l3::audit::AuditConfirmation>,
}
pub(crate) fn parse(set: &MessageSet) -> Result<ParsedAccess<'_>, Error> {
    if set.messages.len() > 1
        && set.messages[0].first() == Some(&crate::l3::audit::CONFIRMATION_TYPE)
        && set.messages[1].first() == Some(&super::REGISTRATION)
    {
        let audit =
            crate::l3::audit::AuditConfirmation::decode(&set.messages[0]).map_err(Error::Audit)?;
        let registration =
            crate::l3::registration::decode_prefix(&set.messages[1]).map_err(Error::Wire)?;
        if !matches!(
            registration.message,
            AccessMessage::Registration {
                registration_type: RegistrationType::Forced,
                ..
            }
        ) {
            return Err(Error::Concatenation);
        }
        let remaining = MessageSet {
            identity: set.identity,
            messages: set.messages[1..].to_vec(),
        };
        let parsed = parse(&remaining)?;
        return Ok(ParsedAccess {
            primary: Cow::Owned(parsed.primary.into_owned()),
            procedure: parsed.procedure,
            maca: parsed.maca,
            audit: Some(audit),
        });
    }
    let mut report = None;
    for (index, bytes) in set.messages.iter().enumerate() {
        if bytes.first() != Some(&crate::l3::maca::MESSAGE_TYPE) {
            continue;
        }
        if index == 0 || report.is_some() {
            return Err(Error::Concatenation);
        }
        report = Some(MacaReport::decode(bytes).map_err(Error::Maca)?);
    }
    let primary = if report.is_some() {
        Cow::Owned(MessageSet {
            identity: set.identity,
            messages: set
                .messages
                .iter()
                .filter(|bytes| bytes.first() != Some(&crate::l3::maca::MESSAGE_TYPE))
                .cloned()
                .collect(),
        })
    } else {
        Cow::Borrowed(set)
    };
    let procedure = classify_primary(&primary)?;
    let Some(report) = report else {
        if procedure == Procedure::QueuedMaca {
            return Err(Error::Concatenation);
        }
        return Ok(ParsedAccess {
            primary,
            procedure,
            maca: None,
            audit: None,
        });
    };
    // TIA-136-123-E §8 Table 15 excludes R-DATA and Power Down Registration from MACA concatenation.
    let (source, version) = match procedure {
        Procedure::Registration => {
            let decoded = crate::l3::registration::decode_prefix(&primary.messages[0])
                .map_err(Error::Wire)?;
            let AccessMessage::Registration {
                registration_type,
                protocol_version,
                ..
            } = decoded.message
            else {
                return Err(Error::Concatenation);
            };
            if registration_type == RegistrationType::PowerDown {
                return Err(Error::Concatenation);
            }
            (
                MacaSource::Broadcast(AccessTrigger::Registration),
                Some(protocol_version),
            )
        }
        Procedure::Origination => {
            let message = crate::l3::origination::decode_prefix(&primary.messages[0])
                .map_err(|_| Error::Concatenation)?
                .message;
            (
                MacaSource::Broadcast(AccessTrigger::Origination),
                Some(message.protocol_version),
            )
        }
        Procedure::PageResponse => {
            let message = crate::l3::page_response::decode_prefix(&primary.messages[0])
                .map_err(|_| Error::Concatenation)?
                .message;
            (
                MacaSource::Broadcast(AccessTrigger::PageResponse),
                Some(message.protocol_version),
            )
        }
        Procedure::Audit => (MacaSource::Broadcast(AccessTrigger::Audit), None),
        Procedure::QueuedMaca => (MacaSource::QueueNotification, None),
        _ => return Err(Error::Concatenation),
    };
    if let (Some(version), Some(format)) = (version, report.measurements.format()) {
        report
            .validate_negotiation(version, format)
            .map_err(Error::Maca)?;
    }
    Ok(ParsedAccess {
        primary,
        procedure,
        maca: Some(MacaCompanion {
            source,
            version,
            report,
        }),
        audit: None,
    })
}

#[cfg(test)]
mod tests;
