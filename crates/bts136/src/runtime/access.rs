pub(crate) mod maca;
use crate::{
    core::{
        call_paging::{CallPaging, PagingError, PagingOutcome, PagingRequest},
        registry::{LocalRegistry, RegistrationReply, RegistryError},
    },
    link::rach::MessageSet,
};
pub(crate) use maca::parse;
use std::{error::Error as StdError, fmt};
// IS-136.1-A §6.4.4 defines these access message types.
const ORIGINATION: u8 = 7;
const PAGE_RESPONSE: u8 = 8;
const REGISTRATION: u8 = 12;
const SERIAL_NUMBER: u8 = 13;
const CAPABILITY_REPORT: u8 = crate::l3::capability_report::MESSAGE_TYPE;
#[derive(Debug)]
pub enum Error {
    Empty,
    Unsupported(u8),
    MixedProcedures,
    Concatenation,
    MissingProcedure,
    MissingPage,
    AmbiguousPage,
    Registry(RegistryError),
    Wire(crate::l3::registration::RegistrationError),
    Capability(crate::l3::capability_report::ReportError),
    Maca(crate::l3::maca::Error),
    Serial(crate::l3::registration::OptionsError),
    Audit(crate::l3::audit::AuditError),
    Confirmation(crate::l3::message_waiting::Error),
    Paging(PagingError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "access dispatch failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug)]
pub enum Dispatch {
    Registration(RegistrationReply),
    Origination(PagingRequest),
    PageResponse(PagingOutcome),
    Capabilities,
    Audit(crate::l3::audit::AuditConfirmation),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Procedure {
    Registration,
    Origination,
    PageResponse,
    Sms,
    MessageWaiting,
    UserAlert,
    Capabilities,
    Audit,
    QueuedMaca,
}
pub fn classify(set: &MessageSet) -> Result<Procedure, Error> {
    Ok(parse(set)?.procedure)
}
fn classify_primary(set: &MessageSet) -> Result<Procedure, Error> {
    if set.messages.len() == 1
        && set.messages[0].first() == Some(&crate::l3::audit::CONFIRMATION_TYPE)
    {
        crate::l3::audit::AuditConfirmation::decode(&set.messages[0]).map_err(Error::Audit)?;
        return Ok(Procedure::Audit);
    }

    if set.messages.len() == 1 && set.messages[0].first() == Some(&CAPABILITY_REPORT) {
        crate::l3::capability_report::decode(&set.messages[0]).map_err(Error::Capability)?;
        return Ok(Procedure::Capabilities);
    }

    const RDATA: u8 = 9;
    const RACCEPT: u8 = 10;
    const RREJECT: u8 = 11;
    const SPACH_CONFIRMATION: u8 = 15;
    let mut procedure = None;
    let mut serial_seen = false;
    let mut capability_seen = false;
    for message in &set.messages {
        let header = *message.first().ok_or(Error::Empty)?;
        let kind = match header {
            REGISTRATION => Some(Procedure::Registration),
            ORIGINATION => Some(Procedure::Origination),
            PAGE_RESPONSE => Some(Procedure::PageResponse),
            RDATA | RACCEPT | RREJECT => Some(Procedure::Sms),
            SPACH_CONFIRMATION => {
                let confirmation = crate::l3::notification::SpachConfirmation::decode(message)
                    .map_err(Error::Confirmation)?;
                match confirmation.confirmed_type {
                    crate::l3::message_waiting::MESSAGE_TYPE => Some(Procedure::MessageWaiting),
                    crate::l3::user_alert::MESSAGE_TYPE => Some(Procedure::UserAlert),
                    crate::l3::notification::SMS_TYPE => Some(Procedure::Sms),
                    crate::l3::notification::MACA_TYPE => Some(Procedure::QueuedMaca),
                    kind => return Err(Error::Unsupported(kind)),
                }
            }
            CAPABILITY_REPORT => {
                if procedure != Some(Procedure::Registration) || capability_seen {
                    return Err(Error::Concatenation);
                }
                crate::l3::capability_report::decode(message).map_err(Error::Capability)?;
                let primary = crate::l3::registration::decode_prefix(&set.messages[0])
                    .map_err(Error::Wire)?;
                if !matches!(
                    primary.message,
                    crate::l3::registration::AccessMessage::Registration {
                        registration_type: crate::l3::registration::RegistrationType::PowerUp
                            | crate::l3::registration::RegistrationType::NewSystem
                            | crate::l3::registration::RegistrationType::Forced,
                        ..
                    }
                ) {
                    return Err(Error::Concatenation);
                }
                capability_seen = true;
                None
            }
            SERIAL_NUMBER => {
                if procedure.is_none()
                    || matches!(set.messages[0].first().copied(), Some(RACCEPT | RREJECT))
                    || matches!(
                        procedure,
                        Some(Procedure::MessageWaiting | Procedure::UserAlert)
                    )
                    || serial_seen
                {
                    return Err(Error::Concatenation);
                }
                serial_seen = true;
                let decoded = crate::l3::registration::SerialNumber::decode(message)
                    .map_err(Error::Serial)?;
                if decoded.has_extensions() {
                    let primary = &set.messages[0];
                    let version = match procedure {
                        Some(Procedure::Registration) => {
                            match crate::l3::registration::decode_prefix(primary)
                                .map_err(Error::Wire)?
                                .message
                            {
                                crate::l3::registration::AccessMessage::Registration {
                                    protocol_version,
                                    ..
                                } => Some(protocol_version),
                                _ => return Err(Error::Concatenation),
                            }
                        }
                        Some(Procedure::Origination) => Some(
                            crate::l3::origination::decode_prefix(primary)
                                .map_err(|_| Error::Concatenation)?
                                .message
                                .protocol_version,
                        ),
                        Some(Procedure::PageResponse) => Some(
                            crate::l3::page_response::decode_prefix(primary)
                                .map_err(|_| Error::Concatenation)?
                                .message
                                .protocol_version,
                        ),
                        _ => None,
                    };
                    if let Some(version) = version {
                        decoded.validate_version(version).map_err(Error::Serial)?;
                    }
                }
                None
            }
            _ => return Err(Error::Unsupported(header)),
        };
        if let Some(kind) = kind {
            if procedure.is_some_and(|previous| previous != kind) {
                return Err(Error::MixedProcedures);
            }
            // TIA-136-123-E §8 Table 15 permits one primary followed by secondary messages.
            if procedure.is_some() {
                return Err(Error::Concatenation);
            }
            procedure = Some(kind);
        }
    }
    procedure.ok_or(Error::MissingProcedure)
}
/// Input must be an assembled CRC-validated RACH message set.
pub fn dispatch(
    set: &MessageSet,
    registry: &mut LocalRegistry,
    calls: &mut CallPaging,
    superframe: u64,
    monotonic_ms: u64,
    setup_timeout_ms: u64,
) -> Result<Dispatch, Error> {
    let parsed = parse(set)?;
    let set = parsed.primary.as_ref();
    match parsed.procedure {
        Procedure::Registration => registry
            .register(set, superframe)
            .map(Dispatch::Registration)
            .map_err(Error::Registry),
        Procedure::Origination => calls
            .begin(registry, set, monotonic_ms, setup_timeout_ms)
            .map(Dispatch::Origination)
            .map_err(Error::Paging),
        Procedure::PageResponse => {
            let handle = {
                let mut requests = calls
                    .requests()
                    .filter(|request| request.recipient == set.identity);
                let handle = requests.next().ok_or(Error::MissingPage)?.handle.clone();
                if requests.next().is_some() {
                    return Err(Error::AmbiguousPage);
                }
                handle
            };
            calls
                .response(&handle, set, monotonic_ms)
                .map(Dispatch::PageResponse)
                .map_err(Error::Paging)
        }
        Procedure::Audit => crate::l3::audit::AuditConfirmation::decode(&set.messages[0])
            .map(Dispatch::Audit)
            .map_err(Error::Audit),
        Procedure::Capabilities => {
            let report = crate::l3::capability_report::decode(&set.messages[0])
                .map_err(Error::Capability)?;
            registry
                .record_capabilities(set.identity, report, superframe)
                .map_err(Error::Registry)?;
            Ok(Dispatch::Capabilities)
        }
        Procedure::Sms
        | Procedure::MessageWaiting
        | Procedure::UserAlert
        | Procedure::QueuedMaca => Err(Error::Unsupported(
            *set.messages[0].first().ok_or(Error::Empty)?,
        )),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn access_concatenation_accepts_one_primary_then_one_serial_number() {
        let serial = AccessMessage::SerialNumber { esn: 1 }.encode().unwrap();
        for (primary, expected) in [
            (vec![12], Procedure::Registration),
            (vec![7], Procedure::Origination),
            (vec![8], Procedure::PageResponse),
            (vec![9], Procedure::Sms),
            (vec![15, 0x68], Procedure::Sms),
        ] {
            let set = |messages| MessageSet {
                identity: MobileIdentity::Min34(1),
                messages,
            };
            assert_eq!(
                classify(&set(vec![primary.clone(), serial.clone()])).unwrap(),
                expected
            );
            for messages in [
                vec![serial.clone(), primary.clone()],
                vec![primary.clone(), primary.clone()],
                vec![primary.clone(), serial.clone(), serial.clone()],
            ] {
                assert!(matches!(
                    classify(&set(messages)),
                    Err(Error::Concatenation)
                ));
            }
        }
        assert!(matches!(
            classify(&MessageSet {
                identity: MobileIdentity::Min34(1),
                messages: vec![vec![15]]
            }),
            Err(Error::Confirmation(_))
        ));
        let mixed_sms = MessageSet {
            identity: MobileIdentity::Min34(1),
            messages: vec![vec![9], vec![10]],
        };
        assert!(matches!(classify(&mixed_sms), Err(Error::Concatenation)));
    }

    #[test]
    fn serial_companions_are_not_permitted_for_relay_replies_or_non_sms_confirmations() {
        let serial = AccessMessage::SerialNumber { esn: 1 }.encode().unwrap();
        // TIA-136-123-E §§5.4.15/8 restrict Serial Number to R-DATA notification confirmations.
        for primary in [vec![10, 1], vec![11, 1, 44], vec![15, 0x5c], vec![15, 0x94]] {
            let mut set = MessageSet {
                identity: MobileIdentity::Min34(1),
                messages: vec![primary],
            };
            assert!(classify(&set).is_ok());
            set.messages.push(serial.clone());
            assert!(matches!(classify(&set), Err(Error::Concatenation)));
        }
    }

    #[test]
    fn capability_reports_are_secondary_only_for_eligible_registrations() {
        use crate::l3::registration::{AccessMessage, RegistrationType};
        let report = vec![5, 0x57, 0x84, 0x1a, 0x10, 0x50, 0, 0xe0, 0];
        let serial = AccessMessage::SerialNumber { esn: 1 }.encode().unwrap();
        let set = |messages| MessageSet {
            identity: MobileIdentity::Min34(1),
            messages,
        };
        for kind in [
            RegistrationType::PowerUp,
            RegistrationType::NewSystem,
            RegistrationType::Forced,
        ] {
            let primary = AccessMessage::Registration {
                registration_type: kind,
                scm: 15,
                protocol_version: crate::l3::broadcast::ProtocolVersion::Is136RevA,
            }
            .encode()
            .unwrap();
            for messages in [
                vec![primary.clone(), report.clone()],
                vec![primary.clone(), serial.clone(), report.clone()],
                vec![primary.clone(), report.clone(), serial.clone()],
            ] {
                assert_eq!(classify(&set(messages)).unwrap(), Procedure::Registration);
            }
            for messages in [
                vec![report.clone(), primary.clone()],
                vec![primary.clone(), report.clone(), report.clone()],
            ] {
                assert!(matches!(
                    classify(&set(messages)),
                    Err(Error::Concatenation)
                ));
            }
            assert!(matches!(
                classify(&set(vec![primary, vec![5]])),
                Err(Error::Capability(_))
            ));
        }
        for primary in [
            vec![7],
            vec![8],
            vec![9],
            vec![15, 0x68],
            AccessMessage::Registration {
                registration_type: RegistrationType::Periodic,
                scm: 15,
                protocol_version: crate::l3::broadcast::ProtocolVersion::Is136RevA,
            }
            .encode()
            .unwrap(),
        ] {
            assert!(matches!(
                classify(&set(vec![primary, report.clone()])),
                Err(Error::Concatenation)
            ));
        }
    }
    use super::*;
    use crate::{
        core::registry::ProvisionedSubscriber,
        l3::{
            broadcast::ProtocolVersion,
            designation::AssignedSlots,
            registration::{AccessMessage, RegistrationType},
        },
        link::rach::MobileIdentity,
        phy::channel::Band,
        resources::TrafficResources,
    };
    #[test]
    fn mixed_or_unsupported_access_sets_do_not_mutate_registration_or_call_state() {
        let identity = MobileIdentity::Min34(1);
        let mut registry = LocalRegistry::try_new(vec![ProvisionedSubscriber {
            identity,
            number: "1001".into(),
        }])
        .unwrap();
        let mut calls = CallPaging::new(
            TrafficResources::try_new(Band::Cellular800, 1, &[AssignedSlots::Full14]).unwrap(),
        );
        let registration = AccessMessage::Registration {
            registration_type: RegistrationType::PowerUp,
            scm: 21,
            protocol_version: ProtocolVersion::Is136RevA,
        }
        .encode()
        .unwrap();
        for messages in [
            vec![registration.clone(), vec![ORIGINATION]],
            vec![registration.clone(), vec![255]],
            vec![registration.clone(), vec![]],
            vec![AccessMessage::SerialNumber { esn: 1 }.encode().unwrap()],
        ] {
            let set = MessageSet { identity, messages };
            assert!(dispatch(&set, &mut registry, &mut calls, 1, 1, 500).is_err());
            assert!(registry.by_number("1001").unwrap().registration.is_none());
            assert_eq!(calls.requests().count(), 0);
            assert_eq!(calls.reservations().count(), 0);
        }
    }
}
#[cfg(test)]
mod standalone_tests {
    use super::*;
    #[test]
    fn standalone_reports_are_a_distinct_procedure_and_cannot_append_secondary_messages() {
        let id = crate::link::rach::MobileIdentity::Min34(1);
        let report = vec![5, 0x57, 0x84, 0x1a, 0x10, 0x50, 0, 0xe0, 0];
        assert_eq!(
            classify(&MessageSet {
                identity: id,
                messages: vec![report.clone()]
            })
            .unwrap(),
            Procedure::Capabilities
        );
        let serial = crate::l3::registration::AccessMessage::SerialNumber { esn: 1 }
            .encode()
            .unwrap();
        for messages in [
            vec![report.clone(), serial],
            vec![report.clone(), report.clone()],
        ] {
            assert!(matches!(
                classify(&MessageSet {
                    identity: id,
                    messages
                }),
                Err(Error::Concatenation)
            ));
        }
        assert!(matches!(
            classify(&MessageSet {
                identity: id,
                messages: vec![vec![5]]
            }),
            Err(Error::Capability(_))
        ));
    }
}
#[cfg(test)]
mod serial_tests {
    use super::*;
    use crate::{
        l3::{
            address::{AddressInfo, AddressValue},
            broadcast::ProtocolVersion,
            origination::Origination,
            page_response::{PageResponse, ServiceCode},
            registration::{AccessMessage, RegistrationType, SerialNumber},
        },
        link::rach::MobileIdentity,
    };
    #[test]
    fn serial_extensions_follow_each_version_bearing_primary_and_cannot_be_primary() {
        let serial = SerialNumber {
            esn: 42,
            esnx: Some(0),
            s_esn: Some(0),
            ignored_bits: Vec::new(),
            unparsed_bits: vec![],
        }
        .encode()
        .unwrap();
        for version in [ProtocolVersion::Pv1, ProtocolVersion::Pv5] {
            for (primary, expected) in [
                (
                    AccessMessage::Registration {
                        registration_type: RegistrationType::PowerUp,
                        scm: 15,
                        protocol_version: version,
                    }
                    .encode()
                    .unwrap(),
                    Procedure::Registration,
                ),
                (
                    Origination {
                        protocol_version: version,
                        emergency_call: false,
                        called_party: AddressInfo {
                            number_type: 0,
                            numbering_plan: 1,
                            address: AddressValue::Tbcd("1002".into()),
                        },
                        last_try: false,
                        scm: 15,
                        service_code: ServiceCode::DigitalSpeech,
                    }
                    .encode()
                    .unwrap(),
                    Procedure::Origination,
                ),
                (
                    PageResponse {
                        protocol_version: version,
                        last_try: false,
                        scm: 15,
                        service_code: ServiceCode::DigitalSpeech,
                    }
                    .encode()
                    .unwrap(),
                    Procedure::PageResponse,
                ),
            ] {
                let set = MessageSet {
                    identity: MobileIdentity::Min34(1),
                    messages: vec![primary, serial.clone()],
                };
                if version == ProtocolVersion::Pv5 {
                    assert_eq!(classify(&set).unwrap(), expected);
                } else {
                    assert!(matches!(classify(&set), Err(Error::Serial(_))));
                }
            }
        }
        let mut set = MessageSet {
            identity: MobileIdentity::Min34(1),
            messages: vec![serial.clone()],
        };
        assert!(matches!(classify(&set), Err(Error::Concatenation)));
        set.messages = vec![vec![12], serial];
        assert!(classify(&set).is_err());
    }
}
